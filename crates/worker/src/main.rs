//! Raspberry Pi worker.
//!
//! `worker serve`  long-running (systemd service): polls `refresh_request`
//!                 every 60s, so the site's "Refresh now" button (which only
//!                 inserts a row) is picked up within a minute.
//! `worker run`    one refresh now (systemd timer, Mon + Fri 01:00).
//!
//! A Postgres advisory lock guarantees only one refresh runs at a time, across
//! both modes. Connects as the `worker` role.

mod fdo;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

const USAGE: &str = "usage: worker serve | worker run";
/// Advisory lock key shared by every refresh ("foot" in ASCII).
const LOCK_KEY: i64 = 0x666f_6f74;
const POLL_EVERY: Duration = Duration::from_secs(60);

pub struct Config {
    db_url: String,
    fdo_key: String,
    raw_dir: PathBuf,
}

impl Config {
    fn from_env() -> Result<Self> {
        let db_url = std::env::var("WORKER_DATABASE_URL")
            .ok()
            .filter(|u| u.starts_with("postgres://") || u.starts_with("postgresql://"))
            .context("WORKER_DATABASE_URL must be a postgresql:// connection string")?;
        let fdo_key = std::env::var("FOOTBALL_DATA_ORG_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .context("FOOTBALL_DATA_ORG_KEY is not set")?;
        Ok(Self {
            db_url,
            fdo_key,
            raw_dir: std::env::var("RAW_DIR").map_or_else(|_| PathBuf::from("raw"), PathBuf::from),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    /// Timer: always runs (and also serves any pending requests).
    Schedule,
    /// Queue poll: runs only when a request is pending.
    Queue,
}

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    match run_cli().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_cli() -> Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    if cmd != "serve" && cmd != "run" {
        bail!(USAGE);
    }
    let cfg = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&cfg.db_url)
        .await?;
    if cmd == "run" {
        cycle(&pool, &cfg, Trigger::Schedule).await
    } else {
        serve(&pool, &cfg).await
    }
}

/// One refresh, if the lock is free. Returns Ok when another run holds the lock.
async fn cycle(pool: &PgPool, cfg: &Config, trigger: Trigger) -> Result<()> {
    // The lock lives on this connection; it is released on unlock or if the
    // process dies (the connection closes), so a crash never leaves it stuck.
    let mut lock_conn = pool.acquire().await?;
    let locked: bool = sqlx::query_scalar("select pg_try_advisory_lock($1)")
        .bind(LOCK_KEY)
        .fetch_one(&mut *lock_conn)
        .await?;
    if !locked {
        tracing::info!(?trigger, "another refresh is running, skipped");
        return Ok(());
    }
    let result = locked_cycle(pool, cfg, trigger).await;
    sqlx::query("select pg_advisory_unlock($1)")
        .bind(LOCK_KEY)
        .execute(&mut *lock_conn)
        .await?;
    result
}

async fn locked_cycle(pool: &PgPool, cfg: &Config, trigger: Trigger) -> Result<()> {
    // We hold the lock, so any 'running' row is left over from a crashed run.
    sqlx::query(
        "update refresh_request set status = 'failed', finished_at = now(),
                message = 'worker stopped mid-run'
         where status = 'running'",
    )
    .execute(pool)
    .await?;
    // Claim every pending request: one run serves them all.
    let claimed: Vec<i64> = sqlx::query_scalar(
        "update refresh_request set status = 'running', started_at = now()
         where status = 'pending' returning id",
    )
    .fetch_all(pool)
    .await?;
    if claimed.is_empty() && trigger == Trigger::Queue {
        return Ok(());
    }

    tracing::info!(?trigger, requests = claimed.len(), "refresh started");
    let result = fdo::refresh_squads(pool, cfg).await;
    let (status, message) = match &result {
        Ok(summary) => ("done", summary.clone()),
        Err(e) => ("failed", format!("{e:#}")),
    };
    sqlx::query(
        "update refresh_request set status = $1, finished_at = now(), message = $2
         where id = any($3)",
    )
    .bind(status)
    .bind(&message)
    .bind(&claimed)
    .execute(pool)
    .await?;
    tracing::info!(status, %message, "refresh finished");
    result.map(|_| ())
}

// --------------------------------------------------------------- serve mode

/// Poll the queue until stopped (systemd sends SIGINT: KillSignal=SIGINT).
async fn serve(pool: &PgPool, cfg: &Config) -> Result<()> {
    tracing::info!("polling refresh_request every {}s", POLL_EVERY.as_secs());
    // Created once so a stop signal that arrives mid-refresh is not lost; the
    // running refresh finishes before we exit.
    let stop = tokio::signal::ctrl_c();
    tokio::pin!(stop);
    loop {
        if let Err(e) = cycle(pool, cfg, Trigger::Queue).await {
            tracing::error!("refresh failed: {e:#}");
        }
        tokio::select! {
            _ = tokio::time::sleep(POLL_EVERY) => {}
            _ = &mut stop => {
                tracing::info!("stopping");
                return Ok(());
            }
        }
    }
}
