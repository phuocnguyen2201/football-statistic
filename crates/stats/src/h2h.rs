//! Head-to-head summary between team A and team B.
//!
//! Each meeting is passed in already oriented from A's point of view
//! (`a` = team A's side whether A was home or away). Optional match stats
//! (shots, corners, cards) are averaged only over meetings where both sides
//! have the value, and reported with that sample size.

/// One team's numbers in one meeting.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Side {
    pub goals: i16,
    pub shots: Option<i16>,
    pub shots_on_target: Option<i16>,
    pub corners: Option<i16>,
    pub yellow: Option<i16>,
    pub red: Option<i16>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meeting {
    pub a: Side,
    pub b: Side,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    AWin,
    Draw,
    BWin,
}

impl Meeting {
    pub fn outcome(&self) -> Outcome {
        match self.a.goals.cmp(&self.b.goals) {
            std::cmp::Ordering::Greater => Outcome::AWin,
            std::cmp::Ordering::Equal => Outcome::Draw,
            std::cmp::Ordering::Less => Outcome::BWin,
        }
    }

    pub fn total_goals(&self) -> i16 {
        self.a.goals + self.b.goals
    }

    pub fn btts(&self) -> bool {
        self.a.goals > 0 && self.b.goals > 0
    }
}

/// Per-match average of one stat for A, B and both combined.
#[derive(Debug, Clone, PartialEq)]
pub struct StatAverage {
    pub label: &'static str,
    pub a: f64,
    pub b: f64,
    pub total: f64,
    /// Meetings that had this stat for both sides.
    pub sample: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub played: usize,
    pub a_wins: usize,
    pub draws: usize,
    pub b_wins: usize,
    pub a_goals: u32,
    pub b_goals: u32,
    pub btts: usize,
    pub over_1_5: usize,
    pub over_2_5: usize,
    pub over_3_5: usize,
    /// Only stats with at least one meeting of data, in a fixed order.
    pub stats: Vec<StatAverage>,
}

impl Summary {
    /// `count` as a percentage of meetings played (0 when none).
    pub fn pct(&self, count: usize) -> f64 {
        if self.played == 0 {
            0.0
        } else {
            count as f64 * 100.0 / self.played as f64
        }
    }

    pub fn avg_goals(&self) -> f64 {
        if self.played == 0 {
            0.0
        } else {
            f64::from(self.a_goals + self.b_goals) / self.played as f64
        }
    }
}

type Getter = fn(&Side) -> Option<i16>;

const STATS: &[(&str, Getter)] = &[
    ("Shots", |s| s.shots),
    ("Shots on target", |s| s.shots_on_target),
    ("Corners", |s| s.corners),
    ("Yellow cards", |s| s.yellow),
    ("Red cards", |s| s.red),
];

pub fn summarize(meetings: &[Meeting]) -> Summary {
    let count = |f: &dyn Fn(&Meeting) -> bool| meetings.iter().filter(|m| f(m)).count();
    Summary {
        played: meetings.len(),
        a_wins: count(&|m| m.outcome() == Outcome::AWin),
        draws: count(&|m| m.outcome() == Outcome::Draw),
        b_wins: count(&|m| m.outcome() == Outcome::BWin),
        a_goals: meetings.iter().map(|m| m.a.goals.max(0) as u32).sum(),
        b_goals: meetings.iter().map(|m| m.b.goals.max(0) as u32).sum(),
        btts: count(&|m| m.btts()),
        over_1_5: count(&|m| m.total_goals() > 1),
        over_2_5: count(&|m| m.total_goals() > 2),
        over_3_5: count(&|m| m.total_goals() > 3),
        stats: STATS
            .iter()
            .filter_map(|(label, get)| stat_average(label, *get, meetings))
            .collect(),
    }
}

fn stat_average(label: &'static str, get: Getter, meetings: &[Meeting]) -> Option<StatAverage> {
    let pairs: Vec<(f64, f64)> = meetings
        .iter()
        .filter_map(|m| Some((f64::from(get(&m.a)?), f64::from(get(&m.b)?))))
        .collect();
    if pairs.is_empty() {
        return None;
    }
    let n = pairs.len() as f64;
    let a = pairs.iter().map(|p| p.0).sum::<f64>() / n;
    let b = pairs.iter().map(|p| p.1).sum::<f64>() / n;
    Some(StatAverage {
        label,
        a,
        b,
        total: a + b,
        sample: pairs.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goals(a: i16, b: i16) -> Meeting {
        Meeting {
            a: Side {
                goals: a,
                ..Default::default()
            },
            b: Side {
                goals: b,
                ..Default::default()
            },
        }
    }

    fn with_corners(mut m: Meeting, a: i16, b: i16) -> Meeting {
        m.a.corners = Some(a);
        m.b.corners = Some(b);
        m
    }

    #[test]
    fn outcome_and_markets_per_meeting() {
        assert_eq!(goals(2, 1).outcome(), Outcome::AWin);
        assert_eq!(goals(0, 0).outcome(), Outcome::Draw);
        assert_eq!(goals(1, 3).outcome(), Outcome::BWin);
        assert!(goals(1, 1).btts());
        assert!(!goals(3, 0).btts());
        assert_eq!(goals(1, 3).total_goals(), 4);
    }

    #[test]
    fn summary_counts_results_goals_and_overs() {
        let s = summarize(&[goals(2, 1), goals(0, 0), goals(1, 3), goals(2, 2)]);
        assert_eq!(s.played, 4);
        assert_eq!((s.a_wins, s.draws, s.b_wins), (1, 2, 1));
        assert_eq!((s.a_goals, s.b_goals), (5, 6));
        assert_eq!(s.btts, 3);
        assert_eq!(s.over_1_5, 3);
        assert_eq!(s.over_2_5, 3);
        assert_eq!(s.over_3_5, 2);
        assert_eq!(s.avg_goals(), 2.75);
        assert_eq!(s.pct(s.btts), 75.0);
    }

    #[test]
    fn empty_history_is_all_zero() {
        let s = summarize(&[]);
        assert_eq!(s.played, 0);
        assert_eq!(s.avg_goals(), 0.0);
        assert_eq!(s.pct(0), 0.0);
        assert!(s.stats.is_empty());
    }

    #[test]
    fn stat_averages_skip_meetings_without_data() {
        let s = summarize(&[
            with_corners(goals(1, 0), 6, 2),
            with_corners(goals(0, 1), 4, 4),
            goals(2, 2), // no corner data: not in the sample
        ]);
        assert_eq!(s.stats.len(), 1, "only corners have data");
        let c = &s.stats[0];
        assert_eq!(c.label, "Corners");
        assert_eq!(c.sample, 2);
        assert_eq!((c.a, c.b, c.total), (5.0, 3.0, 8.0));
    }

    #[test]
    fn stat_needs_both_sides() {
        let mut m = goals(1, 1);
        m.a.yellow = Some(3);
        assert!(summarize(&[m]).stats.is_empty());
    }
}
