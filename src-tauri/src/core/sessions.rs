use crate::core::types::{ActivityBlock, Sample};

/// Merge raw samples into activity blocks.
/// `interval` = expected seconds between samples; a gap > 2*interval closes a block.
/// `idle_threshold` = idle_seconds at/above which a sample is considered idle.
///
/// Duration model: a block's duration is `end - start`, i.e. the span between its
/// first and last sample. A window seen in only one sample therefore has duration 0,
/// and every block under-counts by up to `interval` seconds past its last sample.
/// This is an accepted, bounded approximation for v1 (negligible versus hour-scale
/// totals); we deliberately do NOT add `interval` to avoid over-counting.
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

    #[test]
    fn gap_at_exactly_two_intervals_merges() {
        let samples = vec![s(0, "VS Code", "a", 0), s(10, "VS Code", "a", 0)]; // gap 10 == 2*5
        assert_eq!(merge_samples(&samples, 5, 180).len(), 1);
    }

    #[test]
    fn gap_just_over_two_intervals_splits() {
        let samples = vec![s(0, "VS Code", "a", 0), s(11, "VS Code", "a", 0)]; // gap 11 > 2*5
        assert_eq!(merge_samples(&samples, 5, 180).len(), 2);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(merge_samples(&[], 5, 180).len(), 0);
    }

    #[test]
    fn single_sample_makes_one_zero_duration_block() {
        let blocks = merge_samples(&[s(0, "VS Code", "a", 0)], 5, 180);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].duration_secs(), 0);
    }

    #[test]
    fn idle_change_splits_same_window() {
        let samples = vec![
            s(0, "VS Code", "a", 0),
            s(5, "VS Code", "a", 200),
            s(10, "VS Code", "a", 0),
        ];
        let blocks = merge_samples(&samples, 5, 180);
        assert_eq!(blocks.len(), 3);
        assert!(blocks[1].is_idle);
    }
}
