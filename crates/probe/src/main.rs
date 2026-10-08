//! Phase 0 probe: check the 8 core leagues on API-Football, then fetch each
//! league's teams and each team's squad (optionally also the paged player
//! season stats). Every raw response is saved under
//! `raw/<date>/`; reruns with the same `--date` resume from disk, so a pull
//! larger than the daily quota can be finished over several days.

mod client;
mod leagues;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

use client::{ApiFootballClient, QuotaGuardTripped};
use leagues::{League, LEAGUES};

const USAGE: &str = "\
Usage: probe [--league NAME] [--season YYYY] [--dry-run] [--player-stats] [--date YYYY-MM-DD]

  --league NAME      only this league (e.g. \"Scottish Premiership\")
  --season YYYY      season start year (default: the league's current season;
                     the free plan only allows some past seasons)
  --dry-run          league/season/coverage check only (1 call per league)
  --player-stats     also fetch paged player season stats (~30 calls per league)
  --date YYYY-MM-DD  raw/ subfolder to write/resume (default: today, UTC)";

#[derive(Debug, Default)]
struct Args {
    league: Option<String>,
    season: Option<u16>,
    dry_run: bool,
    player_stats: bool,
    date: Option<String>,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Option<Args>> {
    let mut args = Args::default();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| it.next().with_context(|| format!("{flag} needs a value"));
        match a.as_str() {
            "--league" => args.league = Some(value("--league")?),
            "--season" => {
                let v = value("--season")?;
                match v.parse::<u16>() {
                    Ok(y) if (2000..=2100).contains(&y) => args.season = Some(y),
                    _ => bail!("--season must be a year like 2023, got {v:?}"),
                }
            }
            "--dry-run" => args.dry_run = true,
            "--player-stats" => args.player_stats = true,
            "--date" => args.date = Some(value("--date")?),
            "-h" | "--help" => return Ok(None),
            other => bail!("unknown argument {other:?}\n\n{USAGE}"),
        }
    }
    if let Some(d) = &args.date {
        if !is_date(d) {
            bail!("--date must be YYYY-MM-DD, got {d:?}");
        }
    }
    if let Some(l) = &args.league {
        if leagues::find_league(l).is_none() {
            let names: Vec<_> = LEAGUES.iter().map(|l| l.name).collect();
            bail!("unknown league {l:?}; expected one of {names:?}");
        }
    }
    Ok(Some(args))
}

#[derive(Debug, Default, Serialize)]
struct LeagueSummary {
    league: String,
    league_id: u32,
    status: String,
    /// Name and country as API-Football reports them for `league_id`.
    api_name: Option<String>,
    season: Option<u16>,
    /// `coverage.players` flag for the chosen season.
    players_coverage: Option<bool>,
    teams: usize,
    /// Players across all fetched squads (current rosters).
    squad_players: usize,
    stats_pages: u32,
    stats_players: usize,
}

#[derive(Serialize)]
struct RunSummary<'a> {
    date: &'a str,
    dry_run: bool,
    http_calls: u64,
    cache_hits: u64,
    quota: client::Quota,
    stopped_by_quota_guard: bool,
    leagues: &'a [LeagueSummary],
}

#[tokio::main]
async fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode> {
    let Some(args) = parse_args(std::env::args().skip(1))? else {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    };
    let mut client = ApiFootballClient::from_env()?;
    let date = args.date.clone().unwrap_or_else(today_utc);
    let root = PathBuf::from("raw").join(&date);

    let selected: Vec<&League> = match &args.league {
        Some(name) => leagues::find_league(name).into_iter().collect(),
        None => LEAGUES.iter().collect(),
    };

    let mut summaries = Vec::new();
    let mut stopped = false;
    for league in selected {
        let mut s = LeagueSummary {
            league: league.name.to_string(),
            league_id: league.id,
            ..Default::default()
        };
        match run_league(
            &mut client,
            &root,
            league,
            args.season,
            args.dry_run,
            args.player_stats,
            &mut s,
        )
        .await
        {
            Ok(()) => s.status = "ok".into(),
            Err(e) if e.is::<QuotaGuardTripped>() => {
                tracing::warn!("{e}; stopping. Rerun with --date {date} to resume.");
                s.status = "stopped: quota guard".into();
                stopped = true;
            }
            Err(e) => {
                tracing::error!(league = league.name, "{e:#}");
                s.status = format!("error: {e:#}");
            }
        }
        summaries.push(s);
        if stopped {
            break;
        }
    }

    let summary = RunSummary {
        date: &date,
        dry_run: args.dry_run,
        http_calls: client.http_calls,
        cache_hits: client.cache_hits,
        quota: client.quota,
        stopped_by_quota_guard: stopped,
        leagues: &summaries,
    };
    std::fs::create_dir_all(&root)?;
    let summary_path = root.join("summary.json");
    std::fs::write(&summary_path, serde_json::to_vec_pretty(&summary)?)?;
    print_table(&summary);
    println!("summary: {}", summary_path.display());

    Ok(if stopped {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

async fn run_league(
    client: &mut ApiFootballClient,
    root: &Path,
    league: &League,
    season_override: Option<u16>,
    dry_run: bool,
    player_stats: bool,
    s: &mut LeagueSummary,
) -> Result<()> {
    // 1. League check, season choice, coverage.
    let id = league.id;
    let body = client
        .fetch(
            "/leagues",
            &[("id", id.to_string())],
            &root.join("leagues").join(format!("{id}.json")),
        )
        .await?;
    let info = body
        .pointer("/response/0")
        .with_context(|| format!("league id {id} not found"))?;
    let api_name = info
        .pointer("/league/name")
        .and_then(Value::as_str)
        .unwrap_or("?");
    let api_country = info
        .pointer("/country/name")
        .and_then(Value::as_str)
        .unwrap_or("?");
    s.api_name = Some(format!("{api_name} ({api_country})"));
    if leagues::normalize(api_country) != leagues::normalize(league.country) {
        bail!(
            "id {id} is {api_name} ({api_country}), expected a {} league",
            league.country
        );
    }

    let seasons = info
        .get("seasons")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let season = season_override
        .or_else(|| leagues::pick_current_season(&seasons))
        .context("no seasons returned")?;
    s.season = Some(season);
    s.players_coverage = seasons
        .iter()
        .find(|x| x.get("year").and_then(Value::as_u64) == Some(u64::from(season)))
        .and_then(|x| x.pointer("/coverage/players"))
        .and_then(Value::as_bool);
    if dry_run {
        return Ok(());
    }

    // 2. Teams in the league for this season.
    let teams = client
        .fetch(
            "/teams",
            &[("league", id.to_string()), ("season", season.to_string())],
            &root.join("teams").join(format!("{id}_{season}.json")),
        )
        .await?;
    let team_ids: Vec<u64> = teams
        .get("response")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|t| t.pointer("/team/id").and_then(Value::as_u64))
        .collect();
    if team_ids.is_empty() {
        bail!("no teams returned for season {season}");
    }
    s.teams = team_ids.len();

    // 3. Each team's squad (current roster; the endpoint has no season).
    for tid in team_ids {
        let squad = client
            .fetch(
                "/players/squads",
                &[("team", tid.to_string())],
                &root.join("squads").join(format!("{tid}.json")),
            )
            .await?;
        s.squad_players += squad
            .pointer("/response/0/players")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
    }
    if !player_stats {
        return Ok(());
    }

    // 4. Player season stats, all pages.
    let (pages, players) = client
        .fetch_all_pages(
            "/players",
            &[("league", id.to_string()), ("season", season.to_string())],
            |p| {
                root.join("players")
                    .join(format!("{id}_{season}_p{p}.json"))
            },
        )
        .await?;
    s.stats_pages = pages;
    s.stats_players = players.len();
    Ok(())
}

fn print_table(summary: &RunSummary) {
    println!(
        "\n{:<22} {:>4} {:<32} {:>6} {:>8} {:>5} {:>7} {:>7}  status",
        "league", "id", "api name", "season", "coverage", "teams", "squad", "stats"
    );
    for l in summary.leagues {
        println!(
            "{:<22} {:>4} {:<32} {:>6} {:>8} {:>5} {:>7} {:>7}  {}",
            l.league,
            l.league_id,
            l.api_name.as_deref().unwrap_or("-"),
            l.season.map_or("-".into(), |v| v.to_string()),
            l.players_coverage.map_or("?".into(), |v| v.to_string()),
            l.teams,
            l.squad_players,
            l.stats_players,
            l.status
        );
    }
    println!(
        "\nhttp calls: {}, from disk: {}, daily quota remaining: {}/{}",
        summary.http_calls,
        summary.cache_hits,
        summary
            .quota
            .remaining
            .map_or("?".into(), |v| v.to_string()),
        summary.quota.limit.map_or("?".into(), |v| v.to_string()),
    );
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b.iter().enumerate().all(|(i, c)| {
            if i == 4 || i == 7 {
                *c == b'-'
            } else {
                c.is_ascii_digit()
            }
        })
}

fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 -> (year, month, day). Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: &[&str]) -> Result<Option<Args>> {
        parse_args(v.iter().map(|s| s.to_string()))
    }

    #[test]
    fn civil_from_days_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_734), (2026, 10, 8));
    }

    #[test]
    fn date_validation() {
        assert!(is_date("2026-10-08"));
        assert!(!is_date("2026-1-08"));
        assert!(!is_date("../../etc"));
    }

    #[test]
    fn arg_parsing() {
        let a = args(&[
            "--league",
            "Serie A",
            "--dry-run",
            "--date",
            "2026-10-08",
            "--season",
            "2023",
            "--player-stats",
        ])
        .unwrap()
        .unwrap();
        assert!(a.player_stats);
        assert_eq!(a.league.as_deref(), Some("Serie A"));
        assert_eq!(a.season, Some(2023));
        assert!(a.dry_run);
        assert!(args(&["--help"]).unwrap().is_none());
        assert!(args(&["--league", "MLS"]).is_err());
        assert!(args(&["--season", "23"]).is_err());
        assert!(args(&["--date", "today"]).is_err());
        assert!(args(&["--bogus"]).is_err());
        assert!(args(&["--league"]).is_err());
    }
}
