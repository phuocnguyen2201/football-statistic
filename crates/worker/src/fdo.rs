//! Job: refresh squads from football-data.org (`/v4/competitions/{code}/teams`).
//!
//! Same rules as the one-off SQL import:
//! - teams are matched through `team_alias` (football_data_org id, then the
//!   openfootball name, which football-data.org shares);
//! - an unknown team in a league we import from football-data.co.uk is
//!   skipped and reported, never created (that would duplicate the CSV team);
//!   unknown Eredivisie / Primeira Liga teams are created;
//! - teams with an API-Football squad are left alone (no player listed twice);
//! - team details are only filled where empty.
//!
//! Raw responses are written to `RAW_DIR/football_data_org/<date>/`.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sqlx::{PgConnection, PgPool};

use crate::Config;

const SOURCE: &str = "football_data_org";
const BASE_URL: &str = "https://api.football-data.org/v4";
const COMPETITIONS: &[&str] = &["PL", "PD", "SA", "BL1", "FL1", "DED", "PPL"];
/// Leagues whose teams come from football-data.co.uk CSVs.
const CSV_LEAGUES: &[&str] = &["PL", "PD", "SA", "BL1", "FL1"];
/// Free tier: 10 requests per minute.
const GAP: Duration = Duration::from_millis(6_500);
const MAX_RETRIES: u32 = 3;

#[derive(Deserialize)]
struct TeamsResponse {
    teams: Vec<FdoTeam>,
}

#[derive(Deserialize)]
struct FdoTeam {
    id: i64,
    name: String,
    #[serde(rename = "shortName")]
    short_name: Option<String>,
    tla: Option<String>,
    crest: Option<String>,
    venue: Option<String>,
    founded: Option<i16>,
    area: Option<Area>,
    #[serde(default)]
    squad: Vec<FdoPlayer>,
}

#[derive(Deserialize)]
struct Area {
    name: Option<String>,
}

#[derive(Deserialize)]
struct FdoPlayer {
    id: i64,
    name: String,
    position: Option<String>,
    #[serde(rename = "dateOfBirth")]
    date_of_birth: Option<String>,
}

#[derive(Default)]
struct Tally {
    filled: usize,
    players: usize,
    kept: usize,
    skipped: Vec<String>,
}

pub async fn refresh_squads(pool: &PgPool, cfg: &Config) -> Result<String> {
    let run_id: i64 =
        sqlx::query_scalar("insert into ingest_run (source) values ($1) returning id")
            .bind(SOURCE)
            .fetch_one(pool)
            .await?;
    let result = run(pool, cfg).await;
    let (status, rows, message) = match &result {
        Ok((t, msg)) => ("ok", Some(t.players as i32), msg.clone()),
        Err(e) => ("failed", None, format!("{e:#}")),
    };
    sqlx::query(
        "update ingest_run set finished_at = now(), status = $2, rows = $3, message = $4 where id = $1",
    )
    .bind(run_id)
    .bind(status)
    .bind(rows)
    .bind(&message)
    .execute(pool)
    .await?;
    result.map(|(_, msg)| msg)
}

async fn run(pool: &PgPool, cfg: &Config) -> Result<(Tally, String)> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("football-statistics-worker/0.1")
        .build()?;
    let date: String = sqlx::query_scalar("select to_char(now() at time zone 'utc', 'YYYY-MM-DD')")
        .fetch_one(pool)
        .await?;
    let dir = cfg.raw_dir.join("football_data_org").join(&date);
    tokio::fs::create_dir_all(&dir).await?;

    let mut tally = Tally::default();
    for (i, code) in COMPETITIONS.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(GAP).await;
        }
        let bytes = fetch(&http, &cfg.fdo_key, code).await?;
        tokio::fs::write(dir.join(format!("{code}_teams.json")), &bytes).await?;
        let resp: TeamsResponse =
            serde_json::from_slice(&bytes).with_context(|| format!("parsing {code} teams"))?;
        for team in &resp.teams {
            load_team(pool, code, team, &mut tally).await?;
        }
        tracing::info!(code, teams = resp.teams.len(), "competition loaded");
    }

    let mut msg = format!(
        "football-data.org squads: {} filled ({} players), {} kept from API-Football",
        tally.filled, tally.players, tally.kept
    );
    if !tally.skipped.is_empty() {
        msg.push_str(&format!(
            "; unmapped, needs a team_alias row: {}",
            tally.skipped.join(", ")
        ));
    }
    Ok((tally, msg))
}

async fn fetch(http: &reqwest::Client, key: &str, code: &str) -> Result<Vec<u8>> {
    let url = format!("{BASE_URL}/competitions/{code}/teams");
    let mut attempt = 0;
    loop {
        tracing::info!(code, "GET teams");
        let resp = http.get(&url).header("X-Auth-Token", key).send().await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) if attempt < MAX_RETRIES => {
                tracing::warn!(error = %e, "request failed, retrying");
                tokio::time::sleep(backoff(attempt)).await;
                attempt += 1;
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("GET {code} teams")),
        };
        let status = resp.status();
        if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
            let wait = resp
                .headers()
                .get("x-requestcounter-reset")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(|s| Duration::from_secs(s.clamp(1, 120)))
                .unwrap_or_else(|| backoff(attempt));
            tracing::warn!(%status, ?wait, "backing off");
            tokio::time::sleep(wait).await;
            attempt += 1;
            continue;
        }
        if !status.is_success() {
            bail!("GET {code} teams: HTTP {status}");
        }
        return Ok(resp.bytes().await?.to_vec());
    }
}

async fn load_team(pool: &PgPool, code: &str, team: &FdoTeam, tally: &mut Tally) -> Result<()> {
    let mut tx = pool.begin().await?;
    let Some(team_id) = resolve_team(&mut tx, code, team).await? else {
        tally
            .skipped
            .push(format!("{} ({code}, id {})", team.name, team.id));
        return Ok(());
    };

    sqlx::query(
        "update team set code = coalesce(code, $2), logo_url = coalesce(logo_url, $3),
             venue_name = coalesce(venue_name, $4), founded = coalesce(founded, $5),
             country = coalesce(country, $6)
         where id = $1",
    )
    .bind(team_id)
    .bind(&team.tla)
    .bind(&team.crest)
    .bind(&team.venue)
    .bind(team.founded)
    .bind(team.area.as_ref().and_then(|a| a.name.as_deref()))
    .execute(&mut *tx)
    .await?;

    let has_api_squad: bool = sqlx::query_scalar(
        "select exists (
             select 1 from squad_member m
             join player_alias pa on pa.player_id = m.player_id and pa.source = 'api_football'
             where m.team_id = $1)",
    )
    .bind(team_id)
    .fetch_one(&mut *tx)
    .await?;
    if has_api_squad {
        tally.kept += 1;
        tx.commit().await?;
        return Ok(());
    }
    // An empty response must not wipe the stored squad.
    if team.squad.is_empty() {
        tracing::warn!(team = %team.name, "empty squad, kept existing rows");
        tx.commit().await?;
        return Ok(());
    }

    let mut player_ids = Vec::with_capacity(team.squad.len());
    for p in &team.squad {
        let player_id = upsert_player(&mut tx, p).await?;
        sqlx::query(
            "insert into squad_member (team_id, player_id, shirt_number, position, age, fetched_at)
             values ($1, $2, null, $3, extract(year from age(current_date, $4::date))::smallint, now())
             on conflict (team_id, player_id) do update set
                 position = excluded.position, age = excluded.age, fetched_at = excluded.fetched_at",
        )
        .bind(team_id)
        .bind(player_id)
        .bind(position(p.position.as_deref()))
        .bind(&p.date_of_birth)
        .execute(&mut *tx)
        .await?;
        player_ids.push(player_id);
    }
    sqlx::query("delete from squad_member where team_id = $1 and player_id <> all($2)")
        .bind(team_id)
        .bind(&player_ids)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    tally.filled += 1;
    tally.players += player_ids.len();
    Ok(())
}

/// Our team id for a football-data.org team, or None if it must be mapped by hand.
async fn resolve_team(conn: &mut PgConnection, code: &str, team: &FdoTeam) -> Result<Option<i32>> {
    let key = team.id.to_string();
    let alias = |source: &'static str, source_key: String| {
        sqlx::query_scalar::<_, i32>(
            "select team_id from team_alias where source = $1 and source_key = $2",
        )
        .bind(source)
        .bind(source_key)
    };
    if let Some(id) = alias(SOURCE, key.clone())
        .fetch_optional(&mut *conn)
        .await?
    {
        return Ok(Some(id));
    }
    // football-data.org and openfootball use the same club names.
    let existing = match alias("openfootball", team.name.clone())
        .fetch_optional(&mut *conn)
        .await?
    {
        Some(id) => id,
        None if CSV_LEAGUES.contains(&code) => return Ok(None),
        None => {
            sqlx::query_scalar(
                "insert into team (name, country, code, logo_url, venue_name, founded)
                 values ($1, $2, $3, $4, $5, $6) returning id",
            )
            .bind(team.short_name.as_deref().unwrap_or(&team.name))
            .bind(team.area.as_ref().and_then(|a| a.name.as_deref()))
            .bind(&team.tla)
            .bind(&team.crest)
            .bind(&team.venue)
            .bind(team.founded)
            .fetch_one(&mut *conn)
            .await?
        }
    };
    sqlx::query("insert into team_alias (source, source_key, team_id) values ($1, $2, $3)")
        .bind(SOURCE)
        .bind(&key)
        .bind(existing)
        .execute(&mut *conn)
        .await?;
    Ok(Some(existing))
}

async fn upsert_player(conn: &mut PgConnection, p: &FdoPlayer) -> Result<i32> {
    let key = p.id.to_string();
    let pos = position(p.position.as_deref());
    let existing: Option<i32> = sqlx::query_scalar(
        "select player_id from player_alias where source = $1 and source_key = $2",
    )
    .bind(SOURCE)
    .bind(&key)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(id) = existing {
        sqlx::query("update player set name = $2, position = $3 where id = $1")
            .bind(id)
            .bind(&p.name)
            .bind(pos)
            .execute(&mut *conn)
            .await?;
        return Ok(id);
    }
    let id: i32 =
        sqlx::query_scalar("insert into player (name, position) values ($1, $2) returning id")
            .bind(&p.name)
            .bind(pos)
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

/// football-data.org position -> the groups the team page uses.
fn position(p: Option<&str>) -> Option<&'static str> {
    match p? {
        "Goalkeeper" => Some("Goalkeeper"),
        "Defence" => Some("Defender"),
        "Midfield" => Some("Midfielder"),
        "Offence" => Some("Attacker"),
        _ => None,
    }
}

/// 2s, 4s, 8s ... capped at 60s.
fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(2u64.saturating_pow(attempt + 1).min(60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_map_to_page_groups() {
        assert_eq!(position(Some("Goalkeeper")), Some("Goalkeeper"));
        assert_eq!(position(Some("Defence")), Some("Defender"));
        assert_eq!(position(Some("Midfield")), Some("Midfielder"));
        assert_eq!(position(Some("Offence")), Some("Attacker"));
        assert_eq!(position(Some("Coach")), None);
        assert_eq!(position(None), None);
    }

    #[test]
    fn parses_real_response_shape() {
        let r: TeamsResponse = serde_json::from_str(
            r#"{"count":1,"teams":[{"area":{"name":"England"},"id":57,"name":"Arsenal FC",
                "shortName":"Arsenal","tla":"ARS","crest":"c.png","venue":"Emirates Stadium",
                "founded":1886,"squad":[{"id":3189,"name":"Kepa Arrizabalaga",
                "position":"Goalkeeper","dateOfBirth":"1994-10-03","nationality":"Spain"},
                {"id":1,"name":"No Position","position":null,"dateOfBirth":null}]}]}"#,
        )
        .unwrap();
        let t = &r.teams[0];
        assert_eq!((t.id, t.founded), (57, Some(1886)));
        assert_eq!(t.squad.len(), 2);
        assert_eq!(t.squad[1].position, None);
    }

    #[test]
    fn team_without_squad_field_parses() {
        let r: TeamsResponse = serde_json::from_str(r#"{"teams":[{"id":1,"name":"X"}]}"#).unwrap();
        assert!(r.teams[0].squad.is_empty());
    }

    #[test]
    fn competitions_cover_csv_leagues() {
        assert!(CSV_LEAGUES.iter().all(|c| COMPETITIONS.contains(c)));
    }
}
