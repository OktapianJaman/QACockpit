use crate::core::types::{ActivityBlock, Sample};

/// Merge raw samples into activity blocks.
/// `interval` = expected seconds between samples; a gap > 2*interval closes a block.
/// `idle_threshold` = idle_seconds at/above which a sample is considered idle.
pub fn merge_samples(samples: &[Sample], interval: i64, idle_threshold: u64) -> Vec<ActivityBlock> {
    let mut blocks: Vec<ActivityBlock> = Vec::new();
    for sm in samples {
        let idle = sm.idle_seconds >= idle_threshold;
        let same = blocks.last().map_or(false, |b| {
            b.app == sm.app
                && b.title == sm.title
                && b.is_idle == idle
                && (sm.at - b.end).num_seconds() <= 2 * interval
        });
        if same {
            blocks.last_mut().unwrap().end = sm.at;
        } else {
            blocks.push(ActivityBlock {
                app: sm.app.clone(),
                title: sm.title.clone(),
                start: sm.at,
                end: sm.at,
                is_idle: idle,
            });
        }
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::Sample;
    use chrono::{TimeZone, Utc};

    fn s(secs: i64, app: &str, title: &str, idle: u64) -> Sample {
        Sample {
            at: Utc.timestamp_opt(secs, 0).unwrap(),
            app: app.into(),
            title: title.into(),
            idle_seconds: idle,
        }
    }

    #[test]
    fn merges_same_window_into_one_block() {
        let samples = vec![
            s(0, "VS Code", "login_test.dart", 0),
            s(5, "VS Code", "login_test.dart", 0),
            s(10, "VS Code", "login_test.dart", 0),
        ];
        let blocks = merge_samples(&samples, 5, 180);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].duration_secs(), 10);
        assert!(!blocks[0].is_idle);
    }

    #[test]
    fn splits_when_window_changes() {
        let samples = vec![s(0, "VS Code", "a", 0), s(5, "Chrome", "JIRA-1", 0)];
        assert_eq!(merge_samples(&samples, 5, 180).len(), 2);
    }

    #[test]
    fn marks_idle_block() {
        let samples = vec![s(0, "VS Code", "a", 0), s(5, "VS Code", "a", 200)];
        let blocks = merge_samples(&samples, 5, 180);
        assert!(blocks.iter().any(|b| b.is_idle));
    }

    #[test]
    fn gap_larger_than_two_intervals_splits() {
        let samples = vec![s(0, "VS Code", "a", 0), s(100, "VS Code", "a", 0)]; // gap 100 > 2*5
        assert_eq!(merge_samples(&samples, 5, 180).len(), 2);
    }
}
