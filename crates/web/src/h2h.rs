//! `/h2h?a=<team id>&b=<team id>`: pick two teams, show every finished
//! meeting in the database and the head-to-head summary.

use std::sync::Arc;

use askama::Template;
use axum::extract::{Query, State};
use axum::response::Html;
use serde::Deserialize;
use stats::h2h::{summarize, Meeting, Outcome, Side};

use super::{AppError, AppState, Page};

#[derive(Deserialize)]
pub struct Params {
    a: Option<String>,
    b: Option<String>,
}

#[derive(sqlx::FromRow)]
pub struct TeamOption {
    id: i32,
    name: String,
    league: String,
}

#[derive(sqlx::FromRow)]
struct MatchRow {
    date: String,
    competition: String,
    season: String,
    home_team_id: i32,
    ft_home: i16,
    ft_away: i16,
    ht_home: Option<i16>,
    ht_away: Option<i16>,
    home_shots: Option<i16>,
    away_shots: Option<i16>,
    home_sot: Option<i16>,
    away_sot: Option<i16>,
    home_corners: Option<i16>,
    away_corners: Option<i16>,
    home_yellow: Option<i16>,
    away_yellow: Option<i16>,
    home_red: Option<i16>,
    away_red: Option<i16>,
}

struct MatchView {
    date: String,
    competition: String,
    home: String,
    away: String,
    score: String,
    ht: String,
    corners: Option<String>,
    cards: Option<String>,
    /// From team A's point of view: "W", "D" or "L".
    result: &'static str,
}

struct StatView {
    label: &'static str,
    a: String,
    total: String,
    b: String,
    sample: usize,
}

struct MarketView {
    label: &'static str,
    count: usize,
    pct: String,
}

struct H2hResult {
    a_name: String,
    b_name: String,
    played: usize,
    a_wins: usize,
    draws: usize,
    b_wins: usize,
    a_win_pct: String,
    draw_pct: String,
    b_win_pct: String,
    a_goals: u32,
    b_goals: u32,
    avg_goals: String,
    markets: Vec<MarketView>,
    stats: Vec<StatView>,
    matches: Vec<MatchView>,
    show_corners: bool,
    show_cards: bool,
}

#[derive(Template)]
#[template(path = "h2h.html")]
struct H2hPage {
    groups: Vec<(String, Vec<TeamOption>)>,
    a_id: i32,
    b_id: i32,
    error: Option<&'static str>,
    result: Option<H2hResult>,
}

pub async fn page(State(st): State<Arc<AppState>>, Query(p): Query<Params>) -> Page {
    // A team is listed under the league of its latest loaded season.
    let teams = sqlx::query_as::<_, TeamOption>(
        "select id, name, league from (
             select distinct on (t.id) t.id, t.name, c.name as league, c.id as competition_id
             from team t
             join team_season ts on ts.team_id = t.id
             join season s on s.id = ts.season_id
             join competition c on c.id = s.competition_id
             order by t.id, s.start_year desc, c.id  -- domestic league (lower id) wins ties with CL
         ) x
         order by competition_id, name",
    )
    .fetch_all(&st.pool)
    .await?;

    let parse = |v: &Option<String>| v.as_deref().and_then(|s| s.parse::<i32>().ok());
    let (a, b) = (parse(&p.a), parse(&p.b));
    let mut error = None;
    let mut result = None;
    if let (Some(a), Some(b)) = (a, b) {
        if a == b {
            error = Some("Pick two different teams.");
        } else {
            let name = |id: i32| teams.iter().find(|t| t.id == id).map(|t| t.name.clone());
            let (Some(a_name), Some(b_name)) = (name(a), name(b)) else {
                return Err(AppError::NotFound);
            };
            let rows = sqlx::query_as::<_, MatchRow>(
                "select to_char(m.kickoff_utc at time zone 'UTC', 'YYYY-MM-DD') as date,
                        c.name as competition, s.label as season, m.home_team_id,
                        m.ft_home, m.ft_away, m.ht_home, m.ht_away,
                        ms.home_shots, ms.away_shots, ms.home_sot, ms.away_sot,
                        ms.home_corners, ms.away_corners, ms.home_yellow, ms.away_yellow,
                        ms.home_red, ms.away_red
                 from match m
                 join season s on s.id = m.season_id
                 join competition c on c.id = s.competition_id
                 left join match_stats ms on ms.match_id = m.id
                 where m.status = 'finished' and m.ft_home is not null and m.ft_away is not null
                   and ((m.home_team_id = $1 and m.away_team_id = $2)
                     or (m.home_team_id = $2 and m.away_team_id = $1))
                 order by m.kickoff_utc desc",
            )
            .bind(a)
            .bind(b)
            .fetch_all(&st.pool)
            .await?;
            result = Some(build(a, a_name, b_name, &rows));
        }
    }

    Ok(Html(
        H2hPage {
            groups: group_by_league(teams),
            a_id: a.unwrap_or(0),
            b_id: b.unwrap_or(0),
            error,
            result,
        }
        .render()?,
    ))
}

fn group_by_league(teams: Vec<TeamOption>) -> Vec<(String, Vec<TeamOption>)> {
    let mut groups: Vec<(String, Vec<TeamOption>)> = Vec::new();
    for t in teams {
        match groups.last_mut() {
            Some((league, list)) if *league == t.league => list.push(t),
            _ => groups.push((t.league.clone(), vec![t])),
        }
    }
    groups
}

/// Orient a stored match (home/away) to team A's point of view.
fn meeting(row: &MatchRow, a_id: i32) -> Meeting {
    let home = Side {
        goals: row.ft_home,
        shots: row.home_shots,
        shots_on_target: row.home_sot,
        corners: row.home_corners,
        yellow: row.home_yellow,
        red: row.home_red,
    };
    let away = Side {
        goals: row.ft_away,
        shots: row.away_shots,
        shots_on_target: row.away_sot,
        corners: row.away_corners,
        yellow: row.away_yellow,
        red: row.away_red,
    };
    if row.home_team_id == a_id {
        Meeting { a: home, b: away }
    } else {
        Meeting { a: away, b: home }
    }
}

fn pair(home: Option<i16>, away: Option<i16>) -> Option<String> {
    Some(format!("{}–{}", home?, away?))
}

fn build(a_id: i32, a_name: String, b_name: String, rows: &[MatchRow]) -> H2hResult {
    let meetings: Vec<Meeting> = rows.iter().map(|r| meeting(r, a_id)).collect();
    let s = summarize(&meetings);
    let pct = |n: usize| format!("{:.0}%", s.pct(n));

    let matches: Vec<MatchView> = rows
        .iter()
        .zip(&meetings)
        .map(|(r, m)| {
            let a_home = r.home_team_id == a_id;
            let (home, away) = if a_home {
                (a_name.clone(), b_name.clone())
            } else {
                (b_name.clone(), a_name.clone())
            };
            let cards = match (r.home_yellow, r.away_yellow) {
                (Some(hy), Some(ay)) => {
                    let reds = pair(r.home_red, r.away_red)
                        .filter(|_| r.home_red.unwrap_or(0) + r.away_red.unwrap_or(0) > 0)
                        .map(|p| format!(" · R {p}"))
                        .unwrap_or_default();
                    Some(format!("Y {hy}–{ay}{reds}"))
                }
                _ => None,
            };
            MatchView {
                date: r.date.clone(),
                competition: format!("{} {}", r.competition, r.season),
                home,
                away,
                score: format!("{}–{}", r.ft_home, r.ft_away),
                ht: pair(r.ht_home, r.ht_away).unwrap_or_else(|| "–".into()),
                corners: pair(r.home_corners, r.away_corners),
                cards,
                result: match m.outcome() {
                    Outcome::AWin => "W",
                    Outcome::Draw => "D",
                    Outcome::BWin => "L",
                },
            }
        })
        .collect();

    H2hResult {
        played: s.played,
        a_wins: s.a_wins,
        draws: s.draws,
        b_wins: s.b_wins,
        a_win_pct: pct(s.a_wins),
        draw_pct: pct(s.draws),
        b_win_pct: pct(s.b_wins),
        a_goals: s.a_goals,
        b_goals: s.b_goals,
        avg_goals: format!("{:.2}", s.avg_goals()),
        markets: [
            ("Both teams scored", s.btts),
            ("Over 1.5 goals", s.over_1_5),
            ("Over 2.5 goals", s.over_2_5),
            ("Over 3.5 goals", s.over_3_5),
        ]
        .into_iter()
        .map(|(label, count)| MarketView {
            label,
            count,
            pct: pct(count),
        })
        .collect(),
        stats: s
            .stats
            .iter()
            .map(|x| StatView {
                label: x.label,
                a: format!("{:.1}", x.a),
                total: format!("{:.1}", x.total),
                b: format!("{:.1}", x.b),
                sample: x.sample,
            })
            .collect(),
        show_corners: matches.iter().any(|m| m.corners.is_some()),
        show_cards: matches.iter().any(|m| m.cards.is_some()),
        matches,
        a_name,
        b_name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(home: i32, ft: (i16, i16), corners: Option<(i16, i16)>) -> MatchRow {
        MatchRow {
            date: "2024-01-01".into(),
            competition: "Premier League".into(),
            season: "2023/24".into(),
            home_team_id: home,
            ft_home: ft.0,
            ft_away: ft.1,
            ht_home: Some(0),
            ht_away: Some(0),
            home_shots: None,
            away_shots: None,
            home_sot: None,
            away_sot: None,
            home_corners: corners.map(|c| c.0),
            away_corners: corners.map(|c| c.1),
            home_yellow: None,
            away_yellow: None,
            home_red: None,
            away_red: None,
        }
    }

    #[test]
    fn orients_meetings_to_team_a() {
        // A (id 1) won 2-1 at home, then won 0-3 away.
        let r = build(
            1,
            "A".into(),
            "B".into(),
            &[row(1, (2, 1), Some((7, 3))), row(2, (0, 3), Some((5, 4)))],
        );
        assert_eq!((r.a_wins, r.draws, r.b_wins), (2, 0, 0));
        assert_eq!((r.a_goals, r.b_goals), (5, 1));
        assert_eq!(r.matches[1].home, "B");
        assert_eq!(r.matches[1].result, "W");
        // Corners averaged from A's side: (7 + 4) / 2 for A, (3 + 5) / 2 for B.
        let corners = r.stats.iter().find(|s| s.label == "Corners").unwrap();
        assert_eq!((corners.a.as_str(), corners.b.as_str()), ("5.5", "4.0"));
        assert!(r.show_corners);
        assert!(!r.show_cards);
    }

    #[test]
    fn no_meetings_renders_empty_state() {
        let r = build(1, "A".into(), "B".into(), &[]);
        assert_eq!(r.played, 0);
        let html = H2hPage {
            groups: vec![],
            a_id: 1,
            b_id: 2,
            error: None,
            result: Some(r),
        }
        .render()
        .unwrap();
        assert!(html.contains("No meetings"));
    }

    #[test]
    fn page_marks_selected_teams() {
        let groups = group_by_league(vec![
            TeamOption {
                id: 33,
                name: "Manchester United".into(),
                league: "Premier League".into(),
            },
            TeamOption {
                id: 247,
                name: "Celtic".into(),
                league: "Scottish Premiership".into(),
            },
        ]);
        assert_eq!(groups.len(), 2);
        let html = H2hPage {
            groups,
            a_id: 33,
            b_id: 0,
            error: None,
            result: None,
        }
        .render()
        .unwrap();
        assert!(html.contains(r#"<option value="33" selected>"#));
        assert!(html.contains(r#"<optgroup label="Scottish Premiership">"#));
    }
}
