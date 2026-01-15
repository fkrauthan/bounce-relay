mod db;
mod ingest;
mod worker;

use crate::db::{connect_database, initialize_database};
use crate::ingest::execute_ingest;
use crate::worker::execute_worker;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use config::Config;
use serde::Deserialize;
use std::path::PathBuf;
use std::process::ExitCode;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Sets the log level (trace, debug, info, warn, error)
    #[arg(short, long, value_name = "LEVEL")]
    log_level: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize the database schema
    Init,
    /// Process incoming email from Postfix
    Ingest,
    /// Run the background worker
    Worker,
}

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub database_url: String,

    pub log_level: String,
    pub recipient_delimiter: char,

    pub worker_max_retries: i32,
    pub worker_max_delay_seconds: i64,
    pub worker_api_timeout_seconds: u64,
    pub worker_interval_seconds: u64,
    pub worker_items_per_iteration: u64,
}

const LOG_LEVEL_DEFAULT: &str = "info";
const RECIPIENT_DELIMITER_DEFAULT: char = '+';
const WORKER_MAX_RETRIES_DEFAULT: i32 = 50;
const WORKER_MAX_DELAY_SECONDS_DEFAULT: i64 = 60 * 30;
const WORKER_API_TIMEOUT_SECONDS_DEFAULT: u64 = 60;
const WORKER_INTERVAL_SECONDS_DEFAULT: u64 = 5;
const WORKER_ITEMS_PER_ITERATION_DEFAULT: u64 = 50;

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    let mut config = Config::builder()
        .set_default("log_level", LOG_LEVEL_DEFAULT)?
        .set_default(
            "recipient_delimiter",
            RECIPIENT_DELIMITER_DEFAULT.to_string(),
        )?
        .set_default("worker_max_retries", WORKER_MAX_RETRIES_DEFAULT)?
        .set_default("worker_max_delay_seconds", WORKER_MAX_DELAY_SECONDS_DEFAULT)?
        .set_default(
            "worker_api_timeout_seconds",
            WORKER_API_TIMEOUT_SECONDS_DEFAULT,
        )?
        .set_default("worker_interval_seconds", WORKER_INTERVAL_SECONDS_DEFAULT)?
        .set_default(
            "worker_items_per_iteration",
            WORKER_ITEMS_PER_ITERATION_DEFAULT,
        )?
        .add_source(config::File::with_name("./settings.toml").required(false))
        .add_source(config::File::with_name("/etc/bounce-relay/settings.toml").required(false));
    if let Some(ref config_path) = cli.config {
        config = config.add_source(config::File::from(config_path.clone()));
    }
    let config = config
        .add_source(config::Environment::with_prefix("BOUNCE_RELAY"))
        .build()
        .with_context(|| "failed to read application settings")?
        .try_deserialize::<AppConfig>()
        .with_context(|| "failed to parse application settings")?;

    // Resolve log level: CLI takes precedence over config
    let log_level = cli.log_level.as_deref().unwrap_or(&config.log_level);

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env()
                .add_directive(log_level.parse().unwrap_or(tracing::Level::INFO.into())),
        )
        .init();

    let db = connect_database(&config).await?;

    match cli.command {
        Commands::Init => {
            debug!("Executing init subcommand");
            initialize_database(db).await?;
            info!("Database schema initialized successfully");
        }
        Commands::Ingest => {
            debug!("Executing ingest subcommand");
            execute_ingest(config, db).await?;
        }
        Commands::Worker => {
            debug!("Executing worker subcommand");
            execute_worker(config, db).await?;
        }
    }

    Ok(ExitCode::SUCCESS)
}
