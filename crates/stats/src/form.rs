//! Form and streaks for one team. Matches are oriented to the team (`a` = the
//! team, `b` = the opponent) and passed newest first.

use crate::h2h::{Meeting, Outcome};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Form {
    /// Newest first.
    pub results: Vec<Outcome>,
    pub wins: usize,
    pub draws: usize,
    pub losses: usize,
    pub goals_for: u32,
    pub goals_against: u32,
}

impl Form {
    pub fn points(&self) -> usize {
        self.wins * 3 + self.draws
    }
}

/// The last `n` matches.
pub fn form(newest_first: &[Meeting], n: usize) -> Form {
    let recent = &newest_first[..n.min(newest_first.len())];
    let count = |o: Outcome| recent.iter().filter(|m| m.outcome() == o).count();
    Form {
        results: recent.iter().map(Meeting::outcome).collect(),
        wins: count(Outcome::AWin),
        draws: count(Outcome::Draw),
        losses: count(Outcome::BWin),
        goals_for: recent.iter().map(|m| m.a.goals.max(0) as u32).sum(),
        goals_against: recent.iter().map(|m| m.b.goals.max(0) as u32).sum(),
    }
}

/// Current streaks: how many of the most recent matches in a row satisfy each
/// condition (0 if the latest match breaks it).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Streaks {
    pub wins: usize,
    pub unbeaten: usize,
    pub draws: usize,
    pub winless: usize,
    pub losses: usize,
    pub scored: usize,
    pub failed_to_score: usize,
    pub clean_sheets: usize,
    pub conceded: usize,
    pub btts: usize,
    pub over_2_5: usize,
    pub under_2_5: usize,
}

pub fn streaks(newest_first: &[Meeting]) -> Streaks {
    let run = |f: &dyn Fn(&Meeting) -> bool| newest_first.iter().take_while(|m| f(m)).count();
    Streaks {
        wins: run(&|m| m.outcome() == Outcome::AWin),
        unbeaten: run(&|m| m.outcome() != Outcome::BWin),
        draws: run(&|m| m.outcome() == Outcome::Draw),
        winless: run(&|m| m.outcome() != Outcome::AWin),
        losses: run(&|m| m.outcome() == Outcome::BWin),
        scored: run(&|m| m.a.goals > 0),
        failed_to_score: run(&|m| m.a.goals == 0),
        clean_sheets: run(&|m| m.b.goals == 0),
        conceded: run(&|m| m.b.goals > 0),
        btts: run(&|m| m.btts()),
        over_2_5: run(&|m| m.total_goals() > 2),
        under_2_5: run(&|m| m.total_goals() <= 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::h2h::Side;

    fn m(gf: i16, ga: i16) -> Meeting {
        Meeting {
            a: Side {
                goals: gf,
                ..Default::default()
            },
            b: Side {
                goals: ga,
                ..Default::default()
            },
        }
    }

    #[test]
    fn form_counts_last_n_only() {
        // Newest first: W 2-0, D 1-1, L 0-1, W 3-2 (outside n = 3).
        let f = form(&[m(2, 0), m(1, 1), m(0, 1), m(3, 2)], 3);
        assert_eq!(f.results, vec![Outcome::AWin, Outcome::Draw, Outcome::BWin]);
        assert_eq!((f.wins, f.draws, f.losses), (1, 1, 1));
        assert_eq!((f.goals_for, f.goals_against), (3, 2));
        assert_eq!(f.points(), 4);
    }

    #[test]
    fn form_with_fewer_matches_than_n() {
        let f = form(&[m(1, 0)], 5);
        assert_eq!(f.results.len(), 1);
        assert_eq!(form(&[], 5), Form::default());
    }

    #[test]
    fn streaks_count_from_latest_match() {
        // Newest first: W 2-1, W 1-0, D 2-2, L 0-3.
        let s = streaks(&[m(2, 1), m(1, 0), m(2, 2), m(0, 3)]);
        assert_eq!(s.wins, 2);
        assert_eq!(s.unbeaten, 3);
        assert_eq!(s.winless, 0);
        assert_eq!(s.losses, 0);
        assert_eq!(s.scored, 3);
        assert_eq!(s.clean_sheets, 0, "latest match conceded");
        assert_eq!(s.conceded, 1, "1-0 broke the run");
        assert_eq!(s.btts, 1);
        assert_eq!(s.over_2_5, 1);
        assert_eq!(s.under_2_5, 0);
    }

    #[test]
    fn streaks_of_nothing_are_zero() {
        assert_eq!(streaks(&[]), Streaks::default());
        let s = streaks(&[m(0, 0)]);
        assert_eq!(
            (s.draws, s.failed_to_score, s.clean_sheets, s.under_2_5),
            (1, 1, 1, 1)
        );
        assert_eq!((s.wins, s.scored, s.btts), (0, 0, 0));
    }
}
