use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One raw sample taken by the recorder every ~5s.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sample {
    pub at: DateTime<Utc>,
    pub app: String,
    pub title: String,
    pub idle_seconds: u64,
}

/// A merged span of continuous work in one window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActivityBlock {
    pub app: String,
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub is_idle: bool,
}

impl ActivityBlock {
    pub fn duration_secs(&self) -> i64 {
        (self.end - self.start).num_seconds().max(0)
    }
}
