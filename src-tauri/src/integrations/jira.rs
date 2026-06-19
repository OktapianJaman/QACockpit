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
pub fn build_jql(project: &str, assignee: &str) -> String {
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
    let client = reqwest::blocking::Client::new();
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
    let client = reqwest::blocking::Client::new();
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
    let client = reqwest::blocking::Client::new();
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

pub fn fetch_my_issues(
    base_url: &str,
    email: &str,
    token: &str,
    story_point_field: &str,
    project: &str,
    assignee: &str,
) -> Result<Vec<JiraTicket>> {
    let fields = format!("summary,status,updated,{}", story_point_field);
    // The legacy /rest/api/3/search endpoint was removed by Atlassian (returns
    // 410 Gone since mid-2025); the enhanced-JQL endpoint replaces it. The
    // response still has an `issues[]` array, so `parse_issues` is unchanged.
    let url = format!("{}/rest/api/3/search/jql", base_url.trim_end_matches('/'));
    let jql = build_jql(project, assignee);
    let client = reqwest::blocking::Client::new();
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

#[cfg(test)]
mod tests {
    use super::*;

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
            build_jql("", ""),
            "assignee = currentUser() ORDER BY updated DESC"
        );
    }

    #[test]
    fn jql_scopes_to_project_and_assignee() {
        assert_eq!(
            build_jql("QAT", "okta@company.com"),
            "project = \"QAT\" AND assignee = \"okta@company.com\" ORDER BY updated DESC"
        );
    }

    #[test]
    fn jql_project_only_uses_current_user() {
        assert_eq!(
            build_jql("QAT", ""),
            "project = \"QAT\" AND assignee = currentUser() ORDER BY updated DESC"
        );
    }
}
