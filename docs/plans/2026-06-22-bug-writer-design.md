# Bug Writer (Bug Ghost Writer) — Design

**Date:** 2026-06-22
**Status:** Approved, ready for implementation plan

## Goal

Add a "Bug Writer" feature to QA Cockpit: turn free-form bug notes + an optional
screenshot into a structured bug report via Gemini, let the QA edit it, then push
it to Jira (create issue + attach screenshot). Ported in spirit from the standalone
Electron "Bug Ghost Writer"; re-implemented for QA Cockpit's Tauri (Rust + vanilla TS)
stack — it cannot be dropped in verbatim.

## Context / constraints

- App is **Tauri** (Rust backend, vanilla TS frontend) — NOT Electron. No React.
- **Local-only persistence**: SQLite at `~/Library/Application Support/site.hexalabs.qacockpit/qacockpit.db`.
  No remote/online DB, no shared server. Each user has their own local DB. Source of
  truth = Jira (+ GitHub for PRs); Sync pulls into the local cache.
- Gemini already wired via OpenAI-compatible endpoint (`ai/gemma.rs`, `AiTarget`,
  `ai_provider`/`gemini_*` config).
- Jira client (`integrations/jira.rs`) can read + transition + comment + set story
  points, and already does POST. It does **not** yet create issues or upload attachments.
- `reqwest` features are `["json","blocking"]` only — **no `multipart`**, and there is
  **no `base64` crate**. Decision: add `reqwest` `multipart` feature + `base64` crate.

## Architecture / data flow

```
[main.ts: Bug Writer overlay]
  text + screenshot (paste/drag/file) + language + sections
    -> invoke("generate_bug_report", {...})
[Rust: commands::generate_bug_report]
  -> gemma::build_bug_prompt(language, sections)
  -> AiTarget from config -> vision request -> parse TITLE + body
    -> { title, body, raw }
[main.ts: show editable title + body]
  user edits -> "Buat Bug di Jira"
    -> invoke("create_jira_bug", { projectKey, summary, body, priority, assigneeId, imageBase64 })
[Rust: commands::create_jira_bug]
  -> jira::find_issue_type(project, "Bug")
  -> jira::create_issue(...) -> { key, url }
  -> jira::upload_attachment(key, screenshot)  (if image present)
    -> { key, url }
[main.ts: toast success + link to browse/KEY ; Sync to show on board]
```

Principles: reuse `AiTarget`/config/Jira POST patterns; two separate commands so the
QA can review & edit before pushing; Jira body as ADF (reuse existing ADF helpers);
graceful degrade with existing `AI_UNAVAILABLE` message. No new DB tables (bug becomes
a Jira ticket; appears on board after Sync). Drafts intentionally NOT persisted (YAGNI).

## Backend changes

### `ai/gemma.rs`
- `build_bug_prompt(language, sections) -> String` — port of `buildSystemPrompt`
  (7 section definitions, TITLE-on-first-line rule, output in chosen language, section
  labels stay English).
- `parse_title_and_body(raw) -> (String, String)` — port of `parseTitleAndBody`; unit-tested.
- `build_vision_request(model, prompt, image_base64: Option<&str>)` — variant of
  `build_chat_request`. With image, `content` becomes an array:
  `[{type:text,text}, {type:image_url,image_url:{url:"data:image/png;base64,..."}}]`.
  Without image, plain string. `parse_chat_response` reused as-is.

### `integrations/jira.rs`
- `find_issue_type(base,email,token,project,"Bug") -> id` via `/rest/api/3/issue/createmeta`.
- `create_issue(...) -> {key,url}` — `POST /rest/api/3/issue`, fields
  `{project, issuetype, summary, description(ADF), priority?, assignee?}`.
- `upload_attachment(key, filename, image_base64)` — `POST /.../attachments`,
  header `X-Atlassian-Token: no-check`, multipart (via reqwest `multipart` feature).

### deps
- `reqwest` add feature `multipart`; add `base64` crate (decode screenshot for upload).

## Commands (`commands.rs` + register in `lib.rs`)
- `generate_bug_report(state, text, image_base64, language, sections) -> {title, body, raw}`
- `create_jira_bug(state, project_key, summary, body, priority, assignee_id, image_base64) -> {key, url}`
  — `require_jira_creds` -> find_issue_type -> create_issue -> upload_attachment (if image).
  Indonesian error messages, consistent with existing style.

## Frontend (`index.html` + `main.ts`)
- Top-bar button `🐞 Bug Writer` opens a full overlay (reuse `overlay`/`tc-section`/`.btn`/`toast`).
- Input: description `<textarea>` + screenshot drop-zone (click / drag / **paste Ctrl+V**) + preview.
- Options: output language dropdown (default Indonesia) + section checkboxes (6 defaults).
- Generate -> editable Title + Detail; Copy button.
- Push panel: Project (reuse `list_jira_projects`), Priority, Assignee (reuse `list_jira_assignees`)
  -> `📤 Buat Bug di Jira` -> success toast + `browse/KEY` link.
- Module-level state vars; `invoke<T>` + `errStr`/`toast` patterns as in `generateTestCases`.

## Testing (match file convention: test parsers/builders, skip thin HTTP)
- `parse_title_and_body`: TITLE present/absent, quoted, multiple leading blank lines.
- `build_bug_prompt`: selected sections present; language + TITLE rules present.
- `build_vision_request`: with/without image (string vs array content).
- `create_issue` body shape: correct `project/issuetype/summary/description(ADF)` fields.
- HTTP wrappers (`generate_bug_report`, `create_jira_bug`, `upload_attachment`) not unit-tested.

## Secondary / follow-up
- **Daily QA summary to UI**: `daily_summary_prompt` + `ai_summaries` table already exist;
  add a `generate_daily_summary()` command + a small top-bar button. Separate, smaller
  follow-up after Bug Writer ships.
```
