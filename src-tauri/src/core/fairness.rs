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
    #[test]
    fn boundary_abs_diff_exactly_one_is_fair() {
        // 12600s -> deserved 7.0; assigned 6.0 -> |diff| == 1.0
        assert_eq!(assess(12600, 6.0).status, Fairness::Fair);
    }
    #[test]
    fn boundary_relative_exactly_twenty_percent_is_fair() {
        // 21600s -> deserved 12.0; assigned 10.0 -> rel == 0.20
        assert_eq!(assess(21600, 10.0).status, Fairness::Fair);
    }
    #[test]
    fn assigned_zero_with_work_is_under_pointed() {
        assert_eq!(assess(3600, 0.0).status, Fairness::UnderPointed);
    }
    #[test]
    fn assigned_zero_no_work_is_fair() {
        assert_eq!(assess(0, 0.0).status, Fairness::Fair);
    }
}
