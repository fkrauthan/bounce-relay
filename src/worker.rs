use crate::AppConfig;
use crate::db::{DBConnection, EmailRoute, WebhookQueue};
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use hmac::{Hmac, Mac};
use reqwest::Client;
use sea_query::{Expr, Order, Query};
use sea_query_binder::SqlxBinder;
use sha2::Sha512;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::{Duration as TimeDuration, OffsetDateTime};
use tokio::signal;
use tracing::{debug, error, info, warn};

type HmacSha512 = Hmac<Sha512>;

#[derive(Debug, sqlx::FromRow)]
struct JobToExecute {
    id: i32,
    url: String,
    secret_token: String,
    payload: String,
    attempts: i32,
}

pub async fn execute_worker(config: AppConfig, mut db: DBConnection) -> Result<()> {
    let client = Client::builder()
        .timeout(Duration::from_secs(config.worker_api_timeout_seconds))
        .user_agent(format!(
            "{}/{}",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        ))
        .build()?;

    info!(
        interval_seconds = config.worker_interval_seconds,
        items_per_iteration = config.worker_items_per_iteration,
        "Worker started"
    );

    let mut interval = tokio::time::interval(Duration::from_secs(config.worker_interval_seconds));
    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = signal::ctrl_c() => {
                info!("Worker shutting down");
                break;
            }
        }

        let jobs = find_jobs(&mut db, config.worker_items_per_iteration).await?;
        if !jobs.is_empty() {
            debug!(count = jobs.len(), "Found jobs to process");
        }
        for job in jobs {
            match process_job(&client, &job).await {
                Ok(_) => {
                    info!(id = job.id, url = job.url.as_str(), "Delivered webhook");
                    delete_job(&mut db, job).await?;
                }
                Err(e) => {
                    reschedule_job(
                        config.worker_max_retries,
                        config.worker_max_delay_seconds,
                        &mut db,
                        job,
                        &format!("{:#}", e),
                    )
                    .await?
                }
            }
        }
    }

    Ok(())
}

async fn process_job(client: &Client, job: &JobToExecute) -> Result<()> {
    // Create signature
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .to_string();

    let mut mac = HmacSha512::new_from_slice(job.secret_token.as_bytes())?;
    mac.update(format!("{}.{}", timestamp, job.payload).as_bytes());
    let signature = BASE64_STANDARD.encode(mac.finalize().into_bytes());

    let response = client
        .post(&job.url)
        .header("Content-Type", "application/json")
        .header("X-Timestamp", &timestamp)
        .header("X-Signature", &signature)
        .body(job.payload.to_owned())
        .send()
        .await
        .with_context(|| format!("Failed to call webhook url {}", job.url))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.with_context(|| {
            format!(
                "Failed to get response body for {} and status {}",
                job.url, status
            )
        })?;

        bail!("{}: {}", status, body);
    }

    Ok(())
}

async fn delete_job(db: &mut DBConnection, job: JobToExecute) -> Result<()> {
    let query_builder = &*db.query_builder;
    let (sql, values) = Query::delete()
        .from_table(WebhookQueue::Table)
        .and_where(Expr::col(WebhookQueue::Id).eq(job.id))
        .build_any_sqlx(query_builder);

    sqlx::query_with(&sql, values)
        .execute(&mut db.connection)
        .await?;
    Ok(())
}

async fn reschedule_job(
    max_retries: i32,
    max_delay: i64,
    db: &mut DBConnection,
    job: JobToExecute,
    error: &str,
) -> Result<()> {
    let attempts = job.attempts + 1;
    let is_expired = max_retries > 0 && attempts >= max_retries;
    let minutes_to_wait = 2_i64.pow(attempts as u32).min(max_delay);
    let next_try_at = OffsetDateTime::now_utc() + TimeDuration::minutes(minutes_to_wait);

    if is_expired {
        error!(
            id = job.id,
            attempts = attempts,
            error = error,
            "Webhook expired after max retries"
        );
    } else {
        warn!(
            id = job.id,
            attempt = attempts,
            retry_in_minutes = minutes_to_wait,
            error = error,
            "Webhook failed, scheduling retry"
        );
    }

    let query_builder = &*db.query_builder;
    let (sql, values) = Query::update()
        .table(WebhookQueue::Table)
        .values([
            (WebhookQueue::Attempts, attempts.into()),
            (WebhookQueue::LastError, error.into()),
            (WebhookQueue::IsExpired, is_expired.into()),
            (WebhookQueue::NextRetryAt, next_try_at.into()),
        ])
        .and_where(Expr::col(WebhookQueue::Id).eq(job.id))
        .build_any_sqlx(query_builder);

    sqlx::query_with(&sql, values)
        .execute(&mut db.connection)
        .await?;
    Ok(())
}

async fn find_jobs(db: &mut DBConnection, max_jobs: u64) -> Result<Vec<JobToExecute>> {
    let query_builder = &*db.query_builder;
    let (sql, values) = Query::select()
        .column((WebhookQueue::Table, WebhookQueue::Id))
        .columns([
            WebhookQueue::Payload,
            WebhookQueue::Attempts,
            WebhookQueue::NextRetryAt,
        ])
        .column((EmailRoute::Table, EmailRoute::Url))
        .column((EmailRoute::Table, EmailRoute::SecretToken))
        .from(WebhookQueue::Table)
        .left_join(
            EmailRoute::Table,
            Expr::col((WebhookQueue::Table, WebhookQueue::EmailRouteId))
                .equals((EmailRoute::Table, EmailRoute::Id)),
        )
        .and_where(Expr::col(WebhookQueue::NextRetryAt).lte(Expr::current_timestamp()))
        .and_where(Expr::col(WebhookQueue::IsExpired).eq(false))
        .and_where(Expr::col((EmailRoute::Table, EmailRoute::IsActive)).eq(true))
        .order_by(WebhookQueue::NextRetryAt, Order::Asc)
        .limit(max_jobs)
        .build_any_sqlx(query_builder);

    sqlx::query_as_with(&sql, values)
        .fetch_all(&mut db.connection)
        .await
        .with_context(|| "Failed to load queue entries")
}
