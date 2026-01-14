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

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,

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

    pub worker_max_retries: i32,
    pub worker_max_delay_seconds: i64,
    pub worker_api_timeout_seconds: u64,
    pub worker_interval_seconds: u64,
    pub worker_items_per_iteration: u64,
}

const WORKER_MAX_RETRIES_DEFAULT: i32 = 50;
const WORKER_MAX_DELAY_SECONDS_DEFAULT: i64 = 60 * 30;
const WORKER_API_TIMEOUT_SECONDS_DEFAULT: u64 = 60;
const WORKER_INTERVAL_SECONDS_DEFAULT: u64 = 5;
const WORKER_ITEMS_PER_ITERATION_DEFAULT: u64 = 50;

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    let mut config = Config::builder()
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
        .add_source(config::File::with_name("/etc/email-hook/settings.toml").required(false));
    if let Some(config_path) = cli.config {
        config = config.add_source(config::File::from(config_path));
    }
    let config = config
        .add_source(config::Environment::with_prefix("EMAIL_HOOK"))
        .build()
        .with_context(|| "failed to read application settings")?
        .try_deserialize::<AppConfig>()
        .with_context(|| "failed to parse application settings")?;

    let db = connect_database(&config).await?;

    match cli.command {
        Commands::Init => {
            initialize_database(db).await?;
        }
        Commands::Ingest => {
            execute_ingest(db).await?;
        }
        Commands::Worker => {
            execute_worker(config, db).await?;
        }
    }

    Ok(ExitCode::SUCCESS)
}
