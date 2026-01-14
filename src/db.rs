use crate::AppConfig;
use anyhow::{Context, Result, bail};
use sea_query::{
    ColumnDef, Expr, ForeignKey, Iden, Index, MysqlQueryBuilder, PostgresQueryBuilder,
    QueryBuilder, SchemaBuilder, SqliteQueryBuilder, Table,
};
use sqlx::AnyConnection;
use sqlx::Connection;
use sqlx::any::install_default_drivers;
use tracing::{debug, info};

pub struct DBConnection {
    pub connection: AnyConnection,
    pub query_builder: Box<dyn QueryBuilder>,
    pub schema_builder: Box<dyn SchemaBuilder>,
}

pub enum EmailRoute {
    Table,
    Id,
    Domain,
    User,
    Url,
    SecretToken,
    IsActive,
}
impl Iden for EmailRoute {
    fn unquoted(&self, s: &mut dyn std::fmt::Write) {
        write!(
            s,
            "{}",
            match self {
                Self::Table => "email_routes",
                Self::Id => "id",
                Self::Domain => "domain",
                Self::User => "user",
                Self::Url => "url",
                Self::SecretToken => "secret_token",
                Self::IsActive => "is_active",
            }
        )
        .unwrap();
    }
}

pub enum WebhookQueue {
    Table,
    Id,
    EmailRouteId,
    Payload,
    Attempts,
    NextRetryAt,
    LastError,
    IsExpired,
    CreatedAt,
}
impl Iden for WebhookQueue {
    fn unquoted(&self, s: &mut dyn std::fmt::Write) {
        write!(
            s,
            "{}",
            match self {
                Self::Table => "webhook_queue",
                Self::Id => "id",
                Self::EmailRouteId => "email_route_id",
                Self::Payload => "payload",
                Self::Attempts => "attempts",
                Self::NextRetryAt => "next_retry_at",
                Self::LastError => "last_error",
                Self::IsExpired => "is_expired",
                Self::CreatedAt => "created_at",
            }
        )
        .unwrap();
    }
}

pub async fn connect_database(config: &AppConfig) -> Result<DBConnection> {
    install_default_drivers();

    let connection = AnyConnection::connect(&config.database_url)
        .await
        .with_context(|| "Failed to connect to database")?;

    let backend = connection.backend_name();
    info!(backend = backend, "Connected to database");

    Ok(match backend {
        "PostgreSQL" => DBConnection {
            connection,
            query_builder: Box::new(PostgresQueryBuilder {}),
            schema_builder: Box::new(PostgresQueryBuilder {}),
        },
        "MySQL" => DBConnection {
            connection,
            query_builder: Box::new(MysqlQueryBuilder {}),
            schema_builder: Box::new(MysqlQueryBuilder {}),
        },
        "SQLite" => DBConnection {
            connection,
            query_builder: Box::new(SqliteQueryBuilder {}),
            schema_builder: Box::new(SqliteQueryBuilder {}),
        },
        _ => bail!("Unknown backend name: {}", backend),
    })
}

pub async fn initialize_database(mut db: DBConnection) -> Result<()> {
    let schema_builder = &*db.schema_builder;

    info!("Creating email_routes table");
    let email_routes = Table::create()
        .table(EmailRoute::Table)
        .if_not_exists()
        .col(
            ColumnDef::new(EmailRoute::Id)
                .integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(ColumnDef::new(EmailRoute::Domain).string().not_null())
        .col(ColumnDef::new(EmailRoute::User).string().null())
        .col(ColumnDef::new(EmailRoute::Url).string().not_null())
        .col(ColumnDef::new(EmailRoute::SecretToken).string().not_null())
        .col(
            ColumnDef::new(EmailRoute::IsActive)
                .boolean()
                .not_null()
                .default(true),
        )
        .build_any(schema_builder);
    sqlx::query(&email_routes)
        .execute(&mut db.connection)
        .await?;

    debug!("Creating index idx_route_lookup");
    let email_routes_index = Index::create()
        .name("idx_route_lookup")
        .if_not_exists()
        .table(EmailRoute::Table)
        .col(EmailRoute::Domain)
        .col(EmailRoute::User)
        .col(EmailRoute::IsActive)
        .build_any(schema_builder);
    sqlx::query(&email_routes_index)
        .execute(&mut db.connection)
        .await?;

    debug!("Creating index idx_route_enabled_lookup");
    let email_routes_enabled_index = Index::create()
        .name("idx_route_enabled_lookup")
        .if_not_exists()
        .table(EmailRoute::Table)
        .col(EmailRoute::IsActive)
        .build_any(schema_builder);
    sqlx::query(&email_routes_enabled_index)
        .execute(&mut db.connection)
        .await?;

    info!("Creating webhook_queue table");
    let webhook_queue = Table::create()
        .table(WebhookQueue::Table)
        .if_not_exists()
        .col(
            ColumnDef::new(WebhookQueue::Id)
                .integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(WebhookQueue::EmailRouteId)
                .integer()
                .not_null(),
        )
        .col(ColumnDef::new(WebhookQueue::Payload).text().not_null())
        .col(
            ColumnDef::new(WebhookQueue::Attempts)
                .unsigned()
                .integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(WebhookQueue::NextRetryAt)
                .timestamp_with_time_zone()
                .not_null()
                .default(Expr::current_timestamp()),
        )
        .col(ColumnDef::new(WebhookQueue::LastError).text().null())
        .col(
            ColumnDef::new(WebhookQueue::IsExpired)
                .boolean()
                .not_null()
                .default(false),
        )
        .col(
            ColumnDef::new(WebhookQueue::CreatedAt)
                .timestamp_with_time_zone()
                .not_null()
                .default(Expr::current_timestamp()),
        )
        .foreign_key(
            ForeignKey::create()
                .name("fk_queue_to_route")
                .from(WebhookQueue::Table, WebhookQueue::EmailRouteId)
                .to(EmailRoute::Table, EmailRoute::Id),
        )
        .build_any(schema_builder);
    sqlx::query(&webhook_queue)
        .execute(&mut db.connection)
        .await?;

    debug!("Creating index idx_queue_processing");
    let webhooks_queue_index = Index::create()
        .name("idx_queue_processing")
        .if_not_exists()
        .table(WebhookQueue::Table)
        .col(WebhookQueue::NextRetryAt)
        .col(WebhookQueue::IsExpired)
        .build_any(schema_builder);
    sqlx::query(&webhooks_queue_index)
        .execute(&mut db.connection)
        .await?;

    Ok(())
}
