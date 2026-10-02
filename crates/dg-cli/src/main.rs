//! The dg binary: commands and the worker.

use anyhow::Context;
use clap::{Parser, Subcommand};
use dg_core::Config;
use dg_core::telemetry::init_logging;

#[derive(Debug, Parser)]
#[command(name = "dg", version, about = "Dharma Gates operations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Database operations.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DbCommand {
    /// Apply every pending migration. Safe to run repeatedly.
    Migrate,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = Config::from_env().context("loading configuration")?;
    init_logging(config.env, &config.log)?;

    match cli.command {
        Command::Db {
            command: DbCommand::Migrate,
        } => {
            let pool = dg_db::connect(&config.database_url)
                .await
                .context("connecting to the database")?;
            let report = dg_db::migrate(&pool).await.context("applying migrations")?;
            tracing::info!(
                applied = report.applied,
                known = report.known,
                "migrations up to date"
            );
        }
    }
    Ok(())
}
