//! Raspberry Pi worker.
//!
//! `worker serve`  long-running (systemd service): polls `refresh_request`
//!                 every 60s and serves the admin API for the site's
//!                 "Refresh now" button (reached via Cloudflare Tunnel).
//! `worker run`    one refresh now (systemd timer, Mon + Fri 01:00).
//!
//! A Postgres advisory lock guarantees only one refresh runs at a time, across
//! both modes. Connects as the `worker` role.

mod fdo;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tokio::sync::Notify;

const USAGE: &str = "usage: worker serve | worker run";
/// Advisory lock key shared by every refresh ("foot" in ASCII).
const LOCK_KEY: i64 = 0x666f_6f74;
const POLL_EVERY: Duration = Duration::from_secs(60);

pub struct Config {
    db_url: String,
    fdo_key: String,
    raw_dir: PathBuf,
    admin_bind: String,
    admin_token: Option<String>,
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
            admin_bind: std::env::var("ADMIN_BIND").unwrap_or_else(|_| "127.0.0.1:8787".into()),
            admin_token: std::env::var("WORKER_ADMIN_TOKEN").ok(),
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
    let cfg = Arc::new(Config::from_env()?);
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .connect(&cfg.db_url)
        .await?;
    if cmd == "run" {
        cycle(&pool, &cfg, Trigger::Schedule).await
    } else {
        serve(pool, cfg).await
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

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    token: Arc<str>,
    wake: Arc<Notify>,
}

async fn serve(pool: PgPool, cfg: Arc<Config>) -> Result<()> {
    let token =
        cfg.admin_token.clone().filter(|t| t.len() >= 32).context(
            "WORKER_ADMIN_TOKEN must be set to at least 32 characters for `worker serve`",
        )?;
    let wake = Arc::new(Notify::new());

    let poller = {
        let (pool, cfg, wake) = (pool.clone(), cfg.clone(), wake.clone());
        tokio::spawn(async move {
            loop {
                if let Err(e) = cycle(&pool, &cfg, Trigger::Queue).await {
                    tracing::error!("refresh failed: {e:#}");
                }
                tokio::select! {
                    _ = tokio::time::sleep(POLL_EVERY) => {}
                    _ = wake.notified() => {}
                }
            }
        })
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/refresh", post(refresh))
        .with_state(AppState {
            pool,
            token: token.into(),
            wake,
        });
    let listener = tokio::net::TcpListener::bind(&cfg.admin_bind).await?;
    tracing::info!("admin API on http://{}", cfg.admin_bind);
    // systemd stops the service with SIGINT (KillSignal=SIGINT in the unit).
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    poller.abort();
    Ok(())
}

async fn health(State(st): State<AppState>) -> impl IntoResponse {
    let last: Option<(String, Option<String>)> = sqlx::query_as(
        "select status, to_char(finished_at at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')
         from ingest_run order by id desc limit 1",
    )
    .fetch_optional(&st.pool)
    .await
    .unwrap_or(None);
    Json(serde_json::json!({
        "ok": true,
        "last_ingest_status": last.as_ref().map(|l| l.0.clone()),
        "last_ingest_finished_at": last.and_then(|l| l.1),
    }))
}

/// Queue a refresh (if none is pending) and start it now instead of at the
/// next poll. Requires `Authorization: Bearer <WORKER_ADMIN_TOKEN>`.
async fn refresh(State(st): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    let given = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !constant_time_eq(given.as_bytes(), st.token.as_bytes()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let queued = sqlx::query(
        "insert into refresh_request (status)
         select 'pending' where not exists (select 1 from refresh_request where status = 'pending')",
    )
    .execute(&st.pool)
    .await;
    match queued {
        Ok(_) => {
            st.wake.notify_one();
            (
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "queued": true })),
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!("queueing refresh: {e:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison() {
        assert!(constant_time_eq(b"secret-token", b"secret-token"));
        assert!(!constant_time_eq(b"secret-token", b"secret-tokeN"));
        assert!(!constant_time_eq(b"secret", b"secret-token"));
        assert!(!constant_time_eq(b"", b"x"));
    }
}
