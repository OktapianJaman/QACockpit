use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Pr {
    pub number: i64,
    pub repo: String,
    pub title: String,
    pub state: String,
    pub url: String,
    pub updated: String,
}

/// Derive `OWNER/REPO` from a GitHub API repository_url
/// (e.g. "https://api.github.com/repos/OWNER/REPO").
fn repo_from_url(repository_url: &str) -> String {
    let mut segs = repository_url
        .trim_end_matches('/')
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>();
    // rsplit yields REPO then OWNER; reverse to OWNER, REPO.
    segs.reverse();
    segs.join("/")
}

/// Parse a GitHub `/search/issues` response body into pull requests.
pub fn parse_prs(json: &str) -> Result<Vec<Pr>> {
    let root: Value = serde_json::from_str(json)?;
    let items = root
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut prs = Vec::with_capacity(items.len());
    for item in items {
        let number = item.get("number").and_then(Value::as_i64).unwrap_or_default();
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let state = item
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let url = item
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let updated = item
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let repo = item
            .get("repository_url")
            .and_then(Value::as_str)
            .map(repo_from_url)
            .unwrap_or_default();

        prs.push(Pr {
            number,
            repo,
            title,
            state,
            url,
            updated,
        });
    }
    Ok(prs)
}

/// A pull request referenced by a ticket, as returned by `/search/issues`.
/// Leaner than [`Pr`] (no `updated`); used by the on-demand PR tab.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PrRef {
    pub number: i64,
    pub repo: String,
    pub title: String,
    pub state: String,
    pub url: String,
}

/// Parse a GitHub `/search/issues` response into [`PrRef`]s. Reuses the same
/// repo-derivation as [`parse_prs`].
pub fn parse_pr_search(json: &str) -> Result<Vec<PrRef>> {
    let root: Value = serde_json::from_str(json)?;
    let items = root
        .get("items")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    let mut prs = Vec::with_capacity(items.len());
    for item in items {
        let number = item.get("number").and_then(Value::as_i64).unwrap_or_default();
        let title = item
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let state = item
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let url = item
            .get("html_url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let repo = item
            .get("repository_url")
            .and_then(Value::as_str)
            .map(repo_from_url)
            .unwrap_or_default();

        prs.push(PrRef {
            number,
            repo,
            title,
            state,
            url,
        });
    }
    Ok(prs)
}

/// Search GitHub for PRs that mention a ticket key (e.g. branch/title/body).
/// Thin HTTP wrapper around `parse_pr_search`; not unit-tested.
pub fn search_prs_for_key(token: &str, key: &str) -> Result<Vec<PrRef>> {
    let client = crate::net::client();
    let body = client
        .get("https://api.github.com/search/issues")
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "qa-cockpit")
        // Quote the key for an EXACT phrase match — otherwise GitHub matches the
        // bare token (e.g. "QAT" also hits ML "quantization-aware training" PRs
        // across all of GitHub). `in:title,body` keeps it to where keys appear.
        .query(&[("q", format!("\"{key}\" in:title,body type:pr"))])
        .send()?
        .error_for_status()?
        .text()?;
    parse_pr_search(&body)
}

/// Fetch the raw unified diff for a PR (`repo` = "OWNER/REPO").
/// Thin HTTP wrapper; not unit-tested.
pub fn fetch_pr_diff(token: &str, repo: &str, number: i64) -> Result<String> {
    let client = crate::net::client();
    let diff = client
        .get(format!("https://api.github.com/repos/{repo}/pulls/{number}"))
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github.diff")
        .header("User-Agent", "qa-cockpit")
        .send()?
        .error_for_status()?
        .text()?;
    Ok(diff)
}

/// Fetch PRs authored by the authenticated user.
/// Thin HTTP wrapper around `parse_prs`; not unit-tested.
pub fn fetch_my_prs(token: &str) -> Result<Vec<Pr>> {
    let client = crate::net::client();
    let body = client
        .get("https://api.github.com/search/issues")
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "qa-cockpit")
        .query(&[("q", "author:@me type:pr")])
        .send()?
        .error_for_status()?
        .text()?;
    parse_prs(&body)
}

/// Verify a GitHub token by fetching the authenticated user; returns the login.
/// Thin HTTP wrapper; not unit-tested.
pub fn fetch_user(token: &str) -> Result<String> {
    let client = crate::net::client();
    let body = client
        .get("https://api.github.com/user")
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "qa-cockpit")
        .send()?
        .error_for_status()?
        .text()?;
    let v: Value = serde_json::from_str(&body)?;
    Ok(v.get("login")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured fixture mimicking a GitHub /search/issues response (type:pr).
    // Two items in different repos to exercise the `repo` derivation.
    const FIXTURE: &str = r#"{
      "total_count": 2,
      "items": [
        {
          "number": 42,
          "title": "Add retry to flaky e2e test",
          "state": "open",
          "html_url": "https://github.com/tr8-io/qa-cockpit/pull/42",
          "updated_at": "2026-06-18T11:00:00Z",
          "repository_url": "https://api.github.com/repos/tr8-io/qa-cockpit"
        },
        {
          "number": 7,
          "title": "Fix selector for new modal",
          "state": "closed",
          "html_url": "https://github.com/OktapianJaman/katalon-utils/pull/7",
          "updated_at": "2026-06-15T09:20:00Z",
          "repository_url": "https://api.github.com/repos/OktapianJaman/katalon-utils"
        }
      ]
    }"#;

    #[test]
    fn parses_prs_and_derives_repo() {
        let prs = parse_prs(FIXTURE).unwrap();
        assert_eq!(prs.len(), 2);

        assert_eq!(
            prs[0],
            Pr {
                number: 42,
                repo: "tr8-io/qa-cockpit".to_string(),
                title: "Add retry to flaky e2e test".to_string(),
                state: "open".to_string(),
                url: "https://github.com/tr8-io/qa-cockpit/pull/42".to_string(),
                updated: "2026-06-18T11:00:00Z".to_string(),
            }
        );

        assert_eq!(prs[1].number, 7);
        assert_eq!(prs[1].repo, "OktapianJaman/katalon-utils");
        assert_eq!(prs[1].title, "Fix selector for new modal");
        assert_eq!(prs[1].state, "closed");
        assert_eq!(prs[1].url, "https://github.com/OktapianJaman/katalon-utils/pull/7");
        assert_eq!(prs[1].updated, "2026-06-15T09:20:00Z");
    }

    #[test]
    fn parse_pr_search_derives_repo_for_two_items() {
        let prs = parse_pr_search(FIXTURE).unwrap();
        assert_eq!(prs.len(), 2);

        assert_eq!(
            prs[0],
            PrRef {
                number: 42,
                repo: "tr8-io/qa-cockpit".to_string(),
                title: "Add retry to flaky e2e test".to_string(),
                state: "open".to_string(),
                url: "https://github.com/tr8-io/qa-cockpit/pull/42".to_string(),
            }
        );

        assert_eq!(prs[1].number, 7);
        assert_eq!(prs[1].repo, "OktapianJaman/katalon-utils");
        assert_eq!(prs[1].title, "Fix selector for new modal");
        assert_eq!(prs[1].state, "closed");
        assert_eq!(prs[1].url, "https://github.com/OktapianJaman/katalon-utils/pull/7");
    }
}
