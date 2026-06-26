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
    let body = crate::net::send_retrying(
        client
            .get("https://api.github.com/search/issues")
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "qa-cockpit")
            // Quote the key for an EXACT phrase match — otherwise GitHub matches the
            // bare token (e.g. "QAT" also hits ML "quantization-aware training" PRs
            // across all of GitHub). `in:title,body` keeps it to where keys appear.
            .query(&[("q", format!("\"{key}\" in:title,body type:pr"))]),
    )?
        .error_for_status()?
        .text()?;
    parse_pr_search(&body)
}

/// Fetch the raw unified diff for a PR (`repo` = "OWNER/REPO").
/// Thin HTTP wrapper; not unit-tested.
pub fn fetch_pr_diff(token: &str, repo: &str, number: i64) -> Result<String> {
    let client = crate::net::client();
    let diff = crate::net::send_retrying(
        client
            .get(format!("https://api.github.com/repos/{repo}/pulls/{number}"))
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github.diff")
            .header("User-Agent", "qa-cockpit"),
    )?
        .error_for_status()?
        .text()?;
    Ok(diff)
}

/// Fetch PRs authored by the authenticated user.
/// Thin HTTP wrapper around `parse_prs`; not unit-tested.
pub fn fetch_my_prs(token: &str) -> Result<Vec<Pr>> {
    let client = crate::net::client();
    let body = crate::net::send_retrying(
        client
            .get("https://api.github.com/search/issues")
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "qa-cockpit")
            .query(&[("q", "author:@me type:pr")]),
    )?
        .error_for_status()?
        .text()?;
    parse_prs(&body)
}

/// Verify a GitHub token by fetching the authenticated user; returns the login.
/// Thin HTTP wrapper; not unit-tested.
pub fn fetch_user(token: &str) -> Result<String> {
    let client = crate::net::client();
    let body = crate::net::send_retrying(
        client
            .get("https://api.github.com/user")
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "qa-cockpit"),
    )?
        .error_for_status()?
        .text()?;
    let v: Value = serde_json::from_str(&body)?;
    Ok(v.get("login")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)")
        .to_string())
}

/// Extract `(OWNER/REPO, number)` from a GitHub PR url.
pub fn parse_pr_url(url: &str) -> Option<(String, i64)> {
    let rest = url.split("github.com/").nth(1)?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let pull = parts.next()?; // "pull"
    if pull != "pull" {
        return None;
    }
    let number: i64 = parts.next()?.split(['?', '#']).next()?.parse().ok()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((format!("{owner}/{repo}"), number))
}

/// Parse a GitHub `GET /repos/{repo}/pulls/{n}` response into `(title, body)`.
pub fn parse_pr_detail(json: &str) -> Result<(String, String)> {
    let v: Value = serde_json::from_str(json)?;
    let title = v.get("title").and_then(Value::as_str).unwrap_or_default().to_string();
    let body = v.get("body").and_then(Value::as_str).unwrap_or_default().to_string();
    Ok((title, body))
}

/// Fetch a PR's title + body. Thin HTTP wrapper; not unit-tested.
pub fn fetch_pr_detail(token: &str, repo: &str, number: i64) -> Result<(String, String)> {
    let client = crate::net::client();
    let body = crate::net::send_retrying(
        client
            .get(format!("https://api.github.com/repos/{repo}/pulls/{number}"))
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "qa-cockpit"),
    )?
        .error_for_status()?
        .text()?;
    parse_pr_detail(&body)
}

/// Map a ticket-summary repo tag ([GTI]/[GTG]) to its `OWNER/REPO`. Mirrors the
/// frontend's REPO_TAGS (constants.ts) — keep the two in sync.
fn repo_for_tag(tag: &str) -> Option<&'static str> {
    match tag.to_uppercase().as_str() {
        "GTI" => Some("tr8team/gotradeindoapp"),
        "GTG" => Some("tr8team/tradecharlieflutter"),
        _ => None,
    }
}

/// Resolve the PR(s) a Jira ticket refers to from its summary, using the team
/// convention: a repo tag (`[GTI]`/`[GTG]`) + `#NNNN` PR number(s), e.g.
/// "[UAT] [GTG] feat(srf): … #3250". Mirrors the frontend `pr-ref.ts`. Returns
/// (OWNER/REPO, number) pairs, deduped. Empty when the summary follows no
/// convention. Pure string parsing — unit-tested.
pub fn parse_pr_refs_from_summary(summary: &str) -> Vec<(String, i64)> {
    if summary.is_empty() {
        return vec![];
    }
    // First bracketed alphabetic tag that maps to a known repo (skips [UAT]).
    let mut repo: Option<&'static str> = None;
    let b = summary.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'[' {
            if let Some(end) = summary[i + 1..].find(']') {
                let inner = &summary[i + 1..i + 1 + end];
                if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_alphabetic()) {
                    if let Some(r) = repo_for_tag(inner) {
                        repo = Some(r);
                        break;
                    }
                }
                i = i + 1 + end + 1;
                continue;
            }
        }
        i += 1;
    }
    let Some(repo) = repo else { return vec![] };

    // All #NNNN, attributed to that repo, deduped (preserve order).
    let mut out: Vec<(String, i64)> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'#' {
            let start = i + 1;
            let mut j = start;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > start {
                if let Ok(n) = summary[start..j].parse::<i64>() {
                    if n > 0 && !out.iter().any(|(_, x)| *x == n) {
                        out.push((repo.to_string(), n));
                    }
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// One changed file extracted from a unified PR diff.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffFile {
    /// New-side path (relative to repo root, e.g. "lib/kyc/foo.dart").
    pub path: String,
    /// Lines added by the PR (without the leading `+`). Hunk headers and
    /// removed/context lines are dropped — this is the *new behavior*.
    pub added: Vec<String>,
}

/// Parse a raw unified diff (GitHub `Accept: application/vnd.github.diff`)
/// into per-file added-line sets. Deleted files (`+++ /dev/null`) are skipped
/// since they have no new path. Pure string parsing — unit-tested.
pub fn parse_diff_files(diff: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut cur: Option<DiffFile> = None;
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if let Some(f) = cur.take() {
                files.push(f);
            }
        } else if let Some(p) = line.strip_prefix("+++ b/") {
            cur = Some(DiffFile { path: p.trim().to_string(), added: Vec::new() });
        } else if line.starts_with("--- ") || line.starts_with("+++ ") {
            // file headers (incl. "+++ /dev/null") — not content
        } else if let Some(rest) = line.strip_prefix('+') {
            if let Some(f) = cur.as_mut() {
                f.added.push(rest.to_string());
            }
        }
    }
    if let Some(f) = cur.take() {
        files.push(f);
    }
    files
}

/// Build a compact, AI-friendly digest of a PR diff: only `.dart` files, with
/// each file's added lines capped. `(changed_dart_paths, digest_text)`.
/// `max_lines_per_file` / `max_total_chars` bound the size so it fits a prompt.
pub fn diff_digest(
    diff: &str,
    max_lines_per_file: usize,
    max_total_chars: usize,
) -> (Vec<String>, String) {
    let mut paths = Vec::new();
    let mut digest = String::new();
    for f in parse_diff_files(diff) {
        if !f.path.ends_with(".dart") {
            continue;
        }
        paths.push(f.path.clone());
        digest.push_str(&format!("=== {} (+{} baris) ===\n", f.path, f.added.len()));
        for l in f.added.iter().take(max_lines_per_file) {
            let l = if l.len() > 200 { &l[..200] } else { l.as_str() };
            digest.push_str(&format!("+ {}\n", l.trim_end()));
        }
        if f.added.len() > max_lines_per_file {
            digest.push_str(&format!("  … (+{} baris lagi)\n", f.added.len() - max_lines_per_file));
        }
        digest.push('\n');
        if digest.len() > max_total_chars {
            digest.truncate(max_total_chars);
            digest.push_str("\n… (diff dipotong)\n");
            break;
        }
    }
    (paths, digest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_refs_from_summary_uses_tag_and_hash() {
        assert_eq!(
            parse_pr_refs_from_summary("[UAT] [GTG] feat(srf): withdrawal msg #3250"),
            vec![("tr8team/tradecharlieflutter".to_string(), 3250)],
        );
        assert_eq!(
            parse_pr_refs_from_summary("[GTI] fix login #42"),
            vec![("tr8team/gotradeindoapp".to_string(), 42)],
        );
        // multiple numbers, deduped, all attributed to the one repo tag
        assert_eq!(
            parse_pr_refs_from_summary("[GTG] big #10 and #12 (re #10)"),
            vec![
                ("tr8team/tradecharlieflutter".to_string(), 10),
                ("tr8team/tradecharlieflutter".to_string(), 12),
            ],
        );
        // no repo tag, or no number → empty
        assert!(parse_pr_refs_from_summary("feat: something #3182").is_empty());
        assert!(parse_pr_refs_from_summary("[GTG] feat: something").is_empty());
        assert!(parse_pr_refs_from_summary("").is_empty());
    }

    #[test]
    fn parse_diff_files_extracts_new_paths_and_added_lines() {
        let diff = "\
diff --git a/lib/kyc/foo.dart b/lib/kyc/foo.dart
index 111..222 100644
--- a/lib/kyc/foo.dart
+++ b/lib/kyc/foo.dart
@@ -1,3 +1,4 @@
 unchanged line
-removed line
+added one
+added two
diff --git a/test/old.dart b/test/old.dart
deleted file mode 100644
index 333..000
--- a/test/old.dart
+++ /dev/null
@@ -1,2 +0,0 @@
-gone
diff --git a/lib/new.dart b/lib/new.dart
new file mode 100644
index 000..444
--- /dev/null
+++ b/lib/new.dart
@@ -0,0 +1,1 @@
+brand new
";
        let files = parse_diff_files(diff);
        assert_eq!(files.len(), 2, "deleted file (+++ /dev/null) skipped");
        assert_eq!(files[0].path, "lib/kyc/foo.dart");
        assert_eq!(files[0].added, vec!["added one", "added two"]);
        assert_eq!(files[1].path, "lib/new.dart");
        assert_eq!(files[1].added, vec!["brand new"]);
    }

    #[test]
    fn diff_digest_keeps_only_dart_and_caps_lines() {
        let diff = "\
diff --git a/lib/a.dart b/lib/a.dart
--- a/lib/a.dart
+++ b/lib/a.dart
@@
+l1
+l2
+l3
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@
+docs change
";
        let (paths, digest) = diff_digest(diff, 2, 10_000);
        assert_eq!(paths, vec!["lib/a.dart"], "non-dart files excluded");
        assert!(digest.contains("lib/a.dart"));
        assert!(digest.contains("+ l1") && digest.contains("+ l2"));
        assert!(!digest.contains("+ l3"), "capped at max_lines_per_file");
        assert!(digest.contains("baris lagi"));
        assert!(!digest.contains("docs change"));
    }

    #[test]
    fn parse_pr_url_extracts_repo_and_number() {
        assert_eq!(
            parse_pr_url("https://github.com/tr8team/tradecharlieflutter/pull/3197"),
            Some(("tr8team/tradecharlieflutter".to_string(), 3197))
        );
        assert_eq!(
            parse_pr_url("https://github.com/o/r/pull/12?diff=split"),
            Some(("o/r".to_string(), 12))
        );
        assert_eq!(parse_pr_url("https://github.com/o/r/issues/5"), None);
        assert_eq!(parse_pr_url("not a url"), None);
    }

    #[test]
    fn parse_pr_detail_reads_title_and_body() {
        let json = r#"{"title":"feat: x","body":"does x","number":3200}"#;
        let (t, b) = parse_pr_detail(json).unwrap();
        assert_eq!(t, "feat: x");
        assert_eq!(b, "does x");
    }

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
