use crate::core::matching::extract_ticket_key;
use crate::core::types::ActivityBlock;
use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::Connection;

/// Open (or create) the SQLite database at `path` and apply the schema.
/// Pass `":memory:"` for an in-memory database (used in tests).
pub fn open(path: &str) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(include_str!("schema.sql"))?;
    Ok(conn)
}

/// Insert one activity block, deriving and storing its Jira ticket_key from the title.
pub fn insert_block(conn: &Connection, block: &ActivityBlock) -> Result<()> {
    let ticket_key = extract_ticket_key(&block.title);
    conn.execute(
        "INSERT INTO activity_blocks (app, title, start, end, is_idle, ticket_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            block.app,
            block.title,
            block.start.to_rfc3339(),
            block.end.to_rfc3339(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::ActivityBlock;
    use chrono::{DateTime, Utc};

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
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
}
