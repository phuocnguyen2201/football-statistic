//! The 8 core leagues with their API-Football IDs.

use serde_json::Value;

pub struct League {
    /// Our canonical name (used for `--league`).
    pub name: &'static str,
    /// Country as API-Football spells it; checked against the `/leagues` response.
    pub country: &'static str,
    pub id: u32,
}

pub const LEAGUES: &[League] = &[
    League {
        name: "Premier League",
        country: "England",
        id: 39,
    },
    League {
        name: "Scottish Premiership",
        country: "Scotland",
        id: 179,
    },
    League {
        name: "La Liga",
        country: "Spain",
        id: 140,
    },
    League {
        name: "Bundesliga",
        country: "Germany",
        id: 78,
    },
    League {
        name: "Serie A",
        country: "Italy",
        id: 135,
    },
    League {
        name: "Ligue 1",
        country: "France",
        id: 61,
    },
    League {
        name: "Eredivisie",
        country: "Netherlands",
        id: 88,
    },
    League {
        name: "Primeira Liga",
        country: "Portugal",
        id: 94,
    },
];

/// Lowercase, ASCII alphanumerics only ("serie a" == "Serie A").
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

pub fn find_league(name: &str) -> Option<&'static League> {
    let n = normalize(name);
    LEAGUES.iter().find(|l| normalize(l.name) == n)
}

/// The season flagged `current`, else the latest `year`.
pub fn pick_current_season(seasons: &[Value]) -> Option<u16> {
    let year = |s: &Value| s.get("year").and_then(Value::as_u64).map(|y| y as u16);
    seasons
        .iter()
        .find(|s| s.get("current").and_then(Value::as_bool) == Some(true))
        .and_then(year)
        .or_else(|| seasons.iter().filter_map(year).max())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn find_league_ignores_case_and_spacing() {
        assert_eq!(find_league("serie a").unwrap().id, 135);
        assert_eq!(find_league("Scottish  Premiership").unwrap().id, 179);
        assert!(find_league("MLS").is_none());
    }

    #[test]
    fn current_season_prefers_flag_then_latest_year() {
        let flagged = vec![
            json!({"year": 2025, "current": false}),
            json!({"year": 2024, "current": true}),
        ];
        assert_eq!(pick_current_season(&flagged), Some(2024));
        let unflagged = vec![json!({"year": 2023}), json!({"year": 2025})];
        assert_eq!(pick_current_season(&unflagged), Some(2025));
        assert_eq!(pick_current_season(&[]), None);
    }
}
