//! Finished matches as the pages read them, and orienting one to a team.

use stats::h2h::{Meeting, Outcome, Side};

/// Finished matches with names, competition and optional stats. Callers
/// append `and ...` filters and an `order by`.
pub const FINISHED: &str = "
    select to_char(m.kickoff_utc at time zone 'UTC', 'YYYY-MM-DD') as date,
           c.name as competition, s.label as season, s.start_year,
           m.home_team_id, m.away_team_id, th.name as home_name, ta.name as away_name,
           m.ft_home, m.ft_away, m.ht_home, m.ht_away,
           ms.home_shots, ms.away_shots, ms.home_sot, ms.away_sot,
           ms.home_corners, ms.away_corners, ms.home_yellow, ms.away_yellow,
           ms.home_red, ms.away_red
    from match m
    join season s on s.id = m.season_id
    join competition c on c.id = s.competition_id
    join team th on th.id = m.home_team_id
    join team ta on ta.id = m.away_team_id
    left join match_stats ms on ms.match_id = m.id
    where m.status = 'finished' and m.ft_home is not null and m.ft_away is not null";

#[derive(sqlx::FromRow, Clone)]
pub struct MatchRow {
    pub date: String,
    pub competition: String,
    pub season: String,
    pub start_year: i16,
    pub home_team_id: i32,
    pub away_team_id: i32,
    pub home_name: String,
    pub away_name: String,
    pub ft_home: i16,
    pub ft_away: i16,
    pub ht_home: Option<i16>,
    pub ht_away: Option<i16>,
    pub home_shots: Option<i16>,
    pub away_shots: Option<i16>,
    pub home_sot: Option<i16>,
    pub away_sot: Option<i16>,
    pub home_corners: Option<i16>,
    pub away_corners: Option<i16>,
    pub home_yellow: Option<i16>,
    pub away_yellow: Option<i16>,
    pub home_red: Option<i16>,
    pub away_red: Option<i16>,
}

/// Orient a stored match (home/away) to `team_id`'s point of view (`a`).
pub fn meeting(row: &MatchRow, team_id: i32) -> Meeting {
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
    if row.home_team_id == team_id {
        Meeting { a: home, b: away }
    } else {
        Meeting { a: away, b: home }
    }
}

pub fn pair(home: Option<i16>, away: Option<i16>) -> Option<String> {
    Some(format!("{}–{}", home?, away?))
}

/// "W" / "D" / "L" from the oriented team's point of view (also the CSS class).
pub fn result_letter(m: &Meeting) -> &'static str {
    letter(m.outcome())
}

pub fn letter(o: Outcome) -> &'static str {
    match o {
        Outcome::AWin => "W",
        Outcome::Draw => "D",
        Outcome::BWin => "L",
    }
}

#[cfg(test)]
pub fn sample_row(home: i32, away: i32, ft: (i16, i16), corners: Option<(i16, i16)>) -> MatchRow {
    MatchRow {
        date: "2024-01-01".into(),
        competition: "Premier League".into(),
        season: "2023/24".into(),
        start_year: 2023,
        home_team_id: home,
        away_team_id: away,
        home_name: format!("Team {home}"),
        away_name: format!("Team {away}"),
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
