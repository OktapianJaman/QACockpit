pub mod github;
pub mod jira;

use anyhow::Result;
use rusqlite::Connection;

/// Upsert Jira tickets into the `jira_tickets` table (INSERT OR REPLACE on key).
pub fn save_tickets(conn: &Connection, tickets: &[jira::JiraTicket]) -> Result<()> {
    let mut stmt = conn.prepare(
        "INSERT OR REPLACE INTO jira_tickets (key, summary, status, story_points, updated)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for t in tickets {
        stmt.execute(rusqlite::params![
            t.key,
            t.summary,
            t.status,
            t.story_points,
            t.updated,
        ])?;
    }
    Ok(())
}

/// Replace all rows in `pull_requests` with the given PRs (DELETE all, then insert).
pub fn save_prs(conn: &Connection, prs: &[github::Pr]) -> Result<()> {
    conn.execute("DELETE FROM pull_requests", [])?;
    let mut stmt = conn.prepare(
        "INSERT INTO pull_requests (number, repo, title, state, url, updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for p in prs {
        stmt.execute(rusqlite::params![
            p.number,
            p.repo,
            p.title,
            p.state,
            p.url,
            p.updated,
        ])?;
    }
    Ok(())
}
