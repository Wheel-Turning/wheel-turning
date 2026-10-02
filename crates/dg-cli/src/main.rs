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
    /// Create or update a login role that inherits one group role. Safe to run repeatedly.
    CreateLogin {
        /// Group role to inherit: dg_owner, dg_pipeline, dg_web or dg_analyst.
        #[arg(long)]
        role: String,
        /// Login role name. Defaults to <role>_login.
        #[arg(long)]
        login: Option<String>,
        /// Environment variable holding the password, for example one ECS injects from Secrets
        /// Manager. The password is never accepted as an argument.
        #[arg(long)]
        password_env: String,
    },
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
        Command::Db {
            command:
                DbCommand::CreateLogin {
                    role,
                    login,
                    password_env,
                },
        } => {
            let login = login.unwrap_or_else(|| format!("{role}_login"));
            let password = std::env::var(&password_env)
                .ok()
                .filter(|p| !p.is_empty())
                .map(dg_core::Secret::new)
                .with_context(|| format!("{password_env} is not set"))?;
            let pool = dg_db::connect(&config.database_url)
                .await
                .context("connecting to the database")?;
            let outcome = dg_db::create_login(&pool, &role, &login, &password)
                .await
                .with_context(|| format!("creating login {login}"))?;
            tracing::info!(login = %login, role = %role, outcome = ?outcome, "login ready");
        }
    }
    Ok(())
}
