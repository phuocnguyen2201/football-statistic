//! Player season stats (API-Football; latest season loaded, 2024/25 today).

use sqlx::PgPool;

#[derive(sqlx::FromRow)]
pub struct PlayerStatRow {
    pub name: String,
    pub team_id: i32,
    pub team: String,
    pub season: String,
    pub appearances: Option<i16>,
    pub minutes: Option<i32>,
    pub goals: Option<i16>,
    pub assists: Option<i16>,
    pub yellow: Option<i16>,
    pub red: Option<i16>,
    pub rating: Option<String>,
}

const SELECT: &str = "
    select p.name, t.id as team_id, t.name as team, s.label as season,
           ps.appearances, ps.minutes, ps.goals, ps.assists, ps.yellow, ps.red,
           to_char(ps.rating, 'FM990.00') as rating
    from player_season_stats ps
    join player p on p.id = ps.player_id
    join team t on t.id = ps.team_id
    join season s on s.id = ps.season_id";

/// Every player with stats for the team's latest stats season, top scorers first.
pub async fn for_team(pool: &PgPool, team_id: i32) -> sqlx::Result<Vec<PlayerStatRow>> {
    sqlx::query_as(&format!(
        "{SELECT}
         where ps.team_id = $1
           and ps.season_id = (
               select x.season_id from player_season_stats x
               join season s2 on s2.id = x.season_id
               where x.team_id = $1
               order by s2.start_year desc limit 1)
         order by ps.goals desc nulls last, ps.assists desc nulls last,
                  ps.minutes desc nulls last, p.name"
    ))
    .bind(team_id)
    .fetch_all(pool)
    .await
}

pub struct Leaders {
    pub season: String,
    pub scorers: Vec<PlayerStatRow>,
    pub assists: Vec<PlayerStatRow>,
}

/// Top 10 scorers and assisters for the competition's latest stats season.
pub async fn leaders(pool: &PgPool, code: &str) -> sqlx::Result<Option<Leaders>> {
    let latest = "(select s2.id from season s2
                   join competition c on c.id = s2.competition_id
                   where c.code = $1
                     and exists (select 1 from player_season_stats x where x.season_id = s2.id)
                   order by s2.start_year desc limit 1)";
    let top = |filter_order: &str| {
        format!("{SELECT} where ps.season_id = {latest} {filter_order} limit 10")
    };
    let scorers: Vec<PlayerStatRow> = sqlx::query_as(&top(
        "and ps.goals > 0 order by ps.goals desc, ps.assists desc nulls last, ps.minutes nulls last",
    ))
    .bind(code)
    .fetch_all(pool)
    .await?;
    let assists: Vec<PlayerStatRow> = sqlx::query_as(&top(
        "and ps.assists > 0 order by ps.assists desc, ps.goals desc nulls last, ps.minutes nulls last",
    ))
    .bind(code)
    .fetch_all(pool)
    .await?;
    let Some(season) = scorers
        .first()
        .or(assists.first())
        .map(|r| r.season.clone())
    else {
        return Ok(None);
    };
    Ok(Some(Leaders {
        season,
        scorers,
        assists,
    }))
}
