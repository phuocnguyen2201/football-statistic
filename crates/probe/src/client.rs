//! Minimal API-Football (v3) client: rate limiting, 429/5xx backoff, daily
//! quota guard, and raw-response persistence with resume-from-disk.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use reqwest::header::HeaderMap;
use reqwest::StatusCode;
use serde_json::Value;

const BASE_URL: &str = "https://v3.football.api-sports.io";
// shortcut: assumes the free tier's ~10 req/min; lower it on a paid plan.
const MIN_GAP: Duration = Duration::from_millis(6_500);
const MAX_RETRIES: u32 = 5;
/// Stop once this fraction of the daily quota has been used.
const QUOTA_STOP_USED_FRACTION: f64 = 0.80;

/// Returned (inside anyhow) when the quota guard trips, so callers can stop cleanly.
#[derive(Debug)]
pub struct QuotaGuardTripped {
    pub remaining: u64,
    pub limit: u64,
}

impl std::fmt::Display for QuotaGuardTripped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "daily quota guard: {} of {} requests remaining (>= {:.0}% used)",
            self.remaining,
            self.limit,
            QUOTA_STOP_USED_FRACTION * 100.0
        )
    }
}

impl std::error::Error for QuotaGuardTripped {}

#[derive(Debug, Clone, Copy, Default, serde::Serialize)]
pub struct Quota {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
}

pub struct ApiFootballClient {
    http: reqwest::Client,
    api_key: String,
    last_request: Option<Instant>,
    pub quota: Quota,
    pub http_calls: u64,
    pub cache_hits: u64,
}

impl ApiFootballClient {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("API_FOOTBALL_KEY")
            .context("API_FOOTBALL_KEY is not set (put it in .env or the environment)")?;
        if api_key.trim().is_empty() {
            bail!("API_FOOTBALL_KEY is empty");
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("football-statistics-probe/0.1")
            .build()?;
        Ok(Self {
            http,
            api_key,
            last_request: None,
            quota: Quota::default(),
            http_calls: 0,
            cache_hits: 0,
        })
    }

    /// Fetch `path` and save the raw body to `out`. If `out` already exists it
    /// is read from disk and no request is made. Responses whose `errors`
    /// field is non-empty (API-Football reports plan/season/key problems with
    /// HTTP 200) are not saved, so a rerun after fixing the cause refetches.
    pub async fn fetch(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        out: &Path,
    ) -> Result<Value> {
        if out.exists() {
            self.cache_hits += 1;
            let bytes = std::fs::read(out).with_context(|| format!("reading {}", out.display()))?;
            return serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing {}", out.display()));
        }

        self.check_quota()?;
        let url = format!("{BASE_URL}{path}");
        let mut attempt = 0;
        loop {
            self.pace().await;
            // Only path + query are logged; the key travels in a header.
            tracing::info!(%path, ?query, "GET");
            let resp = self
                .http
                .get(&url)
                .header("x-apisports-key", &self.api_key)
                .query(query)
                .send()
                .await;
            self.last_request = Some(Instant::now());
            self.http_calls += 1;

            let resp = match resp {
                Ok(r) => r,
                Err(e) if attempt < MAX_RETRIES => {
                    let wait = backoff(attempt);
                    tracing::warn!(error = %e, ?wait, "request failed, retrying");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                    continue;
                }
                Err(e) => return Err(e).with_context(|| format!("GET {path}")),
            };

            let status = resp.status();
            let headers = resp.headers().clone();
            self.update_quota(&headers);
            let bytes = resp
                .bytes()
                .await
                .with_context(|| format!("reading body of {path}"))?;
            let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            let errors = body.get("errors").cloned().unwrap_or(Value::Null);
            let rate_limited =
                status == StatusCode::TOO_MANY_REQUESTS || errors.get("rateLimit").is_some();

            if rate_limited || status.is_server_error() {
                if attempt >= MAX_RETRIES {
                    bail!("GET {path}: {status} after {MAX_RETRIES} retries");
                }
                let wait = header_u64(&headers, "retry-after")
                    .map(Duration::from_secs)
                    .unwrap_or_else(|| backoff(attempt));
                tracing::warn!(%status, ?wait, "backing off");
                tokio::time::sleep(wait).await;
                attempt += 1;
                continue;
            }

            if header_u64(&headers, "x-ratelimit-remaining") == Some(0) {
                tracing::info!("per-minute limit reached, waiting 60s");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }

            if !status.is_success() {
                bail!("GET {path}: HTTP {status}");
            }
            if has_errors(&errors) {
                bail!("GET {path}: API errors: {errors}");
            }
            write_atomic(out, &bytes).await?;
            return Ok(body);
        }
    }

    /// Fetch every page of a paginated endpoint (`paging.current/total`).
    /// `out_for_page(n)` gives the file for page `n`. Returns all `response` items.
    pub async fn fetch_all_pages(
        &mut self,
        path: &str,
        query: &[(&str, String)],
        out_for_page: impl Fn(u32) -> PathBuf,
    ) -> Result<(u32, Vec<Value>)> {
        let mut items = Vec::new();
        let mut page = 1u32;
        loop {
            let mut q = query.to_vec();
            q.push(("page", page.to_string()));
            let body = self.fetch(path, &q, &out_for_page(page)).await?;
            if let Some(data) = body.get("response").and_then(Value::as_array) {
                items.extend(data.iter().cloned());
            }
            let total = body
                .pointer("/paging/total")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            if u64::from(page) >= total {
                return Ok((page, items));
            }
            page += 1;
        }
    }

    fn check_quota(&self) -> Result<()> {
        if let (Some(limit), Some(remaining)) = (self.quota.limit, self.quota.remaining) {
            if quota_exhausted(limit, remaining) {
                return Err(QuotaGuardTripped { remaining, limit }.into());
            }
        }
        Ok(())
    }

    fn update_quota(&mut self, headers: &HeaderMap) {
        if let Some(v) = header_u64(headers, "x-ratelimit-requests-limit") {
            self.quota.limit = Some(v);
        }
        if let Some(v) = header_u64(headers, "x-ratelimit-requests-remaining") {
            self.quota.remaining = Some(v);
        }
    }

    async fn pace(&self) {
        if let Some(last) = self.last_request {
            let elapsed = last.elapsed();
            if elapsed < MIN_GAP {
                tokio::time::sleep(MIN_GAP - elapsed).await;
            }
        }
    }
}

fn header_u64(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse().ok())
}

/// API-Football returns `"errors": []` when fine, else an object or non-empty array.
pub fn has_errors(errors: &Value) -> bool {
    match errors {
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        _ => true,
    }
}

/// True once `QUOTA_STOP_USED_FRACTION` of the daily quota is used.
pub fn quota_exhausted(limit: u64, remaining: u64) -> bool {
    if limit == 0 {
        return false;
    }
    let used = limit.saturating_sub(remaining) as f64 / limit as f64;
    used >= QUOTA_STOP_USED_FRACTION
}

/// 2s, 4s, 8s, ... capped at 60s.
pub fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempt + 1).min(60))
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, bytes)
        .await
        .with_context(|| format!("writing {}", tmp.display()))?;
    tokio::fs::rename(&tmp, path)
        .await
        .with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quota_guard_trips_at_80_percent_used() {
        assert!(!quota_exhausted(100, 100));
        assert!(!quota_exhausted(100, 21));
        assert!(quota_exhausted(100, 20));
        assert!(quota_exhausted(100, 0));
        assert!(!quota_exhausted(0, 0));
    }

    #[test]
    fn errors_field_detection() {
        assert!(!has_errors(&json!([])));
        assert!(!has_errors(&json!({})));
        assert!(!has_errors(&Value::Null));
        assert!(has_errors(
            &json!({"plan": "Free plans do not have access to this season"})
        ));
        assert!(has_errors(&json!(["bad"])));
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff(0), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(8));
        assert_eq!(backoff(10), Duration::from_secs(60));
    }
}
