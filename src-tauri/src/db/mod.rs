use crate::core::matching::extract_ticket_key;
use crate::core::types::ActivityBlock;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Serialize;

/// A per-ticket test case stored in the local SQLite db.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TestCase {
    pub id: i64,
    pub ticket_key: String,
    pub title: String,
    pub steps: String,
    pub expected: String,
    pub status: String,
    pub notes: String,
    /// Why the last device run gave its verdict (esp. NOT-AUTO / fail). Empty when
    /// never run or passed.
    #[serde(default)]
    pub verdict_reason: String,
    /// Cached triage classification (auto|spec_drift|buildable|manual|unknown).
    /// Empty until the ticket has been triaged.
    #[serde(default)]
    pub triage_bucket: String,
}

/// Open (or create) the SQLite database at `path` and apply the schema.
/// Pass `":memory:"` for an in-memory database (used in tests).
pub fn open(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(include_str!("schema.sql"))?;
    // Migration for DBs created before `notes` existed: schema.sql only runs
    // CREATE TABLE IF NOT EXISTS, so an existing `test_cases` table won't gain
    // the column. ALTER ... ADD COLUMN errors if it already exists; swallow it.
    let _ = conn.execute("ALTER TABLE test_cases ADD COLUMN notes TEXT", []);
    // Migration: reason for the last device-run verdict (NOT-AUTO / fail detail).
    let _ = conn.execute("ALTER TABLE test_cases ADD COLUMN verdict_reason TEXT", []);
    // Migration: cached triage bucket (auto|spec_drift|buildable|manual|unknown)
    // so re-triaging a ticket doesn't re-call the AI classifier every time.
    let _ = conn.execute("ALTER TABLE test_cases ADD COLUMN triage_bucket TEXT", []);
    Ok(conn)
}

/// Insert one activity block, deriving and storing its Jira ticket_key from the title.
pub fn insert_block(conn: &Connection, block: &ActivityBlock) -> Result<()> {
    let ticket_key = extract_ticket_key(&block.title);
    // Store timestamps in LOCAL time so the stored date prefix (substr 1,10)
    // matches the user's local calendar day. The offset is preserved in the
    // RFC3339 string, so reading back as UTC still round-trips exactly.
    conn.execute(
        "INSERT INTO activity_blocks (app, title, start, end, is_idle, ticket_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            block.app,
            block.title,
            block.start.with_timezone(&chrono::Local).to_rfc3339(),
            block.end.with_timezone(&chrono::Local).to_rfc3339(),
            block.is_idle as i64,
            ticket_key,
        ],
    )?;
    Ok(())
}

fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

/// List all blocks whose `start` date (YYYY-MM-DD) equals `day`, ordered by start.
pub fn list_blocks_for_day(conn: &Connection, day: &str) -> Result<Vec<ActivityBlock>> {
    let mut stmt = conn.prepare(
        "SELECT app, title, start, end, is_idle FROM activity_blocks
         WHERE substr(start, 1, 10) = ?1
         ORDER BY start",
    )?;
    let rows = stmt.query_map([day], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;

    let mut blocks = Vec::new();
    for row in rows {
        let (app, title, start, end, is_idle) = row?;
        blocks.push(ActivityBlock {
            app,
            title,
            start: parse_ts(&start)?,
            end: parse_ts(&end)?,
            is_idle: is_idle != 0,
        });
    }
    Ok(blocks)
}

/// Recompute the `ticket_time` rollup for `day` from non-idle, keyed activity blocks.
/// Deletes existing rows for the day, then inserts one summed row per ticket_key.
pub fn recompute_ticket_time(conn: &Connection, day: &str) -> Result<()> {
    conn.execute("DELETE FROM ticket_time WHERE day = ?1", [day])?;

    // Read non-idle, keyed blocks for the day and sum durations in Rust
    // (reusing ActivityBlock::duration_secs) rather than relying on SQL date math.
    let mut stmt = conn.prepare(
        "SELECT ticket_key, start, end FROM activity_blocks
         WHERE substr(start, 1, 10) = ?1
           AND is_idle = 0
           AND ticket_key IS NOT NULL",
    )?;
    let rows = stmt.query_map([day], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut totals: std::collections::BTreeMap<String, i64> = std::collections::BTreeMap::new();
    for row in rows {
        let (ticket_key, start, end) = row?;
        let secs = (parse_ts(&end)? - parse_ts(&start)?).num_seconds().max(0);
        *totals.entry(ticket_key).or_insert(0) += secs;
    }

    let mut insert = conn.prepare(
        "INSERT INTO ticket_time (day, ticket_key, worked_secs) VALUES (?1, ?2, ?3)",
    )?;
    for (ticket_key, worked_secs) in totals {
        insert.execute(rusqlite::params![day, ticket_key, worked_secs])?;
    }
    Ok(())
}

/// Read the ticket_time rollup for `day` as (ticket_key, worked_secs) pairs.
pub fn get_ticket_time(conn: &Connection, day: &str) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT ticket_key, worked_secs FROM ticket_time WHERE day = ?1 ORDER BY ticket_key",
    )?;
    let rows = stmt.query_map([day], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// Read a single config value by key. Returns `Ok(None)` if the key is absent.
pub fn get_config(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM config WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, String>(0)?))
    } else {
        Ok(None)
    }
}

/// Upsert a single config value (INSERT OR REPLACE on key).
pub fn set_config(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO config (key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Upsert the note body for `day`.
pub fn set_note(conn: &Connection, day: &str, body: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO notes (day, body) VALUES (?1, ?2)",
        rusqlite::params![day, body],
    )?;
    Ok(())
}

/// Read the note body for `day`, or `Ok(None)` if none exists.
pub fn get_note(conn: &Connection, day: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT body FROM notes WHERE day = ?1")?;
    let mut rows = stmt.query([day])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, String>(0)?))
    } else {
        Ok(None)
    }
}

/// Set the `ticket_key` of a single activity block by its row id (manual correction).
pub fn set_block_ticket(conn: &Connection, block_id: i64, ticket_key: &str) -> Result<()> {
    let key: Option<&str> = if ticket_key.trim().is_empty() {
        None
    } else {
        Some(ticket_key)
    };
    conn.execute(
        "UPDATE activity_blocks SET ticket_key = ?1 WHERE id = ?2",
        rusqlite::params![key, block_id],
    )?;
    Ok(())
}

/// One logged QA action (a status move or a point set) for the daily summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QaActivity {
    pub ts: String,
    pub ticket_key: String,
    pub summary: String,
    pub kind: String,
    pub from_status: String,
    pub to_status: String,
    pub points: Option<f64>,
}

/// Record a QA action taken in the app. `from_status`/`to_status` are used for
/// `kind = "transition"`; `points` for `kind = "points"`.
#[allow(clippy::too_many_arguments)]
pub fn log_qa_activity(
    conn: &Connection,
    day: &str,
    ts: &str,
    ticket_key: &str,
    summary: &str,
    kind: &str,
    from_status: &str,
    to_status: &str,
    points: Option<f64>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO qa_activity
           (day, ts, ticket_key, summary, kind, from_status, to_status, points)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![day, ts, ticket_key, summary, kind, from_status, to_status, points],
    )?;
    Ok(())
}

/// List the QA actions logged on `day`, oldest first.
pub fn list_qa_activity_for_day(conn: &Connection, day: &str) -> Result<Vec<QaActivity>> {
    let mut stmt = conn.prepare(
        "SELECT ts, ticket_key, summary, kind, from_status, to_status, points
         FROM qa_activity WHERE day = ?1 ORDER BY ts ASC, id ASC",
    )?;
    let rows = stmt.query_map([day], |row| {
        Ok(QaActivity {
            ts: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            ticket_key: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            summary: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
            kind: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            from_status: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            to_status: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            points: row.get::<_, Option<f64>>(6)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Upsert an AI summary for `day` of the given `kind`.
pub fn set_ai_summary(conn: &Connection, day: &str, kind: &str, body: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO ai_summaries (day, kind, body, generated_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![day, kind, body, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Read the AI summary body for `day` of the given `kind`, or `Ok(None)`.
pub fn get_ai_summary(conn: &Connection, day: &str, kind: &str) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT body FROM ai_summaries WHERE day = ?1 AND kind = ?2")?;
    let mut rows = stmt.query([day, kind])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row.get::<_, String>(0)?))
    } else {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Test cases (per-ticket QA test cases)
// ---------------------------------------------------------------------------

/// Insert one test case for a ticket. Returns the new row id. `created_at` is
/// stamped with the local time in RFC3339.
pub fn add_test_case(
    conn: &Connection,
    ticket_key: &str,
    title: &str,
    steps: &str,
    expected: &str,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO test_cases (ticket_key, title, steps, expected, status, created_at)
         VALUES (?1, ?2, ?3, ?4, 'untested', ?5)",
        rusqlite::params![
            ticket_key,
            title,
            steps,
            expected,
            chrono::Local::now().to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Map a row of (id, ticket_key, title, steps, expected, status, notes, verdict_reason)
/// to a TestCase. Shared by list + single-row getters.
fn row_to_test_case(row: &rusqlite::Row) -> rusqlite::Result<TestCase> {
    Ok(TestCase {
        id: row.get(0)?,
        ticket_key: row.get(1)?,
        title: row.get(2)?,
        steps: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        expected: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
        status: row.get(5)?,
        notes: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        verdict_reason: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
        triage_bucket: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
    })
}

const TC_COLS: &str =
    "id, ticket_key, title, steps, expected, status, notes, verdict_reason, triage_bucket";

/// List all test cases for a ticket, ordered by id.
pub fn list_test_cases(conn: &Connection, ticket_key: &str) -> Result<Vec<TestCase>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TC_COLS} FROM test_cases WHERE ticket_key = ?1 ORDER BY id"
    ))?;
    let rows = stmt.query_map([ticket_key], row_to_test_case)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Fetch a single test case by id.
pub fn get_test_case(conn: &Connection, id: i64) -> Result<TestCase> {
    Ok(conn.query_row(
        &format!("SELECT {TC_COLS} FROM test_cases WHERE id = ?1"),
        [id],
        row_to_test_case,
    )?)
}

/// Record a device-run verdict: status ('passed'|'failed'|'not_auto'|'running') plus
/// the human reason (cleared on pass).
pub fn set_test_case_verdict(conn: &Connection, id: i64, status: &str, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE test_cases SET status = ?1, verdict_reason = ?2 WHERE id = ?3",
        rusqlite::params![status, reason, id],
    )?;
    Ok(())
}

/// Cache a triage classification bucket for a case (so re-triage is instant and
/// the AI classifier isn't re-called). Also persists the reason in verdict_reason.
pub fn set_triage(conn: &Connection, id: i64, bucket: &str, reason: &str) -> Result<()> {
    conn.execute(
        "UPDATE test_cases SET triage_bucket = ?1, verdict_reason = ?2 WHERE id = ?3",
        rusqlite::params![bucket, reason, id],
    )?;
    Ok(())
}

/// Update a single test case's run status (e.g. 'untested' | 'passed' | 'failed').
pub fn set_test_case_status(conn: &Connection, id: i64, status: &str) -> Result<()> {
    conn.execute(
        "UPDATE test_cases SET status = ?1 WHERE id = ?2",
        rusqlite::params![status, id],
    )?;
    Ok(())
}

/// Update a single test case's free-text notes (actual result / remark).
pub fn set_test_case_notes(conn: &Connection, id: i64, notes: &str) -> Result<()> {
    conn.execute(
        "UPDATE test_cases SET notes = ?1 WHERE id = ?2",
        rusqlite::params![notes, id],
    )?;
    Ok(())
}

/// Update a test case's title/steps/expected (manual edit).
pub fn update_test_case(
    conn: &Connection,
    id: i64,
    title: &str,
    steps: &str,
    expected: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE test_cases SET title = ?1, steps = ?2, expected = ?3 WHERE id = ?4",
        rusqlite::params![title, steps, expected, id],
    )?;
    Ok(())
}

/// Delete a test case by id.
pub fn delete_test_case(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM test_cases WHERE id = ?1", [id])?;
    Ok(())
}

/// One persisted PR follow-up chat message. `images` are data: URLs attached to
/// the message (empty when none).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PrChatMsg {
    pub role: String,
    pub content: String,
    pub images: Vec<String>,
}

/// Upsert the cached AI summary for a PR.
pub fn set_pr_summary(conn: &Connection, repo: &str, number: i64, body: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO pr_summaries (repo, number, body, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(repo, number) DO UPDATE SET body = excluded.body, updated_at = excluded.updated_at",
        rusqlite::params![repo, number, body, chrono::Local::now().to_rfc3339()],
    )?;
    Ok(())
}

/// Read the cached AI summary for a PR, if any.
pub fn get_pr_summary(conn: &Connection, repo: &str, number: i64) -> Result<Option<String>> {
    let mut stmt =
        conn.prepare("SELECT body FROM pr_summaries WHERE repo = ?1 AND number = ?2")?;
    let mut rows = stmt.query(rusqlite::params![repo, number])?;
    if let Some(row) = rows.next()? {
        Ok(row.get::<_, Option<String>>(0)?)
    } else {
        Ok(None)
    }
}

/// Append one message to a PR's persisted chat.
pub fn add_pr_chat(
    conn: &Connection,
    repo: &str,
    number: i64,
    role: &str,
    content: &str,
    images: &[String],
) -> Result<()> {
    let images_json = serde_json::to_string(images).unwrap_or_else(|_| "[]".to_string());
    conn.execute(
        "INSERT INTO pr_chat (repo, number, role, content, images, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            repo,
            number,
            role,
            content,
            images_json,
            chrono::Local::now().to_rfc3339()
        ],
    )?;
    Ok(())
}

/// List a PR's persisted chat in chronological order.
pub fn list_pr_chat(conn: &Connection, repo: &str, number: i64) -> Result<Vec<PrChatMsg>> {
    let mut stmt = conn.prepare(
        "SELECT role, content, images FROM pr_chat WHERE repo = ?1 AND number = ?2 ORDER BY id",
    )?;
    let rows = stmt.query_map(rusqlite::params![repo, number], |row| {
        let images_json: String = row.get::<_, Option<String>>(2)?.unwrap_or_default();
        let images: Vec<String> = serde_json::from_str(&images_json).unwrap_or_default();
        Ok(PrChatMsg {
            role: row.get(0)?,
            content: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            images,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::ActivityBlock;
    use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

    /// Build a UTC timestamp from a wall-clock string interpreted in the LOCAL
    /// timezone. Because blocks are now stored in local time, this makes the
    /// stored date prefix equal the date in `s` regardless of the machine's
    /// timezone, keeping `list_blocks_for_day` assertions deterministic in CI.
    fn ts(s: &str) -> DateTime<Utc> {
        let s = s.trim_end_matches('Z');
        let naive = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").unwrap();
        chrono::Local
            .from_local_datetime(&naive)
            .single()
            .expect("unambiguous local time")
            .with_timezone(&Utc)
    }

    fn ticket_key_for(conn: &rusqlite::Connection, title: &str) -> Option<String> {
        conn.query_row(
            "SELECT ticket_key FROM activity_blocks WHERE title = ?1",
            [title],
            |row| row.get::<_, Option<String>>(0),
        )
        .unwrap()
    }

    #[test]
    fn log_and_list_qa_activity_for_day() {
        let conn = open(":memory:").unwrap();

        log_qa_activity(
            &conn,
            "2026-06-22",
            "2026-06-22T09:00:00+07:00",
            "QAT-1",
            "Support CX Account Opening",
            "transition",
            "Ready for QA",
            "QA In Progress",
            None,
        )
        .unwrap();
        log_qa_activity(
            &conn,
            "2026-06-22",
            "2026-06-22T10:00:00+07:00",
            "QAT-2",
            "Deposit Method",
            "points",
            "",
            "",
            Some(3.0),
        )
        .unwrap();
        // A different day must be excluded.
        log_qa_activity(
            &conn,
            "2026-06-21",
            "2026-06-21T09:00:00+07:00",
            "QAT-9",
            "Old",
            "transition",
            "To Do",
            "Ready for QA",
            None,
        )
        .unwrap();

        let acts = list_qa_activity_for_day(&conn, "2026-06-22").unwrap();
        assert_eq!(acts.len(), 2);
        // Ordered by ts ascending.
        assert_eq!(acts[0].ticket_key, "QAT-1");
        assert_eq!(acts[0].kind, "transition");
        assert_eq!(acts[0].from_status, "Ready for QA");
        assert_eq!(acts[0].to_status, "QA In Progress");
        assert_eq!(acts[1].ticket_key, "QAT-2");
        assert_eq!(acts[1].kind, "points");
        assert_eq!(acts[1].points, Some(3.0));
    }

    #[test]
    fn insert_and_list_blocks_for_day() {
        let conn = open(":memory:").unwrap();

        let jira = ActivityBlock {
            app: "Chrome".into(),
            title: "JIRA-1234 Login".into(),
            start: ts("2026-06-18T09:00:00Z"),
            end: ts("2026-06-18T09:10:00Z"),
            is_idle: false,
        };
        let slack = ActivityBlock {
            app: "Slack".into(),
            title: "Slack".into(),
            start: ts("2026-06-18T08:00:00Z"),
            end: ts("2026-06-18T08:05:00Z"),
            is_idle: false,
        };
        // A block on a different day must be excluded.
        let other_day = ActivityBlock {
            app: "Chrome".into(),
            title: "JIRA-1234 Login".into(),
            start: ts("2026-06-17T09:00:00Z"),
            end: ts("2026-06-17T09:10:00Z"),
            is_idle: false,
        };

        insert_block(&conn, &jira).unwrap();
        insert_block(&conn, &slack).unwrap();
        insert_block(&conn, &other_day).unwrap();

        let blocks = list_blocks_for_day(&conn, "2026-06-18").unwrap();
        assert_eq!(blocks.len(), 2);
        // Ordered by start: slack (08:00) before jira (09:00).
        assert_eq!(blocks[0], slack);
        assert_eq!(blocks[1], jira);

        // ticket_key derived and stored.
        assert_eq!(
            ticket_key_for(&conn, "JIRA-1234 Login"),
            Some("JIRA-1234".to_string())
        );
        assert_eq!(ticket_key_for(&conn, "Slack"), None);
    }

    #[test]
    fn recompute_ticket_time_sums_and_excludes() {
        let conn = open(":memory:").unwrap();

        // ABC-1: two blocks -> 600 + 300 = 900s
        insert_block(
            &conn,
            &ActivityBlock {
                app: "Chrome".into(),
                title: "ABC-1 first".into(),
                start: ts("2026-06-18T09:00:00Z"),
                end: ts("2026-06-18T09:10:00Z"),
                is_idle: false,
            },
        )
        .unwrap();
        insert_block(
            &conn,
            &ActivityBlock {
                app: "Chrome".into(),
                title: "ABC-1 second".into(),
                start: ts("2026-06-18T11:00:00Z"),
                end: ts("2026-06-18T11:05:00Z"),
                is_idle: false,
            },
        )
        .unwrap();
        // XY-2: one block -> 120s
        insert_block(
            &conn,
            &ActivityBlock {
                app: "Editor".into(),
                title: "XY-2 work".into(),
                start: ts("2026-06-18T12:00:00Z"),
                end: ts("2026-06-18T12:02:00Z"),
                is_idle: false,
            },
        )
        .unwrap();
        // Idle block with a key -> excluded.
        insert_block(
            &conn,
            &ActivityBlock {
                app: "Chrome".into(),
                title: "ABC-1 idle".into(),
                start: ts("2026-06-18T13:00:00Z"),
                end: ts("2026-06-18T13:30:00Z"),
                is_idle: true,
            },
        )
        .unwrap();
        // No-key block -> excluded.
        insert_block(
            &conn,
            &ActivityBlock {
                app: "Slack".into(),
                title: "Slack".into(),
                start: ts("2026-06-18T14:00:00Z"),
                end: ts("2026-06-18T14:30:00Z"),
                is_idle: false,
            },
        )
        .unwrap();

        recompute_ticket_time(&conn, "2026-06-18").unwrap();

        let mut got = get_ticket_time(&conn, "2026-06-18").unwrap();
        got.sort();
        assert_eq!(
            got,
            vec![("ABC-1".to_string(), 900), ("XY-2".to_string(), 120)]
        );
    }

    #[test]
    fn recompute_ticket_time_is_idempotent() {
        let conn = open(":memory:").unwrap();
        insert_block(
            &conn,
            &ActivityBlock {
                app: "Chrome".into(),
                title: "ABC-1 work".into(),
                start: ts("2026-06-18T09:00:00Z"),
                end: ts("2026-06-18T09:10:00Z"),
                is_idle: false,
            },
        )
        .unwrap();

        recompute_ticket_time(&conn, "2026-06-18").unwrap();
        recompute_ticket_time(&conn, "2026-06-18").unwrap();

        let got = get_ticket_time(&conn, "2026-06-18").unwrap();
        assert_eq!(got, vec![("ABC-1".to_string(), 600)]);
    }

    #[test]
    fn config_round_trips_and_upserts() {
        let conn = open(":memory:").unwrap();
        assert_eq!(get_config(&conn, "jira_email").unwrap(), None);
        set_config(&conn, "jira_email", "a@b.co").unwrap();
        assert_eq!(
            get_config(&conn, "jira_email").unwrap(),
            Some("a@b.co".to_string())
        );
        // Upsert overwrites.
        set_config(&conn, "jira_email", "x@y.co").unwrap();
        assert_eq!(
            get_config(&conn, "jira_email").unwrap(),
            Some("x@y.co".to_string())
        );
    }

    #[test]
    fn set_block_ticket_overrides_and_clears() {
        let conn = open(":memory:").unwrap();
        insert_block(
            &conn,
            &ActivityBlock {
                app: "Slack".into(),
                title: "no key here".into(),
                start: ts("2026-06-18T09:00:00Z"),
                end: ts("2026-06-18T09:10:00Z"),
                is_idle: false,
            },
        )
        .unwrap();
        let id: i64 = conn
            .query_row("SELECT id FROM activity_blocks LIMIT 1", [], |r| r.get(0))
            .unwrap();

        set_block_ticket(&conn, id, "ABC-9").unwrap();
        let key: Option<String> = conn
            .query_row("SELECT ticket_key FROM activity_blocks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(key, Some("ABC-9".to_string()));

        // Empty string clears it back to NULL.
        set_block_ticket(&conn, id, "").unwrap();
        let key2: Option<String> = conn
            .query_row("SELECT ticket_key FROM activity_blocks WHERE id = ?1", [id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(key2, None);
    }

    #[test]
    fn test_cases_crud_round_trips() {
        let conn = open(":memory:").unwrap();

        let id1 = add_test_case(&conn, "QAT-1", "Login valid", "Buka login; isi kredensial", "Masuk ke dashboard").unwrap();
        let id2 = add_test_case(&conn, "QAT-1", "Login invalid", "Isi password salah", "Tampil pesan error").unwrap();
        assert!(id1 > 0 && id2 > id1);

        let cases = list_test_cases(&conn, "QAT-1").unwrap();
        assert_eq!(cases.len(), 2);
        // Ordered by id.
        assert_eq!(cases[0].id, id1);
        assert_eq!(cases[0].title, "Login valid");
        assert_eq!(cases[0].steps, "Buka login; isi kredensial");
        assert_eq!(cases[0].expected, "Masuk ke dashboard");
        // Default status.
        assert_eq!(cases[0].status, "untested");
        // Notes default to empty.
        assert_eq!(cases[0].notes, "");
        assert_eq!(cases[1].title, "Login invalid");

        // Notes round-trip.
        set_test_case_notes(&conn, id1, "Aktual: muncul 500, bukan dashboard").unwrap();
        let cases = list_test_cases(&conn, "QAT-1").unwrap();
        assert_eq!(cases[0].notes, "Aktual: muncul 500, bukan dashboard");
        assert_eq!(cases[1].notes, "");

        // A different ticket has no cases.
        assert!(list_test_cases(&conn, "QAT-2").unwrap().is_empty());

        // Update status.
        set_test_case_status(&conn, id1, "passed").unwrap();
        let cases = list_test_cases(&conn, "QAT-1").unwrap();
        assert_eq!(cases[0].status, "passed");
        assert_eq!(cases[1].status, "untested");

        // Update fields.
        update_test_case(&conn, id2, "Login salah", "step baru", "hasil baru").unwrap();
        let cases = list_test_cases(&conn, "QAT-1").unwrap();
        assert_eq!(cases[1].title, "Login salah");
        assert_eq!(cases[1].steps, "step baru");
        assert_eq!(cases[1].expected, "hasil baru");

        // Delete one.
        delete_test_case(&conn, id1).unwrap();
        let cases = list_test_cases(&conn, "QAT-1").unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].id, id2);
    }
}
