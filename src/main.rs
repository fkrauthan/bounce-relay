mod db;
mod worker;
mod ingest;

use std::path::PathBuf;
use std::process::ExitCode;
use clap::{Parser, Subcommand};
use config::Config;
use anyhow::{Context, Result};
use serde::Deserialize;
use crate::db::{connect_database, initialize_database};
use crate::ingest::execute_ingest;
use crate::worker::execute_worker;

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
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let cli = Cli::parse();

    let mut config = Config::builder()
        .add_source(config::File::with_name("./settings.toml").required(false))
        .add_source(config::File::with_name("/etc/email-hook/settings.toml").required(false));
    if let Some(config_path) = cli.config {
        config = config
            .add_source(config::File::from(config_path));
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
            execute_worker(db).await?;
        }
    }

    Ok(ExitCode::SUCCESS)
}
