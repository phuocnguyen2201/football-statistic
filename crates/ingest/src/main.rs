//! Load API-Football raw responses (teams, squads) from `raw/<date>/` into
//! Postgres. Idempotent: teams and players are matched through
//! `team_alias` / `player_alias` on (source, source_key), so reruns update.
//!
//! Usage: ingest [RAW_DIR]   (default: newest `raw/<YYYY-MM-DD>` folder)
//! Needs WORKER_DATABASE_URL (the `worker` role).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool};

const SOURCE: &str = "api_football";

/// API-Football league ID -> football-data.co.uk code (`competition.code`).
// shortcut: duplicates the IDs in crates/probe/src/leagues.rs; move to a shared crate when a third user appears.
const COMPETITIONS: &[(u32, &str)] = &[
    (39, "E0"),
    (179, "SC0"),
    (140, "SP1"),
    (78, "D1"),
    (135, "I1"),
    (61, "F1"),
    (88, "N1"),
    (94, "P1"),
];

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    response: Vec<T>,
}

#[derive(Debug, Deserialize)]
struct TeamEntry {
    team: ApiTeam,
    venue: Option<ApiVenue>,
}

#[derive(Debug, Deserialize)]
struct ApiTeam {
    id: i64,
    name: String,
    code: Option<String>,
    country: Option<String>,
    founded: Option<i16>,
    logo: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiVenue {
    name: Option<String>,
    city: Option<String>,
    capacity: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct SquadEntry {
    team: SquadTeam,
    players: Vec<ApiPlayer>,
}

#[derive(Debug, Deserialize)]
struct SquadTeam {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct ApiPlayer {
    id: i64,
    name: String,
    age: Option<i16>,
    number: Option<i16>,
    position: Option<String>,
    photo: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let dir = match std::env::args().nth(1) {
        Some(d) => PathBuf::from(d),
        None => latest_raw_dir(Path::new("raw"))?,
    };
    tracing::info!(dir = %dir.display(), "loading");

    let url = std::env::var("WORKER_DATABASE_URL")
        .ok()
        .filter(|u| u.starts_with("postgres://") || u.starts_with("postgresql://"))
        .context("WORKER_DATABASE_URL must be a postgresql:// connection string (Supabase > Connect > Session mode)")?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await?;

    let run_id: i64 =
        sqlx::query_scalar("insert into ingest_run (source) values ($1) returning id")
            .bind(SOURCE)
            .fetch_one(&pool)
            .await?;
    let result = load(&pool, &dir).await;
    let (status, rows, message) = match &result {
        Ok(rows) => ("ok", Some(*rows as i32), None),
        Err(e) => ("failed", None, Some(format!("{e:#}"))),
    };
    sqlx::query(
        "update ingest_run set finished_at = now(), status = $2, rows = $3, message = $4 where id = $1",
    )
    .bind(run_id)
    .bind(status)
    .bind(rows)
    .bind(message)
    .execute(&pool)
    .await?;

    let rows = result?;
    tracing::info!(rows, "done");
    Ok(())
}

async fn load(pool: &PgPool, dir: &Path) -> Result<usize> {
    let mut rows = 0;

    for path in json_files(&dir.join("teams"))? {
        let name = file_name(&path);
        let Some((league_id, season)) = parse_teams_filename(&name) else {
            tracing::warn!(file = %name, "unexpected teams file name, skipped");
            continue;
        };
        let Some(code) = competition_code(league_id) else {
            tracing::warn!(league_id, "unknown league, skipped");
            continue;
        };
        let teams: Envelope<TeamEntry> = read_json(&path)?;

        let mut tx = pool.begin().await?;
        let competition_id: i32 = sqlx::query_scalar("select id from competition where code = $1")
            .bind(code)
            .fetch_optional(&mut *tx)
            .await?
            .with_context(|| format!("competition {code} missing; run the migration"))?;
        let season_id: i32 = sqlx::query_scalar(
            "insert into season (competition_id, start_year, label) values ($1, $2, $3)
             on conflict (competition_id, start_year) do update set label = excluded.label
             returning id",
        )
        .bind(competition_id)
        .bind(season)
        .bind(season_label(season))
        .fetch_one(&mut *tx)
        .await?;
        for entry in &teams.response {
            let team_id = upsert_team(&mut tx, entry).await?;
            sqlx::query("insert into team_season (season_id, team_id) values ($1, $2) on conflict do nothing")
                .bind(season_id)
                .bind(team_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        tracing::info!(file = %name, teams = teams.response.len(), "teams loaded");
        rows += teams.response.len();
    }

    for path in json_files(&dir.join("squads"))? {
        let squads: Envelope<SquadEntry> = read_json(&path)?;
        for squad in &squads.response {
            let team_id: Option<i32> = sqlx::query_scalar(
                "select team_id from team_alias where source = $1 and source_key = $2",
            )
            .bind(SOURCE)
            .bind(squad.team.id.to_string())
            .fetch_optional(pool)
            .await?;
            let Some(team_id) = team_id else {
                tracing::warn!(api_team = squad.team.id, "squad for unknown team, skipped");
                continue;
            };
            // An empty response must not wipe the stored squad.
            if squad.players.is_empty() {
                tracing::warn!(team_id, "empty squad, kept existing rows");
                continue;
            }

            let mut tx = pool.begin().await?;
            let mut player_ids = Vec::with_capacity(squad.players.len());
            for p in &squad.players {
                let player_id = upsert_player(&mut tx, p).await?;
                sqlx::query(
                    "insert into squad_member (team_id, player_id, shirt_number, position, age, fetched_at)
                     values ($1, $2, $3, $4, $5, now())
                     on conflict (team_id, player_id) do update set
                         shirt_number = excluded.shirt_number, position = excluded.position,
                         age = excluded.age, fetched_at = excluded.fetched_at",
                )
                .bind(team_id)
                .bind(player_id)
                .bind(p.number)
                .bind(&p.position)
                .bind(p.age)
                .execute(&mut *tx)
                .await?;
                player_ids.push(player_id);
            }
            // Players who left the club drop out of the current squad.
            sqlx::query("delete from squad_member where team_id = $1 and player_id <> all($2)")
                .bind(team_id)
                .bind(&player_ids)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            rows += player_ids.len();
        }
    }
    tracing::info!(rows, "squads loaded");
    Ok(rows)
}

async fn upsert_team(conn: &mut PgConnection, entry: &TeamEntry) -> Result<i32> {
    let t = &entry.team;
    let v = entry.venue.as_ref();
    let key = t.id.to_string();
    let existing: Option<i32> =
        sqlx::query_scalar("select team_id from team_alias where source = $1 and source_key = $2")
            .bind(SOURCE)
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?;
    let team_id: i32 = match existing {
        Some(id) => {
            sqlx::query(
                "update team set name = $2, code = $3, country = $4, founded = $5, logo_url = $6,
                     venue_name = $7, venue_city = $8, venue_capacity = $9
                 where id = $1",
            )
            .bind(id)
            .bind(&t.name)
            .bind(&t.code)
            .bind(&t.country)
            .bind(t.founded)
            .bind(&t.logo)
            .bind(v.and_then(|v| v.name.as_deref()))
            .bind(v.and_then(|v| v.city.as_deref()))
            .bind(v.and_then(|v| v.capacity))
            .execute(&mut *conn)
            .await?;
            id
        }
        None => {
            let id: i32 = sqlx::query_scalar(
                "insert into team (name, code, country, founded, logo_url, venue_name, venue_city, venue_capacity)
                 values ($1, $2, $3, $4, $5, $6, $7, $8) returning id",
            )
            .bind(&t.name)
            .bind(&t.code)
            .bind(&t.country)
            .bind(t.founded)
            .bind(&t.logo)
            .bind(v.and_then(|v| v.name.as_deref()))
            .bind(v.and_then(|v| v.city.as_deref()))
            .bind(v.and_then(|v| v.capacity))
            .fetch_one(&mut *conn)
            .await?;
            sqlx::query("insert into team_alias (source, source_key, team_id) values ($1, $2, $3)")
                .bind(SOURCE)
                .bind(&key)
                .bind(id)
                .execute(&mut *conn)
                .await?;
            id
        }
    };
    Ok(team_id)
}

async fn upsert_player(conn: &mut PgConnection, p: &ApiPlayer) -> Result<i32> {
    let key = p.id.to_string();
    let existing: Option<i32> = sqlx::query_scalar(
        "select player_id from player_alias where source = $1 and source_key = $2",
    )
    .bind(SOURCE)
    .bind(&key)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = existing {
        sqlx::query("update player set name = $2, position = $3, photo_url = $4 where id = $1")
            .bind(id)
            .bind(&p.name)
            .bind(&p.position)
            .bind(&p.photo)
            .execute(&mut *conn)
            .await?;
        return Ok(id);
    }
    let id: i32 = sqlx::query_scalar(
        "insert into player (name, position, photo_url) values ($1, $2, $3) returning id",
    )
    .bind(&p.name)
    .bind(&p.position)
    .bind(&p.photo)
    .fetch_one(&mut *conn)
    .await?;
    sqlx::query("insert into player_alias (source, source_key, player_id) values ($1, $2, $3)")
        .bind(SOURCE)
        .bind(&key)
        .bind(id)
        .execute(&mut *conn)
        .await?;
    Ok(id)
}

fn competition_code(league_id: u32) -> Option<&'static str> {
    COMPETITIONS
        .iter()
        .find(|(id, _)| *id == league_id)
        .map(|(_, code)| *code)
}

/// `39_2024.json` -> (39, 2024)
fn parse_teams_filename(name: &str) -> Option<(u32, i16)> {
    let (league, season) = name.strip_suffix(".json")?.split_once('_')?;
    Some((league.parse().ok()?, season.parse().ok()?))
}

/// 2024 -> "2024/25"
fn season_label(start_year: i16) -> String {
    format!("{start_year}/{:02}", (start_year + 1) % 100)
}

/// Newest `YYYY-MM-DD` folder under `raw/` (ISO dates sort lexically).
fn latest_raw_dir(raw: &Path) -> Result<PathBuf> {
    let mut dates: Vec<String> = std::fs::read_dir(raw)
        .with_context(|| format!("reading {}", raw.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    dates.retain(|d| is_date_dir(d));
    match dates.into_iter().max() {
        Some(d) => Ok(raw.join(d)),
        None => bail!("no raw/<YYYY-MM-DD> folder found"),
    }
}

fn is_date_dir(s: &str) -> bool {
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

/// `*.json` files in `dir`, sorted; a missing folder is just empty.
fn json_files(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    Ok(files)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_filename_parsing() {
        assert_eq!(parse_teams_filename("39_2024.json"), Some((39, 2024)));
        assert_eq!(parse_teams_filename("179_2022.json"), Some((179, 2022)));
        assert_eq!(parse_teams_filename("39.json"), None);
        assert_eq!(parse_teams_filename("x_2024.json"), None);
        assert_eq!(parse_teams_filename("39_2024.tmp"), None);
    }

    #[test]
    fn season_labels() {
        assert_eq!(season_label(2024), "2024/25");
        assert_eq!(season_label(2099), "2099/00");
        assert_eq!(season_label(2008), "2008/09");
    }

    #[test]
    fn competition_codes() {
        assert_eq!(competition_code(39), Some("E0"));
        assert_eq!(competition_code(179), Some("SC0"));
        assert_eq!(competition_code(1), None);
    }

    #[test]
    fn date_dirs() {
        assert!(is_date_dir("2026-10-08"));
        assert!(!is_date_dir("latest"));
        assert!(!is_date_dir("2026-10-8"));
    }

    #[test]
    fn parses_api_football_team_and_squad_shapes() {
        let teams: Envelope<TeamEntry> = serde_json::from_str(
            r#"{"response":[{"team":{"id":33,"name":"Manchester United","code":"MUN",
                "country":"England","founded":1878,"national":false,"logo":"x.png"},
                "venue":{"id":556,"name":"Old Trafford","city":"Manchester","capacity":76212}}]}"#,
        )
        .unwrap();
        let t = &teams.response[0];
        assert_eq!(t.team.id, 33);
        assert_eq!(t.team.founded, Some(1878));
        assert_eq!(t.venue.as_ref().unwrap().capacity, Some(76212));

        let squads: Envelope<SquadEntry> = serde_json::from_str(
            r#"{"response":[{"team":{"id":1386,"name":"Dundee Utd"},"players":[
                {"id":328126,"name":"J. Amissah","age":24,"number":30,"position":"Goalkeeper","photo":"p.png"},
                {"id":1,"name":"No Number","age":null,"number":null,"position":null,"photo":null}]}]}"#,
        )
        .unwrap();
        let s = &squads.response[0];
        assert_eq!(s.team.id, 1386);
        assert_eq!(s.players[0].number, Some(30));
        assert_eq!(s.players[1].number, None);
    }
}
