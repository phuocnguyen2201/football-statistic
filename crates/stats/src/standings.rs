//! League table from results.
//!
//! Sorted by points, goal difference, goals scored. Teams still level keep
//! the order the caller passed them in (pass them sorted by name).
// shortcut: no head-to-head tiebreak (used by La Liga, Serie A); add it if a level table matters.

use crate::h2h::Outcome;

/// One finished match. Pass them oldest first so `recent` reads naturally.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Played {
    pub home: i32,
    pub away: i32,
    pub home_goals: i16,
    pub away_goals: i16,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Row {
    pub team: i32,
    pub played: u32,
    pub won: u32,
    pub drawn: u32,
    pub lost: u32,
    pub goals_for: u32,
    pub goals_against: u32,
    pub points: u32,
    /// Last five results, newest first (team's point of view).
    pub recent: Vec<Outcome>,
}

impl Row {
    pub fn goal_difference(&self) -> i64 {
        i64::from(self.goals_for) - i64::from(self.goals_against)
    }
}

/// `teams`: every team in the league, so teams without a match still appear.
/// Results for teams not in `teams` are ignored.
pub fn table(teams: &[i32], results: &[Played]) -> Vec<Row> {
    let mut rows: Vec<Row> = teams
        .iter()
        .map(|&team| Row {
            team,
            ..Default::default()
        })
        .collect();
    for r in results {
        let (hg, ag) = (r.home_goals.max(0) as u32, r.away_goals.max(0) as u32);
        for (team, gf, ga) in [(r.home, hg, ag), (r.away, ag, hg)] {
            let Some(row) = rows.iter_mut().find(|row| row.team == team) else {
                continue;
            };
            let outcome = match gf.cmp(&ga) {
                std::cmp::Ordering::Greater => Outcome::AWin,
                std::cmp::Ordering::Equal => Outcome::Draw,
                std::cmp::Ordering::Less => Outcome::BWin,
            };
            row.played += 1;
            row.goals_for += gf;
            row.goals_against += ga;
            match outcome {
                Outcome::AWin => {
                    row.won += 1;
                    row.points += 3;
                }
                Outcome::Draw => {
                    row.drawn += 1;
                    row.points += 1;
                }
                Outcome::BWin => row.lost += 1,
            }
            row.recent.insert(0, outcome);
            row.recent.truncate(5);
        }
    }
    // Stable sort: ties beyond goals scored keep the caller's order.
    rows.sort_by(|a, b| {
        b.points
            .cmp(&a.points)
            .then(b.goal_difference().cmp(&a.goal_difference()))
            .then(b.goals_for.cmp(&a.goals_for))
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(home: i32, away: i32, hg: i16, ag: i16) -> Played {
        Played {
            home,
            away,
            home_goals: hg,
            away_goals: ag,
        }
    }

    #[test]
    fn points_goals_and_order() {
        // 1 beats 2, 2 draws 3, 3 beats 1 3-0.
        let t = table(&[1, 2, 3], &[p(1, 2, 2, 1), p(2, 3, 1, 1), p(3, 1, 3, 0)]);
        let order: Vec<i32> = t.iter().map(|r| r.team).collect();
        assert_eq!(order, vec![3, 1, 2]);
        let top = &t[0];
        assert_eq!((top.played, top.won, top.drawn, top.lost), (2, 1, 1, 0));
        assert_eq!((top.goals_for, top.goals_against, top.points), (4, 1, 4));
        assert_eq!(top.goal_difference(), 3);
        assert_eq!(t[1].goal_difference(), -2);
    }

    #[test]
    fn recent_is_newest_first_and_capped_at_five() {
        let results: Vec<Played> = (0..6)
            .map(|i| if i == 5 { p(1, 2, 0, 1) } else { p(1, 2, 1, 0) })
            .collect();
        let t = table(&[1, 2], &results);
        let one = t.iter().find(|r| r.team == 1).unwrap();
        assert_eq!(one.recent.len(), 5);
        assert_eq!(one.recent[0], Outcome::BWin, "latest result first");
        assert_eq!(one.recent[1], Outcome::AWin);
    }

    #[test]
    fn ties_keep_caller_order_and_idle_teams_listed() {
        let t = table(&[7, 5, 9], &[]);
        let order: Vec<i32> = t.iter().map(|r| r.team).collect();
        assert_eq!(order, vec![7, 5, 9]);
        assert!(t.iter().all(|r| r.played == 0));
    }

    #[test]
    fn goal_difference_then_goals_for_break_ties() {
        // 1 and 2 both win once: 1 by 1-0, 2 by 3-2 (same GD, more goals).
        let t = table(&[1, 2, 3, 4], &[p(1, 3, 1, 0), p(2, 4, 3, 2)]);
        assert_eq!(t[0].team, 2);
        assert_eq!(t[1].team, 1);
    }

    #[test]
    fn unknown_teams_are_ignored() {
        let t = table(&[1], &[p(1, 99, 2, 0)]);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].points, 3);
    }
}
