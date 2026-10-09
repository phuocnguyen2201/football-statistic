//! Team page stats: form, streaks, this season's goals markets (overall /
//! home / away) and recent matches. Rows come newest first, all competitions.

use stats::form::{form, streaks};
use stats::h2h::{summarize, Meeting, Summary};

use crate::matches::{meeting, result_letter, MatchRow};

pub struct TeamStats {
    /// Last five results, newest first: "W" / "D" / "L".
    pub form: Vec<&'static str>,
    pub form_summary: String,
    /// Current runs of two or more matches.
    pub streaks: Vec<(&'static str, usize)>,
    pub season: String,
    pub markets: Vec<MarketRow>,
    pub recent: Vec<RecentMatch>,
}

pub struct MarketRow {
    pub label: &'static str,
    pub all: String,
    pub home: String,
    pub away: String,
}

pub struct RecentMatch {
    pub date: String,
    pub competition: String,
    /// "H" or "A".
    pub venue: &'static str,
    pub opponent_id: i32,
    pub opponent: String,
    /// Home–away score as played.
    pub score: String,
    pub result: &'static str,
}

/// Average stats shown as "for / against" when any match has them.
const STAT_ROWS: &[(&str, &str)] = &[
    ("Shots", "Shots for / against"),
    ("Corners", "Corners for / against"),
    ("Yellow cards", "Yellow cards for / against"),
];

pub fn build(team_id: i32, rows: &[MatchRow]) -> Option<TeamStats> {
    let latest = rows.iter().map(|r| r.start_year).max()?;
    let all: Vec<Meeting> = rows.iter().map(|r| meeting(r, team_id)).collect();

    let f = form(&all, 5);
    let s = streaks(&all);
    let streak_list = [
        ("Wins", s.wins),
        ("Unbeaten", s.unbeaten),
        ("Draws", s.draws),
        ("Without a win", s.winless),
        ("Losses", s.losses),
        ("Scored in", s.scored),
        ("Failed to score in", s.failed_to_score),
        ("Clean sheets", s.clean_sheets),
        ("Conceded in", s.conceded),
        ("Both teams scored", s.btts),
        ("Over 2.5 goals", s.over_2_5),
        ("Under 2.5 goals", s.under_2_5),
    ]
    .into_iter()
    .filter(|(_, n)| *n >= 2)
    .collect();

    // This season (all competitions), split by venue.
    let season_rows: Vec<&MatchRow> = rows.iter().filter(|r| r.start_year == latest).collect();
    let subset = |keep: &dyn Fn(&MatchRow) -> bool| -> Summary {
        let ms: Vec<Meeting> = season_rows
            .iter()
            .filter(|r| keep(r))
            .map(|r| meeting(r, team_id))
            .collect();
        summarize(&ms)
    };
    let cols = [
        subset(&|_| true),
        subset(&|r| r.home_team_id == team_id),
        subset(&|r| r.away_team_id == team_id),
    ];

    Some(TeamStats {
        form: all.iter().take(5).map(result_letter).collect(),
        form_summary: format!(
            "{}W {}D {}L · goals {}–{}",
            f.wins, f.draws, f.losses, f.goals_for, f.goals_against
        ),
        streaks: streak_list,
        season: season_rows
            .first()
            .map(|r| r.season.clone())
            .unwrap_or_default(),
        markets: markets(&cols),
        recent: rows
            .iter()
            .zip(&all)
            .take(10)
            .map(|(r, m)| {
                let home = r.home_team_id == team_id;
                RecentMatch {
                    date: r.date.clone(),
                    competition: r.competition.clone(),
                    venue: if home { "H" } else { "A" },
                    opponent_id: if home { r.away_team_id } else { r.home_team_id },
                    opponent: if home {
                        r.away_name.clone()
                    } else {
                        r.home_name.clone()
                    },
                    score: format!("{}–{}", r.ft_home, r.ft_away),
                    result: result_letter(m),
                }
            })
            .collect(),
    })
}

fn markets(cols: &[Summary; 3]) -> Vec<MarketRow> {
    let row = |label: &'static str, f: &dyn Fn(&Summary) -> String| MarketRow {
        label,
        all: f(&cols[0]),
        home: f(&cols[1]),
        away: f(&cols[2]),
    };
    let pct = |n: fn(&Summary) -> usize| {
        move |s: &Summary| {
            if s.played == 0 {
                "–".into()
            } else {
                format!("{:.0}%", s.pct(n(s)))
            }
        }
    };
    let per_match = |n: fn(&Summary) -> u32| {
        move |s: &Summary| {
            if s.played == 0 {
                "–".into()
            } else {
                format!("{:.2}", f64::from(n(s)) / s.played as f64)
            }
        }
    };

    let mut out = vec![
        row("Matches", &|s| s.played.to_string()),
        row("Won – drawn – lost", &|s| {
            format!("{}–{}–{}", s.a_wins, s.draws, s.b_wins)
        }),
        row("Goals scored per match", &per_match(|s| s.a_goals)),
        row("Goals conceded per match", &per_match(|s| s.b_goals)),
        row("Both teams scored", &pct(|s| s.btts)),
        row("Over 1.5 goals", &pct(|s| s.over_1_5)),
        row("Over 2.5 goals", &pct(|s| s.over_2_5)),
        row("Over 3.5 goals", &pct(|s| s.over_3_5)),
        row("Clean sheets", &pct(|s| s.a_clean_sheets)),
        row("Failed to score", &pct(|s| s.a_failed_to_score)),
    ];
    for (stat, label) in STAT_ROWS {
        if cols
            .iter()
            .all(|c| c.stats.iter().all(|x| x.label != *stat))
        {
            continue;
        }
        out.push(row(label, &|s| {
            s.stats
                .iter()
                .find(|x| x.label == *stat)
                .map_or_else(|| "–".into(), |x| format!("{:.1} / {:.1}", x.a, x.b))
        }));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matches::sample_row;

    fn row(
        year: i16,
        home: i32,
        away: i32,
        ft: (i16, i16),
        corners: Option<(i16, i16)>,
    ) -> MatchRow {
        let mut r = sample_row(home, away, ft, corners);
        r.start_year = year;
        r.season = format!("{year}/{:02}", (year + 1) % 100);
        r
    }

    #[test]
    fn no_matches_no_stats() {
        assert!(build(1, &[]).is_none());
    }

    #[test]
    fn form_streaks_markets_and_recent() {
        // Team 1, newest first: W 2-0 home, W 0-1 away, D 1-1 home, last season L 0-2 away.
        let rows = vec![
            row(2026, 1, 2, (2, 0), Some((8, 2))),
            row(2026, 3, 1, (0, 1), None),
            row(2026, 1, 4, (1, 1), Some((6, 4))),
            row(2025, 5, 1, (2, 0), None),
        ];
        let t = build(1, &rows).unwrap();
        assert_eq!(t.form, vec!["W", "W", "D", "L"]);
        assert_eq!(t.form_summary, "2W 1D 1L · goals 4–3");
        assert!(t.streaks.contains(&("Wins", 2)));
        assert!(t.streaks.contains(&("Unbeaten", 3)));
        assert!(t.streaks.contains(&("Clean sheets", 2)));
        assert_eq!(t.season, "2026/27");

        let get = |label: &str| t.markets.iter().find(|m| m.label == label).unwrap();
        // This season only: 3 matches, 2 at home, 1 away.
        assert_eq!(
            (
                get("Matches").all.as_str(),
                get("Matches").home.as_str(),
                get("Matches").away.as_str()
            ),
            ("3", "2", "1")
        );
        assert_eq!(get("Won – drawn – lost").all, "2–1–0");
        assert_eq!(get("Clean sheets").all, "67%");
        assert_eq!(get("Goals scored per match").home, "1.50");
        // Corners exist only in the two home matches: (8 + 6) / 2 for, (2 + 4) / 2 against.
        let corners = get("Corners for / against");
        assert_eq!(corners.home, "7.0 / 3.0");
        assert_eq!(corners.away, "–");
        assert!(t.markets.iter().all(|m| m.label != "Shots for / against"));

        let r = &t.recent[1];
        assert_eq!(
            (r.venue, r.opponent_id, r.score.as_str(), r.result),
            ("A", 3, "0–1", "W")
        );
    }
}
