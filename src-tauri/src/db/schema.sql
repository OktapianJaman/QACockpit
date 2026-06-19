CREATE TABLE IF NOT EXISTS activity_blocks (
    id INTEGER PRIMARY KEY,
    app TEXT,
    title TEXT,
    start TEXT,
    end TEXT,
    is_idle INTEGER,
    ticket_key TEXT
);

CREATE TABLE IF NOT EXISTS jira_tickets (
    key TEXT PRIMARY KEY,
    summary TEXT,
    status TEXT,
    story_points REAL,
    updated TEXT
);

CREATE TABLE IF NOT EXISTS pull_requests (
    id INTEGER PRIMARY KEY,
    number INTEGER,
    repo TEXT,
    title TEXT,
    state TEXT,
    url TEXT,
    updated TEXT
);

CREATE TABLE IF NOT EXISTS ticket_time (
    day TEXT,
    ticket_key TEXT,
    worked_secs INTEGER,
    PRIMARY KEY (day, ticket_key)
);

CREATE TABLE IF NOT EXISTS notes (
    day TEXT PRIMARY KEY,
    body TEXT
);

CREATE TABLE IF NOT EXISTS ai_summaries (
    day TEXT PRIMARY KEY,
    kind TEXT,
    body TEXT,
    generated_at TEXT
);

CREATE TABLE IF NOT EXISTS config (
    key TEXT PRIMARY KEY,
    value TEXT
);
