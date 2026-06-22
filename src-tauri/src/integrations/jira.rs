use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JiraTicket {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub story_points: Option<f64>,
    pub updated: String,
}

/// Parse a Jira `/rest/api/3/search` response body into tickets.
/// `story_point_field` is the custom field id holding story points
/// (e.g. "customfield_10016"); it may be a number, null, or absent.
pub fn parse_issues(json: &str, story_point_field: &str) -> Result<Vec<JiraTicket>> {
    let root: Value = serde_json::from_str(json)?;
    let issues = root
        .get("issues")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut tickets = Vec::with_capacity(issues.len());
    for issue in issues {
        let key = issue
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let fields = issue.get("fields").cloned().unwrap_or(Value::Null);
        let summary = fields
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let status = fields
            .get("status")
            .and_then(|s| s.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let updated = fields
            .get("updated")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let story_points = fields.get(story_point_field).and_then(Value::as_f64);

        tickets.push(JiraTicket {
            key,
            summary,
            status,
            story_points,
            updated,
        });
    }
    Ok(tickets)
}

/// Fetch issues assigned to the current user, updated in the last day.
/// Thin HTTP wrapper around `parse_issues`; not unit-tested.
/// Build the JQL for "my tickets", optionally scoped to a project/board and a
/// specific assignee. Empty `assignee` means the logged-in user; empty
/// `project` means all projects. Returns the user's matching tickets ordered by
/// most-recently-updated (no date cutoff, so the whole assigned backlog shows).
pub fn build_jql(project: &str, assignee: &str, status_category: &str, sprint_scope: &str) -> String {
    let assignee_clause = if assignee.trim().is_empty() {
        "assignee = currentUser()".to_string()
    } else {
        format!("assignee = \"{}\"", assignee.trim())
    };
    let mut jql = String::new();
    if !project.trim().is_empty() {
        jql.push_str(&format!("project = \"{}\" AND ", project.trim()));
    }
    jql.push_str(&assignee_clause);
    // Optional status-category filter ("To Do" | "In Progress" | "Done") so a QA
    // can show only what they're actively working (In Progress).
    if !status_category.trim().is_empty() {
        jql.push_str(&format!(
            " AND statusCategory = \"{}\"",
            status_category.trim()
        ));
    }
    // Optional sprint scope: "active" = current sprint, "backlog" = not in any
    // sprint. Anything else (incl. empty) = no sprint filter (all-time).
    match sprint_scope.trim() {
        "active" => jql.push_str(" AND sprint in openSprints()"),
        "backlog" => jql.push_str(" AND sprint is EMPTY"),
        _ => {}
    }
    jql.push_str(" ORDER BY updated DESC");
    jql
}

// ---------------------------------------------------------------------------
// Jira metadata for Settings dropdowns (fields / projects / assignees)
// ---------------------------------------------------------------------------

/// A Jira field, e.g. the custom field that holds story points. Serialized to
/// the frontend with snake_case keys (`id`, `name`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JiraField {
    pub id: String,
    pub name: String,
}

/// Parse the body of `GET /rest/api/3/field` — a JSON ARRAY of field objects
/// `{"id":"customfield_10016","name":"Story point estimate", ...}`.
pub fn parse_fields(json: &str) -> Result<Vec<JiraField>> {
    let root: Value = serde_json::from_str(json)?;
    let arr = root.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut fields = Vec::with_capacity(arr.len());
    for f in arr {
        let id = f
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = f
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        fields.push(JiraField { id, name });
    }
    Ok(fields)
}

/// Fetch all Jira fields. Thin HTTP wrapper around `parse_fields`; not unit-tested.
pub fn fetch_fields(base_url: &str, email: &str, token: &str) -> Result<Vec<JiraField>> {
    let url = format!("{}/rest/api/3/field", base_url.trim_end_matches('/'));
    let client = crate::net::client();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .send()?
        .error_for_status()?
        .text()?;
    parse_fields(&body)
}

/// A Jira project, serialized with snake_case keys (`key`, `name`).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JiraProject {
    pub key: String,
    pub name: String,
}

/// Parse the body of `GET /rest/api/3/project/search` — `{"values":[{"key":...,
/// "name":...}], ...}`.
pub fn parse_projects(json: &str) -> Result<Vec<JiraProject>> {
    let root: Value = serde_json::from_str(json)?;
    let values = root
        .get("values")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut projects = Vec::with_capacity(values.len());
    for p in values {
        let key = p
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = p
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        projects.push(JiraProject { key, name });
    }
    Ok(projects)
}

/// Fetch projects visible to the user. Thin HTTP wrapper; not unit-tested.
pub fn fetch_projects(base_url: &str, email: &str, token: &str) -> Result<Vec<JiraProject>> {
    let url = format!(
        "{}/rest/api/3/project/search",
        base_url.trim_end_matches('/')
    );
    let client = crate::net::client();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .query(&[("maxResults", "100")])
        .send()?
        .error_for_status()?
        .text()?;
    parse_projects(&body)
}

/// A Jira user assignable to issues. Serialized with snake_case keys
/// (`account_id`, `display_name`), which is what the frontend reads.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JiraUser {
    pub account_id: String,
    pub display_name: String,
}

/// Parse the body of `GET /rest/api/3/user/assignable/search` — a JSON ARRAY of
/// user objects `{"accountId":"abc","displayName":"Okta", ...}`.
pub fn parse_assignees(json: &str) -> Result<Vec<JiraUser>> {
    let root: Value = serde_json::from_str(json)?;
    let arr = root.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut users = Vec::with_capacity(arr.len());
    for u in arr {
        let account_id = u
            .get("accountId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let display_name = u
            .get("displayName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        users.push(JiraUser {
            account_id,
            display_name,
        });
    }
    Ok(users)
}

/// Fetch users assignable in `project`. `assignable/search` requires a project,
/// so when `project` is empty we return an empty list rather than erroring.
/// Thin HTTP wrapper; not unit-tested.
pub fn fetch_assignees(
    base_url: &str,
    email: &str,
    token: &str,
    project: &str,
) -> Result<Vec<JiraUser>> {
    if project.trim().is_empty() {
        return Ok(Vec::new());
    }
    let url = format!(
        "{}/rest/api/3/user/assignable/search",
        base_url.trim_end_matches('/')
    );
    let client = crate::net::client();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .query(&[("project", project.trim()), ("maxResults", "100")])
        .send()?
        .error_for_status()?
        .text()?;
    parse_assignees(&body)
}

// ---------------------------------------------------------------------------
// Jira issue transitions (change a ticket's status, e.g. To Do -> In Progress)
// ---------------------------------------------------------------------------

/// An available workflow transition for an issue. `id` is what you POST back to
/// trigger it; `to_status` is the status the issue lands on. Serialized with
/// snake_case keys (`id`, `name`, `to_status`) for the frontend.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct JiraTransition {
    pub id: String,
    pub name: String,
    pub to_status: String,
}

/// Parse the body of `GET /rest/api/3/issue/{key}/transitions` —
/// `{"transitions":[{"id":"11","name":"Start Progress","to":{"name":"In Progress"}}, ...]}`.
/// `to_status` defaults to "" when the `to.name` is missing.
pub fn parse_transitions(json: &str) -> Result<Vec<JiraTransition>> {
    let root: Value = serde_json::from_str(json)?;
    let arr = root
        .get("transitions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut out = Vec::with_capacity(arr.len());
    for t in arr {
        let id = t
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let to_status = t
            .get("to")
            .and_then(|to| to.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        out.push(JiraTransition {
            id,
            name,
            to_status,
        });
    }
    Ok(out)
}

/// Fetch the transitions available for `issue_key`. Thin HTTP wrapper around
/// `parse_transitions`; not unit-tested.
pub fn fetch_transitions(
    base_url: &str,
    email: &str,
    token: &str,
    issue_key: &str,
) -> Result<Vec<JiraTransition>> {
    let url = format!(
        "{}/rest/api/3/issue/{}/transitions",
        base_url.trim_end_matches('/'),
        issue_key
    );
    let client = crate::net::client();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .send()?
        .error_for_status()?
        .text()?;
    parse_transitions(&body)
}

/// Trigger transition `transition_id` on `issue_key` (a WRITE to Jira). Jira
/// returns 204 No Content on success. Thin HTTP wrapper; not unit-tested.
pub fn do_transition(
    base_url: &str,
    email: &str,
    token: &str,
    issue_key: &str,
    transition_id: &str,
) -> Result<()> {
    let url = format!(
        "{}/rest/api/3/issue/{}/transitions",
        base_url.trim_end_matches('/'),
        issue_key
    );
    let body = serde_json::json!({ "transition": { "id": transition_id } });
    let client = crate::net::client();
    client
        .post(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .json(&body)
        .send()?
        .error_for_status()?;
    Ok(())
}

/// Set (or clear) a ticket's story points via the configured custom field.
/// `points = None` clears it. Thin HTTP wrapper; not unit-tested.
pub fn update_story_points(
    base_url: &str,
    email: &str,
    token: &str,
    issue_key: &str,
    field: &str,
    points: Option<f64>,
) -> Result<()> {
    let url = format!(
        "{}/rest/api/3/issue/{}",
        base_url.trim_end_matches('/'),
        issue_key
    );
    let value = match points {
        Some(p) => serde_json::json!(p),
        None => serde_json::Value::Null,
    };
    let body = serde_json::json!({ "fields": { field: value } });
    let client = crate::net::client();
    client
        .put(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .json(&body)
        .send()?
        .error_for_status()?;
    Ok(())
}

pub fn fetch_my_issues(
    base_url: &str,
    email: &str,
    token: &str,
    story_point_field: &str,
    project: &str,
    assignee: &str,
    status_category: &str,
    sprint_scope: &str,
) -> Result<Vec<JiraTicket>> {
    let fields = format!("summary,status,updated,{}", story_point_field);
    // The legacy /rest/api/3/search endpoint was removed by Atlassian (returns
    // 410 Gone since mid-2025); the enhanced-JQL endpoint replaces it. The
    // response still has an `issues[]` array, so `parse_issues` is unchanged.
    let url = format!("{}/rest/api/3/search/jql", base_url.trim_end_matches('/'));
    let jql = build_jql(project, assignee, status_category, sprint_scope);
    let client = crate::net::client();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .query(&[
            ("jql", jql.as_str()),
            ("fields", fields.as_str()),
            ("maxResults", "100"),
        ])
        .send()?
        .error_for_status()?
        .text()?;
    parse_issues(&body, story_point_field)
}

// ---------------------------------------------------------------------------
// Posting QA test results back to Jira as a comment (ADF table)
// ---------------------------------------------------------------------------

/// One test-case result destined for the ADF table. `status` is the raw db
/// value ("passed" | "failed" | anything else = untested); `notes` is the
/// optional free-text actual-result remark (empty = omitted).
#[derive(Debug, Clone)]
pub struct ResultRow {
    pub title: String,
    pub steps: String,
    pub expected: String,
    pub status: String,
    pub notes: String,
}

/// Map a raw status to a `status` lozenge (label, color) per Jira's ADF schema.
/// Colors are from the fixed ADF palette: green/red/neutral.
fn status_lozenge(status: &str) -> (&'static str, &'static str) {
    match status {
        "passed" => ("PASS", "green"),
        "failed" => ("FAIL", "red"),
        _ => ("UNTESTED", "neutral"),
    }
}

/// Build an Atlassian Document Format (ADF) comment body for QA test results:
/// a level-3 `heading`, a colored `panel` summary, then a compact 3-column
/// `table` (No / Test Case / Hasil & Catatan). The result cell carries an inline
/// `status` lozenge; any note is folded UNDERNEATH it (hardBreak + text) in the
/// same paragraph, keeping the table to 3 columns.
pub fn build_results_adf(
    heading: &str,
    panel_type: &str,
    panel_text: &str,
    rows: &[ResultRow],
) -> Value {
    // A paragraph node wrapping a single text run.
    let para = |text: &str| {
        serde_json::json!({
            "type": "paragraph",
            "content": [{ "type": "text", "text": text }]
        })
    };
    let header_cell = |text: &str| {
        serde_json::json!({
            "type": "tableHeader",
            "attrs": {},
            "content": [para(text)]
        })
    };
    // A body cell wrapping arbitrary content nodes (usually a single paragraph).
    let cell = |content: Vec<Value>| {
        serde_json::json!({
            "type": "tableCell",
            "attrs": {},
            "content": content
        })
    };

    // A cell that shows a dash when the text is empty (keeps cells non-blank).
    let text_cell = |text: &str| {
        let t = if text.trim().is_empty() { "—" } else { text };
        cell(vec![para(t)])
    };

    let mut table_rows: Vec<Value> = Vec::with_capacity(rows.len() + 1);
    // No manual "No" column — `isNumberColumnEnabled` below gives Jira's own
    // narrow auto-number column on the left.
    table_rows.push(serde_json::json!({
        "type": "tableRow",
        "content": [
            header_cell("Test Case"),
            header_cell("Langkah"),
            header_cell("Harapan"),
            header_cell("Hasil & Catatan"),
        ]
    }));
    for r in rows.iter() {
        let (label, color) = status_lozenge(&r.status);
        // The result cell's paragraph: an inline `status` lozenge, optionally
        // followed by a hardBreak + the note text (same paragraph).
        let mut result_inline: Vec<Value> = vec![serde_json::json!({
            "type": "status",
            "attrs": { "text": label, "color": color }
        })];
        if !r.notes.trim().is_empty() {
            result_inline.push(serde_json::json!({ "type": "hardBreak" }));
            result_inline.push(serde_json::json!({ "type": "text", "text": r.notes }));
        }
        let result_para = serde_json::json!({
            "type": "paragraph",
            "content": result_inline
        });

        table_rows.push(serde_json::json!({
            "type": "tableRow",
            "content": [
                cell(vec![para(&r.title)]),
                text_cell(&r.steps),
                text_cell(&r.expected),
                cell(vec![result_para]),
            ]
        }));
    }

    serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": [
            {
                "type": "heading",
                "attrs": { "level": 3 },
                "content": [{ "type": "text", "text": heading }]
            },
            {
                "type": "panel",
                "attrs": { "panelType": panel_type },
                "content": [{
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": panel_text }]
                }]
            },
            {
                "type": "table",
                "attrs": { "isNumberColumnEnabled": true, "layout": "default" },
                "content": table_rows
            }
        ]
    })
}

// ---------------------------------------------------------------------------
// Creating a bug issue (Bug Writer)
// ---------------------------------------------------------------------------

/// Convert plain multi-line text into an ADF `doc`. Each line becomes a
/// paragraph; a blank line becomes a content-less paragraph (an empty text node
/// is invalid ADF, so blank lines must carry no `content`).
pub fn text_to_adf(text: &str) -> Value {
    let content: Vec<Value> = text
        .split('\n')
        .map(|line| {
            if line.trim().is_empty() {
                serde_json::json!({ "type": "paragraph" })
            } else {
                serde_json::json!({
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": line }]
                })
            }
        })
        .collect();
    serde_json::json!({ "type": "doc", "version": 1, "content": content })
}

/// Build the `{ "fields": {...} }` body for `POST /rest/api/3/issue`. `priority`
/// and `assignee_account_id` are omitted when `None`.
pub fn build_create_issue_body(
    project_key: &str,
    issue_type_id: &str,
    summary: &str,
    description_adf: &Value,
    priority: Option<&str>,
    assignee_account_id: Option<&str>,
) -> Value {
    let mut fields = serde_json::Map::new();
    fields.insert("project".into(), serde_json::json!({ "key": project_key }));
    fields.insert("issuetype".into(), serde_json::json!({ "id": issue_type_id }));
    fields.insert("summary".into(), serde_json::json!(summary));
    fields.insert("description".into(), description_adf.clone());
    if let Some(p) = priority {
        fields.insert("priority".into(), serde_json::json!({ "name": p }));
    }
    if let Some(a) = assignee_account_id {
        fields.insert("assignee".into(), serde_json::json!({ "accountId": a }));
    }
    serde_json::json!({ "fields": Value::Object(fields) })
}

/// Find an issue type id by name in a createmeta `issuetypes` response. Matches
/// exactly first, then case-insensitively. Returns `None` if not found.
pub fn parse_issue_type_id(json: &str, name: &str) -> Option<String> {
    let root: Value = serde_json::from_str(json).ok()?;
    let types = root.get("issueTypes").and_then(Value::as_array)?;
    let id_of = |it: &Value| {
        it.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    types
        .iter()
        .find(|it| it.get("name").and_then(Value::as_str) == Some(name))
        .or_else(|| {
            types.iter().find(|it| {
                it.get("name")
                    .and_then(Value::as_str)
                    .map(|n| n.eq_ignore_ascii_case(name))
                    .unwrap_or(false)
            })
        })
        .and_then(id_of)
}

/// Resolve an issue type id (e.g. "Bug") for a project via createmeta.
/// Thin HTTP wrapper around [`parse_issue_type_id`]; not unit-tested.
pub fn find_issue_type(
    base_url: &str,
    email: &str,
    token: &str,
    project_key: &str,
    issue_type_name: &str,
) -> Result<String> {
    let url = format!(
        "{}/rest/api/3/issue/createmeta/{}/issuetypes?maxResults=200",
        base_url.trim_end_matches('/'),
        project_key
    );
    let client = crate::net::client();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .send()?
        .error_for_status()?
        .text()?;
    parse_issue_type_id(&body, issue_type_name).ok_or_else(|| {
        anyhow::anyhow!("Issue type \"{issue_type_name}\" tidak ditemukan di project {project_key}")
    })
}

/// A created Jira issue: its key and a browse URL.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CreatedIssue {
    pub key: String,
    pub url: String,
}

/// Create a bug issue (a WRITE to Jira). Returns the new key + browse URL.
/// Thin HTTP wrapper around [`build_create_issue_body`]; not unit-tested.
#[allow(clippy::too_many_arguments)]
pub fn create_issue(
    base_url: &str,
    email: &str,
    token: &str,
    project_key: &str,
    issue_type_id: &str,
    summary: &str,
    description_adf: &Value,
    priority: Option<&str>,
    assignee_account_id: Option<&str>,
) -> Result<CreatedIssue> {
    let base = base_url.trim_end_matches('/');
    let url = format!("{base}/rest/api/3/issue");
    let body = build_create_issue_body(
        project_key,
        issue_type_id,
        summary,
        description_adf,
        priority,
        assignee_account_id,
    );
    let client = crate::net::client();
    let resp = client
        .post(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()?
        .error_for_status()?
        .text()?;
    let parsed: Value = serde_json::from_str(&resp)?;
    let key = parsed
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("Jira tidak mengembalikan key issue"))?
        .to_string();
    let url = format!("{base}/browse/{key}");
    Ok(CreatedIssue { key, url })
}

/// Upload a base64 image as an attachment on an existing issue (a WRITE to Jira).
/// Accepts a bare base64 string or a `data:` URL. Thin HTTP wrapper; not unit-tested.
pub fn upload_attachment(
    base_url: &str,
    email: &str,
    token: &str,
    issue_key: &str,
    filename: &str,
    image_base64: &str,
) -> Result<()> {
    use base64::Engine;
    // Strip a data-URL prefix if present.
    let raw = match image_base64.find(',') {
        Some(idx) if image_base64.starts_with("data:") => &image_base64[idx + 1..],
        _ => image_base64,
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .map_err(|e| anyhow::anyhow!("screenshot bukan base64 valid: {e}"))?;
    let safe_name = filename.replace(['\r', '\n', '"'], "_");
    let part = reqwest::blocking::multipart::Part::bytes(bytes)
        .file_name(safe_name)
        .mime_str("application/octet-stream")?;
    let form = reqwest::blocking::multipart::Form::new().part("file", part);
    let url = format!(
        "{}/rest/api/3/issue/{}/attachments",
        base_url.trim_end_matches('/'),
        issue_key
    );
    let client = crate::net::client();
    client
        .post(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .header("X-Atlassian-Token", "no-check")
        .multipart(form)
        .send()?
        .error_for_status()?;
    Ok(())
}

/// Verify Jira credentials by fetching the current user; returns the display
/// name. Thin HTTP wrapper; not unit-tested.
pub fn fetch_myself(base_url: &str, email: &str, token: &str) -> Result<String> {
    let url = format!("{}/rest/api/3/myself", base_url.trim_end_matches('/'));
    let client = crate::net::client();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .send()?
        .error_for_status()?
        .text()?;
    let v: Value = serde_json::from_str(&body)?;
    Ok(v.get("displayName")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)")
        .to_string())
}

/// Add a comment to a Jira issue (a WRITE to Jira). `body_adf` is the ADF doc
/// node. Thin HTTP wrapper; not unit-tested.
pub fn add_comment(
    base_url: &str,
    email: &str,
    token: &str,
    key: &str,
    body_adf: &Value,
) -> Result<()> {
    let url = format!(
        "{}/rest/api/3/issue/{}/comment",
        base_url.trim_end_matches('/'),
        key
    );
    let body = serde_json::json!({ "body": body_adf });
    let client = crate::net::client();
    client
        .post(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()?
        .error_for_status()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_to_adf_wraps_each_line_in_a_paragraph() {
        let adf = text_to_adf("Line one\nLine two");
        assert_eq!(adf["type"], "doc");
        assert_eq!(adf["version"], 1);
        let content = adf["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "paragraph");
        assert_eq!(content[0]["content"][0]["text"], "Line one");
        assert_eq!(content[1]["content"][0]["text"], "Line two");
    }

    #[test]
    fn text_to_adf_blank_line_becomes_contentless_paragraph() {
        // A blank line must NOT emit an empty text node (invalid ADF).
        let adf = text_to_adf("A\n\nB");
        let content = adf["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert!(content[1].get("content").is_none());
    }

    #[test]
    fn build_create_issue_body_minimal_has_required_fields() {
        let desc = text_to_adf("body");
        let v = build_create_issue_body("QAT", "10001", "My bug", &desc, None, None);
        assert_eq!(v["fields"]["project"]["key"], "QAT");
        assert_eq!(v["fields"]["issuetype"]["id"], "10001");
        assert_eq!(v["fields"]["summary"], "My bug");
        assert_eq!(v["fields"]["description"]["type"], "doc");
        assert!(v["fields"].get("priority").is_none());
        assert!(v["fields"].get("assignee").is_none());
    }

    #[test]
    fn build_create_issue_body_includes_optional_priority_and_assignee() {
        let desc = text_to_adf("body");
        let v = build_create_issue_body("QAT", "10001", "s", &desc, Some("High"), Some("acc-123"));
        assert_eq!(v["fields"]["priority"]["name"], "High");
        assert_eq!(v["fields"]["assignee"]["accountId"], "acc-123");
    }

    #[test]
    fn parse_issue_type_id_matches_by_name_case_insensitively() {
        let json = r#"{"issueTypes":[{"id":"10000","name":"Task"},{"id":"10004","name":"Bug"}]}"#;
        assert_eq!(parse_issue_type_id(json, "Bug").unwrap(), "10004");
        assert_eq!(parse_issue_type_id(json, "bug").unwrap(), "10004");
    }

    #[test]
    fn parse_issue_type_id_returns_none_when_absent() {
        let json = r#"{"issueTypes":[{"id":"10000","name":"Task"}]}"#;
        assert!(parse_issue_type_id(json, "Bug").is_none());
    }

    #[test]
    fn build_results_adf_has_heading_panel_and_status_table() {
        let rows = vec![
            ResultRow {
                title: "Login valid".to_string(),
                steps: "Buka login, isi benar".to_string(),
                expected: "Masuk dashboard".to_string(),
                status: "passed".to_string(),
                notes: String::new(),
            },
            ResultRow {
                title: "Login invalid".to_string(),
                steps: "Isi password salah".to_string(),
                expected: "Pesan error".to_string(),
                status: "failed".to_string(),
                notes: "Muncul 500, bukan pesan error".to_string(),
            },
        ];
        let doc = build_results_adf(
            "🧪 Hasil Test QA — QAT-3444 · 19 Jun 2026 · okta@tr8.io",
            "error",
            "1 test case GAGAL dari 2",
            &rows,
        );

        assert_eq!(doc["type"], "doc");
        let content = doc["content"].as_array().unwrap();
        // heading first.
        assert_eq!(content[0]["type"], "heading");
        assert_eq!(content[0]["attrs"]["level"], 3);

        // A panel node with the requested panelType exists.
        let panel = content
            .iter()
            .find(|n| n["type"] == "panel")
            .expect("panel node present");
        assert_eq!(panel["attrs"]["panelType"], "error");
        assert_eq!(
            panel["content"][0]["content"][0]["text"],
            "1 test case GAGAL dari 2"
        );

        // Find the table node.
        let table = content
            .iter()
            .find(|n| n["type"] == "table")
            .expect("table node present");
        let trows = table["content"].as_array().unwrap();
        // 1 header row + 2 body rows.
        assert_eq!(trows.len(), 3);

        // Header row has 4 header cells: Test Case / Langkah / Harapan / Hasil & Catatan.
        let header = trows[0]["content"].as_array().unwrap();
        assert_eq!(header.len(), 4);
        assert_eq!(header[0]["type"], "tableHeader");
        assert_eq!(header[0]["content"][0]["content"][0]["text"], "Test Case");
        assert_eq!(
            header[3]["content"][0]["content"][0]["text"],
            "Hasil & Catatan"
        );

        // A body cell's text matches (row 1, first cell = title).
        assert_eq!(
            trows[1]["content"][0]["content"][0]["content"][0]["text"],
            "Login valid"
        );

        // Row 1 result cell (4th) contains a `status` lozenge with text "PASS".
        let pass_status = &trows[1]["content"][3]["content"][0]["content"][0];
        assert_eq!(pass_status["type"], "status");
        assert_eq!(pass_status["attrs"]["text"], "PASS");
        assert_eq!(pass_status["attrs"]["color"], "green");

        // Row 2 result cell (4th): FAIL lozenge + hardBreak + the note text.
        let fail_inline = trows[2]["content"][3]["content"][0]["content"]
            .as_array()
            .unwrap();
        assert_eq!(fail_inline[0]["type"], "status");
        assert_eq!(fail_inline[0]["attrs"]["text"], "FAIL");
        assert_eq!(fail_inline[1]["type"], "hardBreak");
        assert_eq!(fail_inline[2]["text"], "Muncul 500, bukan pesan error");
    }

    // --- fields fixture: GET /rest/api/3/field returns a JSON array ---
    const FIELDS_FIXTURE: &str = r#"[
      {"id":"summary","key":"summary","name":"Summary","custom":false},
      {"id":"customfield_10016","key":"customfield_10016","name":"Story point estimate","custom":true},
      {"id":"customfield_10222","key":"customfield_10222","name":"Actual sprint point","custom":true}
    ]"#;

    #[test]
    fn parses_fields_id_and_name() {
        let fields = parse_fields(FIELDS_FIXTURE).unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].id, "summary");
        assert_eq!(fields[0].name, "Summary");
        assert_eq!(fields[2].id, "customfield_10222");
        assert_eq!(fields[2].name, "Actual sprint point");
    }

    // --- projects fixture: GET /rest/api/3/project/search returns {values:[...]} ---
    const PROJECTS_FIXTURE: &str = r#"{
      "maxResults": 50,
      "total": 2,
      "values": [
        {"id":"10000","key":"QAT","name":"QA Team"},
        {"id":"10001","key":"DEV","name":"Development"}
      ]
    }"#;

    #[test]
    fn parses_projects_key_and_name() {
        let projects = parse_projects(PROJECTS_FIXTURE).unwrap();
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].key, "QAT");
        assert_eq!(projects[0].name, "QA Team");
        assert_eq!(projects[1].key, "DEV");
        assert_eq!(projects[1].name, "Development");
    }

    // --- assignees fixture: GET /rest/api/3/user/assignable/search → JSON array ---
    const ASSIGNEES_FIXTURE: &str = r#"[
      {"accountId":"abc123","displayName":"Okta Jaman","active":true},
      {"accountId":"def456","displayName":"Budi Santoso","active":true}
    ]"#;

    #[test]
    fn parses_assignees_account_id_and_display_name() {
        let users = parse_assignees(ASSIGNEES_FIXTURE).unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].account_id, "abc123");
        assert_eq!(users[0].display_name, "Okta Jaman");
        assert_eq!(users[1].account_id, "def456");
        assert_eq!(users[1].display_name, "Budi Santoso");
    }

    // Captured fixture mimicking a Jira /rest/api/3/search response.
    // Issue 1 has a story-point number; issue 2 has it null/absent.
    const FIXTURE: &str = r#"{
      "issues": [
        {
          "key": "QA-101",
          "fields": {
            "summary": "Write regression suite for login",
            "status": { "name": "In Progress" },
            "updated": "2026-06-18T10:15:00.000+0700",
            "customfield_10016": 5
          }
        },
        {
          "key": "QA-102",
          "fields": {
            "summary": "Investigate flaky checkout test",
            "status": { "name": "To Do" },
            "updated": "2026-06-17T08:30:00.000+0700",
            "customfield_10016": null
          }
        }
      ]
    }"#;

    #[test]
    fn parses_issues_with_and_without_story_points() {
        let tickets = parse_issues(FIXTURE, "customfield_10016").unwrap();
        assert_eq!(tickets.len(), 2);

        assert_eq!(
            tickets[0],
            JiraTicket {
                key: "QA-101".to_string(),
                summary: "Write regression suite for login".to_string(),
                status: "In Progress".to_string(),
                story_points: Some(5.0),
                updated: "2026-06-18T10:15:00.000+0700".to_string(),
            }
        );

        assert_eq!(tickets[1].key, "QA-102");
        assert_eq!(tickets[1].summary, "Investigate flaky checkout test");
        assert_eq!(tickets[1].status, "To Do");
        assert_eq!(tickets[1].story_points, None);
        assert_eq!(tickets[1].updated, "2026-06-17T08:30:00.000+0700");
    }

    #[test]
    fn jql_defaults_to_current_user_all_projects() {
        assert_eq!(
            build_jql("", "", "", ""),
            "assignee = currentUser() ORDER BY updated DESC"
        );
    }

    #[test]
    fn jql_scopes_to_project_and_assignee() {
        assert_eq!(
            build_jql("QAT", "okta@company.com", "", ""),
            "project = \"QAT\" AND assignee = \"okta@company.com\" ORDER BY updated DESC"
        );
    }

    #[test]
    fn jql_project_only_uses_current_user() {
        assert_eq!(
            build_jql("QAT", "", "", ""),
            "project = \"QAT\" AND assignee = currentUser() ORDER BY updated DESC"
        );
    }

    #[test]
    fn jql_with_status_category() {
        assert_eq!(
            build_jql("QAT", "", "In Progress", ""),
            "project = \"QAT\" AND assignee = currentUser() AND statusCategory = \"In Progress\" ORDER BY updated DESC"
        );
    }

    #[test]
    fn jql_with_active_sprint() {
        assert_eq!(
            build_jql("QAT", "", "", "active"),
            "project = \"QAT\" AND assignee = currentUser() AND sprint in openSprints() ORDER BY updated DESC"
        );
    }

    // --- transitions fixture: GET /rest/api/3/issue/{key}/transitions ---
    const TRANSITIONS_FIXTURE: &str = r#"{
      "transitions": [
        {"id":"11","name":"Start Progress","to":{"name":"In Progress"}},
        {"id":"31","name":"Done","to":{"name":"Done"}}
      ]
    }"#;

    #[test]
    fn parses_transitions_id_name_and_to_status() {
        let trans = parse_transitions(TRANSITIONS_FIXTURE).unwrap();
        assert_eq!(trans.len(), 2);
        assert_eq!(
            trans[0],
            JiraTransition {
                id: "11".to_string(),
                name: "Start Progress".to_string(),
                to_status: "In Progress".to_string(),
            }
        );
        assert_eq!(trans[1].id, "31");
        assert_eq!(trans[1].name, "Done");
        assert_eq!(trans[1].to_status, "Done");
    }

    #[test]
    fn jql_with_backlog_scope() {
        assert_eq!(
            build_jql("QAT", "", "", "backlog"),
            "project = \"QAT\" AND assignee = currentUser() AND sprint is EMPTY ORDER BY updated DESC"
        );
    }
}
