# Ticket Builder — Design

**Date:** 2026-06-22
**Status:** Approved, phased implementation

## Goal

Replicate, inside QA Cockpit, the Claude-Desktop + Atlassian-Rovo flow the QA team
uses to bulk-create QAT Story tickets under an Epic from a pasted list of PRs.
Paste a free-form blob → AI parses it into rows → QA edits the table → app
fetches PRs/source tickets, builds Acceptance Criteria, and creates the Stories
with all fields. Confirm-then-execute; nothing is created silently.

## Input (the de-facto spec)

There is no separate written prompt — the team pastes a blob like:

```
Epic: QAT-3423
UAT GTG
[[USSTOCK-2835](url)] Social stock prediction [#3197](github/pull/3197) @Reva Anggada (Reva)
feat(ipo): surface IPO stocks ... [#3200](github/pull/3200) @Oktapian Saepul Jaman (Okta)
...
assign the created ticket based on the name that i gave you
```

The format is NOT fixed — it varies — so parsing must be robust (AI, not regex).
Header yields the Epic and the summary tags (e.g. `[UAT] [GTG]`). Each row yields:
optional source ticket (USSTOCK-xxxx), a title, a GitHub PR (#number + url), and an
assignee `@Full Name (Short)`.

## Rules (reverse-engineered from real tickets QAT-3425..3432)

| Field | Rule |
|-------|------|
| issuetype | Story |
| project | QAT |
| parent (epic) | from input (e.g. QAT-3423) |
| sprint (customfield_10021) | the active sprint's id |
| reporter | Theo (constant; resolved by name) |
| priority | Highest |
| assignee | from `@mention`, resolved to accountId |
| summary | `[UAT] [<app>] <[SOURCE] >title #PR` |
| squad (customfield_10447, project-picker) | copy from source; default "Quality Assurance Team" when no source |
| developer (customfield_10612, user-picker) | from source's Developer; fallback source assignee; empty when no source |
| AC (customfield_10125, ADF) | see below |

### AC structure (ADF)
- Heading **Source Ticket** — link + summary, or "*No source ticket - ...*".
- Heading **GitHub PR** — the PR url.
- Divider.
- Heading **Acceptance Criteria** —
  - source has real AC → splice the source AC ADF verbatim;
  - else → Gemini generates from the PR description, as a numbered list under
    "*Based on PR #N description:*".

## Flow

```
Ticket Builder overlay (top-bar 🎫):
  Epic + App label (defaults QAT-3423 / GTG, editable)
  paste blob → [Parse: Gemini] → editable table (source?, title, PR, assignee dropdown)
  [Preview & generate AC] → per row: fetch PR + source → build AC → show summary
  [Create N tickets] → loop create_story → results (key + link), per-row errors non-fatal
```

## Backend (Rust)

- `integrations/jira.rs`:
  - `parse_active_sprint_id(json) -> Option<i64>` (pure, tested) + `fetch_active_sprint_id(project)`.
  - `resolve_user(query) -> Option<accountId>` (reuse assignable/search).
  - `parse_source_ticket(json) -> SourceTicket{summary, squad(Value), developer(accountId), assignee(accountId), ac_adf(Value)}` (pure, tested) + `fetch_source_ticket(key)`.
  - extend issue creation to accept parent/sprint/reporter/squad/developer/AC.
  - `jira_body` (already added) surfaces real errors.
- `ai/gemma.rs`:
  - `parse_ticket_rows_prompt(blob)` → instruct Gemini to emit JSON rows (pure, tested).
  - `generate_ac_prompt(pr_title, pr_body)` → numbered AC list (pure, tested).
  - `build_ac_adf(source, pr_url, ac_lines_or_source_adf)` ADF builder (pure, tested).
- `commands.rs`:
  - `parse_ticket_blob(blob) -> Vec<Row>` (Gemini, returns structured rows for the table).
  - `preview_ticket_row(row)` → fetch PR + source, build AC, return preview.
  - `create_story_tickets(epic, app, rows) -> Vec<Result>` (loop; per-row best-effort).
  - Settings: default reporter name, default squad (QA Team) project key.

## Frontend (vanilla TS)

Overlay with: epic/app inputs, paste textarea, Parse button → editable table
(checkbox, PR, title, assignee `list_jira_assignees` dropdown, source), Preview
button → per-row AC summary, Create button → results list (open each via opener).
Reuse invoke/toast/openUrl patterns; per-row errors shown inline.

## Phases (separate commits, TDD on parsers/builders)

1. Backend resolve: active sprint id + resolve_user + fetch/parse source ticket.
2. AC ADF builder + extend create_issue with the extra fields.
3. `create_story_tickets` command (+ register) and the Gemini parse/AC prompts.
4. Frontend: panel + Parse + editable table.
5. Frontend: preview AC + create + results.

Scope note: roughly the size of all prior features combined — built incrementally.
