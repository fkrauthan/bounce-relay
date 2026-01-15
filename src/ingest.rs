use crate::AppConfig;
use crate::db::{DBConnection, EmailRoute, WebhookQueue};
use anyhow::{Context, Result};
use mail_parser::{Message, MessageParser, MimeHeaders, PartType};
use sea_query::{Expr, Iden, Query};
use sea_query_binder::SqlxBinder;
use sqlx::Row;
use std::collections::HashMap;
use time::UtcDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::io;
use tokio::io::AsyncReadExt;
use tracing::{debug, info, warn};

#[derive(Debug, Default)]
struct BounceInfo {
    recipient: String,
    reason: String,
    status: String,
    action: String,
}

#[derive(Debug, Default)]
struct MessageInfo {
    from: String,
    subject: String,
    message_id: Option<String>,
    metadata: HashMap<String, String>,
}

pub async fn execute_ingest(config: AppConfig, mut db: DBConnection) -> Result<()> {
    // Parse message
    let mut buffer = Vec::new();
    io::stdin()
        .read_to_end(&mut buffer)
        .await
        .with_context(|| "Failed to read stdin")?;
    debug!(bytes = buffer.len(), "Read email from stdin");

    let message = MessageParser::default()
        .parse(&buffer)
        .with_context(|| "Failed to parse email")?;

    let target_address = message
        .to()
        .and_then(|a| a.first())
        .and_then(|a| a.address.clone())
        .map(|a| a.to_string())
        .unwrap_or("unknown".to_string());

    let (user, domain) = target_address.split_once('@').unwrap_or(("", ""));
    let user = user
        .split_once(config.recipient_delimiter)
        .map(|(user, _)| user)
        .unwrap_or(user);

    info!(domain = domain, user = user, "Processing email");

    // Validate that this is a bounce email (has DSN delivery-status part)
    let Some(bounce_info) = parse_dsn(&message) else {
        warn!("Email is not a bounce notification, ignoring");
        return Ok(());
    };
    debug!("Validated email as bounce notification");

    // Find valid webhook destinations (both specific user routes and catch-all domain routes)
    let query_builder = &*db.query_builder;
    let (sql, values) = Query::select()
        .columns([EmailRoute::Id])
        .from(EmailRoute::Table)
        .and_where(Expr::col(EmailRoute::Domain).eq(domain))
        .and_where(
            Expr::col(EmailRoute::User)
                .eq(user)
                .or(Expr::col(EmailRoute::User).is_null()),
        )
        .and_where(Expr::col(EmailRoute::IsEnabled).eq(true))
        .build_any_sqlx(query_builder);
    let routes = sqlx::query_with(&sql, values)
        .fetch_all(&mut db.connection)
        .await
        .with_context(|| "Failed to load applicable routes")?;
    if routes.is_empty() {
        warn!(domain = domain, user = user, "No active routes found");
        return Ok(());
    }
    debug!(count = routes.len(), "Found matching routes");

    // Extract relevant webhook information
    let original_info = parse_original_message(&message);
    let payload = serde_json::json!({
        "event": "bounce",
        "timestamp": UtcDateTime::now().format(&Rfc3339)?,
        "message_id": original_info.message_id,
        "from": original_info.from,
        "subject": original_info.subject,
        "metadata": original_info.metadata,
        "email": bounce_info.recipient,
        "reason": bounce_info.reason,
        "status": bounce_info.status,
        "action": bounce_info.action,
        "is_permanent": bounce_info.status.starts_with("5"),
    })
    .to_string();

    // Insert into webhook queue for delivery
    for route in routes {
        let route_id: i32 = route
            .try_get(EmailRoute::Id.to_string().as_str())
            .with_context(|| "Could not read route id")?;

        let (sql, values) = Query::insert()
            .into_table(WebhookQueue::Table)
            .columns([WebhookQueue::EmailRouteId, WebhookQueue::Payload])
            .values_panic([route_id.into(), payload.clone().into()])
            .build_any_sqlx(query_builder);
        sqlx::query_with(&sql, values)
            .execute(&mut db.connection)
            .await
            .with_context(|| format!("Failed to insert payload for route {}", route_id))?;
        info!(route_id = route_id, "Queued webhook");
    }

    Ok(())
}

fn parse_original_message(email: &Message) -> MessageInfo {
    let mut info = MessageInfo {
        from: "unknown".to_string(),
        subject: "unknown".to_string(),
        message_id: None,
        metadata: HashMap::new(),
    };

    let mut parse_message = |message: &Message| {
        info.subject = message.subject().unwrap_or("unknown").to_string();
        info.from = message
            .from()
            .and_then(|f| f.first())
            .and_then(|a| a.address.clone())
            .map(|a| a.to_string())
            .unwrap_or("unknown".to_string());
        info.message_id = message
            .message_id()
            .or(email.message_id())
            .map(|id| id.to_string());

        for header in message.headers() {
            let header_name = header.name.as_str();
            if header_name.starts_with("X-") || header_name.starts_with("x-") {
                info.metadata.insert(
                    header_name[2..].to_string(),
                    header.value.as_text().unwrap_or("").to_string(),
                );
            }
        }
    };

    for part in &email.parts {
        if let PartType::Message(message) = &part.body {
            parse_message(message);
            break;
        }
        match part.content_type() {
            Some(ct) if ct.c_type == "text" && ct.subtype().unwrap_or("") == "rfc822-headers" => {
                if let Some(message) = MessageParser::default().parse_headers(&part.contents()) {
                    parse_message(&message);
                    break;
                }
            }
            _ => {}
        }
    }

    info
}

fn parse_dsn(email: &Message) -> Option<BounceInfo> {
    for part in &email.parts {
        match part.content_type() {
            Some(ct)
                if ct.c_type == "message" && ct.subtype().unwrap_or("") == "delivery-status" =>
            {
                let mut info = BounceInfo {
                    recipient: "unknown".to_string(),
                    reason: "No reason found".to_string(),
                    status: "5.0.0".to_string(),
                    action: "failed".to_string(),
                };

                let text = part.text_contents().unwrap_or("");
                for line in text.lines() {
                    let lower = line.to_lowercase();
                    if lower.starts_with("original-recipient:")
                        || (lower.starts_with("final-recipient:") && info.recipient.eq("unknown"))
                    {
                        info.recipient =
                            line.split(';').next_back().unwrap_or("").trim().to_string();
                    } else if lower.starts_with("diagnostic-code:") {
                        info.reason = line.splitn(2, ':').last().unwrap_or("").trim().to_string();
                    } else if lower.starts_with("status:") {
                        info.status = line.split(':').next_back().unwrap_or("").trim().to_string();
                    } else if lower.starts_with("action:") {
                        info.action = line.split(':').next_back().unwrap_or("").trim().to_string();
                    }
                }

                return Some(info);
            }
            _ => continue,
        }
    }

    None
}
