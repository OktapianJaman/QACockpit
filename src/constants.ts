// ---------------------------------------------------------------------------
// App constants — the single source of truth for values that used to be
// scattered inline. Change them here without touching core logic.
// ---------------------------------------------------------------------------

import type { ConfigKey } from "./types";

/** Config keys persisted to the backend, in Settings-form order. */
export const CONFIG_KEYS: ConfigKey[] = [
  "jira_base_url",
  "jira_email",
  "jira_token",
  "jira_story_point_field",
  "jira_project",
  "jira_assignee",
  "jira_sprint_scope",
  "github_token",
  "gemini_api_key",
  "ai_language",
];

/** Fixed list of repos used by the per-ticket PR dropdown (not user-editable). */
export const KNOWN_REPOS = ["tr8team/gotradeindoapp", "tr8team/tradecharlieflutter"];

/** Maps the `[GTI]`/`[GTG]` tag in a Jira ticket summary to its GitHub repo.
 *  GTI = Gotradeindonesia, GTG = Tradecharlie. Used to auto-resolve the PR a
 *  ticket refers to (e.g. "[GTG] … #3182" → tr8team/tradecharlieflutter#3182). */
export const REPO_TAGS: Record<string, string> = {
  GTI: "tr8team/gotradeindoapp",
  GTG: "tr8team/tradecharlieflutter",
};

/** localStorage key for the persisted light/dark theme. */
export const THEME_KEY = "qacockpit-theme";

// Preferred left-to-right board column order. A status fills a slot if, case-
// insensitive, it equals or contains the keyword. Order matters: more specific
// keywords (e.g. "qa in progress") must come before broader ones ("in progress").
export const STATUS_ORDER = [
  "to do",
  "ready for qa",
  "today",
  "qa in progress",
  "in progress",
  "qa passed",
  "qa failed",
  "done",
];

// Terminal/closed statuses collapse into a single "Done" column.
export const DONE_KEYWORDS = ["done", "passed", "closed", "complete", "resolved", "selesai"];

// Canonical QA columns that always render — even with zero tickets — so they're
// always available as drag-and-drop targets. A status already present (any case)
// keeps its real Jira name; missing ones are added from here.
export const ALWAYS_COLUMNS = ["Ready for QA", "Today", "QA In Progress", "Done"];
