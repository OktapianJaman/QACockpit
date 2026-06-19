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
pub fn fetch_my_issues(
    base_url: &str,
    email: &str,
    token: &str,
    story_point_field: &str,
) -> Result<Vec<JiraTicket>> {
    let fields = format!("summary,status,updated,{}", story_point_field);
    let url = format!("{}/rest/api/3/search", base_url.trim_end_matches('/'));
    let client = reqwest::blocking::Client::new();
    let body = client
        .get(url)
        .basic_auth(email, Some(token))
        .header("Accept", "application/json")
        .query(&[
            ("jql", "assignee=currentUser() AND updated>=-1d"),
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
}
