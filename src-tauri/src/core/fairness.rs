use serde::Serialize;

#[derive(Debug, PartialEq, Serialize)]
pub enum Fairness {
    Fair,
    UnderPointed,
    OverPointed,
}

#[derive(Debug, Serialize)]
pub struct Assessment {
    pub deserved: f64,
    pub assigned: f64,
    pub status: Fairness,
}

/// 1 hour worked = 2 points.
pub fn deserved_points(worked_secs: i64) -> f64 {
    (worked_secs as f64 / 3600.0) * 2.0
}

pub fn assess(worked_secs: i64, assigned: f64) -> Assessment {
    let deserved = deserved_points(worked_secs);
    let diff = deserved - assigned;
    let rel = if assigned > 0.0 {
        diff.abs() / assigned
    } else {
        1.0
    };
    let status = if diff.abs() <= 1.0 || rel <= 0.20 {
        Fairness::Fair
    } else if diff > 0.0 {
        Fairness::UnderPointed
    } else {
        Fairness::OverPointed
    };
    Assessment {
        deserved,
        assigned,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deserved_is_two_per_hour() {
        assert_eq!(deserved_points(21600), 12.0);
        assert_eq!(deserved_points(1800), 1.0);
    }
    #[test]
    fn flags_under_pointed() {
        let f = assess(21600, 3.0);
        assert_eq!(f.deserved, 12.0);
        assert_eq!(f.status, Fairness::UnderPointed);
    }
    #[test]
    fn flags_over_pointed() {
        assert_eq!(assess(7200, 8.0).status, Fairness::OverPointed);
    }
    #[test]
    fn flags_fair_when_close() {
        assert_eq!(assess(10800, 6.0).status, Fairness::Fair);
    }
}
