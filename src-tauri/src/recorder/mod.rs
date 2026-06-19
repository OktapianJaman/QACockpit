//! macOS activity recorder.
//!
//! Thin orchestration layer: samples the active window + idle time every
//! [`SAMPLE_INTERVAL_SECS`] into an in-memory buffer, then [`Recorder::flush`]
//! merges those raw samples (via [`crate::core::sessions::merge_samples`]) and
//! persists the resulting blocks (via [`crate::db::insert_block`]). No shaping
//! logic lives here.

pub mod idle;
pub mod window;

use crate::core::sessions::merge_samples;
use crate::core::types::Sample;
use crate::db;
use chrono::Utc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Seconds between samples. Must match the `interval` passed to `merge_samples`.
pub const SAMPLE_INTERVAL_SECS: i64 = 5;
/// Idle threshold (seconds) above which a span is considered idle.
pub const IDLE_THRESHOLD_SECS: u64 = 180;

/// A start/stoppable sampling recorder, usable as Tauri managed state
/// (`Send + Sync` via `Arc`/atomics).
pub struct Recorder {
    buffer: Arc<Mutex<Vec<Sample>>>,
    running: Arc<AtomicBool>,
    db_path: String,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Recorder {
    pub fn new(db_path: String) -> Self {
        Recorder {
            buffer: Arc::new(Mutex::new(Vec::new())),
            running: Arc::new(AtomicBool::new(false)),
            db_path,
            handle: Mutex::new(None),
        }
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Start sampling in a background thread. No-op if already running.
    pub fn start(&self) {
        // Atomically claim the "running" flag; bail if it was already set.
        if self
            .running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let buffer = Arc::clone(&self.buffer);
        let running = Arc::clone(&self.running);

        let handle = std::thread::spawn(move || {
            while running.load(Ordering::SeqCst) {
                let (app, title) = window::current_window().unwrap_or_default();
                let sample = Sample {
                    at: Utc::now(),
                    app,
                    title,
                    idle_seconds: idle::idle_seconds(),
                };
                // Recover from a poisoned lock rather than panicking the loop.
                let mut buf = buffer.lock().unwrap_or_else(|e| e.into_inner());
                buf.push(sample);
                drop(buf);

                // Sleep the interval in small chunks so stop() takes effect promptly.
                let mut slept = 0u64;
                let total = SAMPLE_INTERVAL_SECS as u64 * 1000;
                while slept < total && running.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(200));
                    slept += 200;
                }
            }
        });

        if let Ok(mut h) = self.handle.lock() {
            *h = Some(handle);
        }
    }

    /// Stop sampling. The background thread exits on its next check.
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Ok(mut h) = self.handle.lock() {
            if let Some(handle) = h.take() {
                let _ = handle.join();
            }
        }
    }

    /// Drain the buffer, merge samples into blocks, persist them, and return
    /// the number of blocks written. Returns `Ok(0)` if the buffer was empty.
    pub fn flush(&self) -> anyhow::Result<usize> {
        let drained: Vec<Sample> = {
            let mut buf = self.buffer.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *buf)
        };

        if drained.is_empty() {
            return Ok(0);
        }

        let blocks = merge_samples(&drained, SAMPLE_INTERVAL_SECS, IDLE_THRESHOLD_SECS);
        let conn = db::open(&self.db_path)?;
        for block in &blocks {
            db::insert_block(&conn, block)?;
        }
        Ok(blocks.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone};

    fn at(offset_secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + offset_secs, 0).unwrap()
    }

    #[test]
    fn flush_writes_blocks_to_db() {
        // Fixed temp path so a fresh db::open in flush() reads the same file.
        let mut path = std::env::temp_dir();
        path.push("qacockpit_recorder_flush_test.sqlite");
        let path_str = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);

        let rec = Recorder::new(path_str.clone());

        // Push a few contiguous samples in the same window (5s apart) -> one block.
        {
            let mut buf = rec.buffer.lock().unwrap();
            for i in 0..3 {
                buf.push(Sample {
                    at: at(i * SAMPLE_INTERVAL_SECS),
                    app: "Chrome".into(),
                    title: "ABC-1 work".into(),
                    idle_seconds: 0,
                });
            }
        }

        let written = rec.flush().unwrap();
        assert!(written >= 1, "expected at least one block, got {written}");

        // Buffer is drained after flush.
        assert_eq!(rec.buffer.lock().unwrap().len(), 0);

        // Verify rows actually landed in the db.
        let conn = db::open(&path_str).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM activity_blocks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count as usize, written);

        // Empty buffer -> Ok(0).
        assert_eq!(rec.flush().unwrap(), 0);

        let _ = std::fs::remove_file(&path);
    }
}
