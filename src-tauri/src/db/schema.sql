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

-- A log of QA actions taken in the app (status moves + point sets), used to
-- build the daily summary. Jira itself isn't queried for this; we record each
-- action locally as the user performs it.
CREATE TABLE IF NOT EXISTS qa_activity (
    id INTEGER PRIMARY KEY,
    day TEXT,
    ts TEXT,
    ticket_key TEXT,
    summary TEXT,
    kind TEXT,          -- 'transition' | 'points'
    from_status TEXT,
    to_status TEXT,
    points REAL
);

CREATE TABLE IF NOT EXISTS test_cases (
    id INTEGER PRIMARY KEY,
    ticket_key TEXT NOT NULL,
    title TEXT NOT NULL,
    steps TEXT,
    expected TEXT,
    status TEXT NOT NULL DEFAULT 'untested',
    created_at TEXT,
    notes TEXT
);

-- Cached AI summary for a PR (the "Ringkas + apa yang dites" output), keyed by
-- the PR itself so it survives closing the ticket modal.
CREATE TABLE IF NOT EXISTS pr_summaries (
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    body TEXT,
    updated_at TEXT,
    PRIMARY KEY (repo, number)
);

-- Persisted follow-up Q&A chat for a PR. `images` is a JSON array of data: URLs
-- attached to the message (empty array when none).
CREATE TABLE IF NOT EXISTS pr_chat (
    id INTEGER PRIMARY KEY,
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    role TEXT NOT NULL,
    content TEXT,
    images TEXT NOT NULL DEFAULT '[]',
    created_at TEXT
);
