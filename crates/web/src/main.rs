//! Server-rendered site: leagues -> teams -> squads, head-to-head, plus the "Refresh now"
//! button that queues a `refresh_request` row. Connects as `web_reader`.

mod h2h;

use std::sync::Arc;

use anyhow::Context;
use askama::Template;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

#[derive(Clone)]
struct AppState {
    pool: PgPool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let url = std::env::var("WEB_DATABASE_URL")
        .ok()
        .filter(|u| u.starts_with("postgres://") || u.starts_with("postgresql://"))
        .context("WEB_DATABASE_URL must be a postgresql:// connection string (Supabase > Connect > Session mode)")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await?;
    let bind = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());

    let app = Router::new()
        .route("/", get(index))
        .route("/league/{code}", get(league))
        .route("/team/{id}", get(team))
        .route("/h2h", get(h2h::page))
        .route("/refresh", post(refresh))
        .with_state(Arc::new(AppState { pool }));

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("listening on http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

// ------------------------------------------------------------------ errors

enum AppError {
    NotFound,
    Internal(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for AppError {
    fn from(e: E) -> Self {
        AppError::Internal(e.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match self {
            AppError::NotFound => {
                (StatusCode::NOT_FOUND, Html("<h1>Not found</h1>")).into_response()
            }
            AppError::Internal(e) => {
                tracing::error!("{e:#}");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Html("<h1>Something went wrong</h1>"),
                )
                    .into_response()
            }
        }
    }
}

type Page = Result<Html<String>, AppError>;

// ------------------------------------------------------------------- pages

#[derive(sqlx::FromRow)]
struct LeagueRow {
    code: String,
    name: String,
    country: String,
    season_label: Option<String>,
    teams: i64,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexPage {
    leagues: Vec<LeagueRow>,
}

async fn index(State(st): State<Arc<AppState>>) -> Page {
    let leagues = sqlx::query_as::<_, LeagueRow>(
        "select c.code, c.name, c.country, s.label as season_label, count(ts.team_id) as teams
         from competition c
         left join lateral (
             select id, label from season where competition_id = c.id order by start_year desc limit 1
         ) s on true
         left join team_season ts on ts.season_id = s.id
         group by c.id, s.label
         order by c.id",
    )
    .fetch_all(&st.pool)
    .await?;
    Ok(Html(IndexPage { leagues }.render()?))
}

#[derive(sqlx::FromRow)]
struct CompetitionRow {
    name: String,
    country: String,
    season_id: Option<i32>,
    season_label: Option<String>,
}

#[derive(sqlx::FromRow)]
struct TeamListRow {
    id: i32,
    name: String,
    logo_url: Option<String>,
    venue_name: Option<String>,
    venue_city: Option<String>,
    players: i64,
}

#[derive(Template)]
#[template(path = "league.html")]
struct LeaguePage {
    competition: CompetitionRow,
    teams: Vec<TeamListRow>,
}

async fn league(State(st): State<Arc<AppState>>, Path(code): Path<String>) -> Page {
    let competition = sqlx::query_as::<_, CompetitionRow>(
        "select c.name, c.country, s.id as season_id, s.label as season_label
         from competition c
         left join lateral (
             select id, label from season where competition_id = c.id order by start_year desc limit 1
         ) s on true
         where c.code = $1",
    )
    .bind(&code)
    .fetch_optional(&st.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let teams = match competition.season_id {
        Some(season_id) => {
            sqlx::query_as::<_, TeamListRow>(
                "select t.id, t.name, t.logo_url, t.venue_name, t.venue_city,
                        (select count(*) from squad_member m where m.team_id = t.id) as players
                 from team_season ts
                 join team t on t.id = ts.team_id
                 where ts.season_id = $1
                 order by t.name",
            )
            .bind(season_id)
            .fetch_all(&st.pool)
            .await?
        }
        None => Vec::new(),
    };
    Ok(Html(LeaguePage { competition, teams }.render()?))
}

#[derive(sqlx::FromRow)]
struct TeamRow {
    name: String,
    country: Option<String>,
    founded: Option<i16>,
    logo_url: Option<String>,
    venue_name: Option<String>,
    venue_city: Option<String>,
    venue_capacity: Option<i32>,
}

#[derive(sqlx::FromRow)]
struct SquadRow {
    name: String,
    photo_url: Option<String>,
    shirt_number: Option<i16>,
    position: Option<String>,
    age: Option<i16>,
}

#[derive(Template)]
#[template(path = "team.html")]
struct TeamPage {
    id: i32,
    team: TeamRow,
    groups: Vec<(String, Vec<SquadRow>)>,
}

async fn team(State(st): State<Arc<AppState>>, Path(id): Path<i32>) -> Page {
    let team = sqlx::query_as::<_, TeamRow>(
        "select name, country, founded, logo_url, venue_name, venue_city, venue_capacity
         from team where id = $1",
    )
    .bind(id)
    .fetch_optional(&st.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let squad = sqlx::query_as::<_, SquadRow>(
        "select p.name, p.photo_url, m.shirt_number, m.position, m.age
         from squad_member m
         join player p on p.id = m.player_id
         where m.team_id = $1
         order by case m.position
                      when 'Goalkeeper' then 1 when 'Defender' then 2
                      when 'Midfielder' then 3 when 'Attacker' then 4 else 5 end,
                  m.shirt_number nulls last, p.name",
    )
    .bind(id)
    .fetch_all(&st.pool)
    .await?;
    Ok(Html(
        TeamPage {
            id,
            team,
            groups: group_by_position(squad),
        }
        .render()?,
    ))
}

/// Rows arrive sorted by position; split them into consecutive groups.
fn group_by_position(rows: Vec<SquadRow>) -> Vec<(String, Vec<SquadRow>)> {
    let mut groups: Vec<(String, Vec<SquadRow>)> = Vec::new();
    for row in rows {
        let pos = row.position.clone().unwrap_or_else(|| "Other".into());
        match groups.last_mut() {
            Some((p, list)) if *p == pos => list.push(row),
            _ => groups.push((pos, vec![row])),
        }
    }
    groups
}

// ----------------------------------------------------------------- refresh

#[derive(Template)]
#[template(path = "refresh.html")]
struct RefreshFragment {
    message: String,
}

/// Queue a refresh. Requires the HX-Request header (a cross-site form cannot
/// set it) and refuses while a request is already pending or running.
// shortcut: only queues the row; the Pi admin API call via Cloudflare Tunnel comes in Phase 4.
async fn refresh(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if !headers.contains_key("hx-request") {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let busy: bool = sqlx::query_scalar(
        "select exists (select 1 from refresh_request where status in ('pending', 'running'))",
    )
    .fetch_one(&st.pool)
    .await?;
    let message = if busy {
        "A refresh is already queued.".to_string()
    } else {
        let id: i64 = sqlx::query_scalar(
            "insert into refresh_request (status) values ('pending') returning id",
        )
        .fetch_one(&st.pool)
        .await?;
        format!("Refresh queued (#{id}).")
    };
    Ok(Html(RefreshFragment { message }.render()?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, pos: Option<&str>) -> SquadRow {
        SquadRow {
            name: name.into(),
            photo_url: None,
            shirt_number: None,
            position: pos.map(Into::into),
            age: None,
        }
    }

    #[test]
    fn groups_consecutive_positions() {
        let groups = group_by_position(vec![
            row("a", Some("Goalkeeper")),
            row("b", Some("Goalkeeper")),
            row("c", Some("Defender")),
            row("d", None),
        ]);
        let shape: Vec<(&str, usize)> = groups.iter().map(|(p, l)| (p.as_str(), l.len())).collect();
        assert_eq!(
            shape,
            vec![("Goalkeeper", 2), ("Defender", 1), ("Other", 1)]
        );
        assert!(group_by_position(vec![]).is_empty());
    }

    #[test]
    fn index_renders_and_escapes() {
        let html = IndexPage {
            leagues: vec![LeagueRow {
                code: "E0".into(),
                name: "<script>x</script>".into(),
                country: "England".into(),
                season_label: Some("2024/25".into()),
                teams: 20,
            }],
        }
        .render()
        .unwrap();
        assert!(html.contains("/league/E0"));
        assert!(html.contains("2024/25"));
        assert!(!html.contains("<script>x"));
    }

    #[test]
    fn team_page_renders_groups() {
        let html = TeamPage {
            id: 1,
            team: TeamRow {
                name: "Dundee Utd".into(),
                country: Some("Scotland".into()),
                founded: Some(1909),
                logo_url: None,
                venue_name: Some("Tannadice Park".into()),
                venue_city: Some("Dundee".into()),
                venue_capacity: Some(14223),
            },
            groups: group_by_position(vec![row("J. Walton", Some("Goalkeeper"))]),
        }
        .render()
        .unwrap();
        assert!(html.contains("Goalkeeper"));
        assert!(html.contains("J. Walton"));
        assert!(html.contains("Tannadice Park"));
    }

    #[test]
    fn empty_league_renders_placeholder() {
        let html = LeaguePage {
            competition: CompetitionRow {
                name: "Eredivisie".into(),
                country: "Netherlands".into(),
                season_id: None,
                season_label: None,
            },
            teams: vec![],
        }
        .render()
        .unwrap();
        assert!(html.contains("No teams loaded yet"));
    }
}
