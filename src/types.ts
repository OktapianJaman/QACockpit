// ---------------------------------------------------------------------------
// Backend types (mirror src-tauri/src/commands.rs — serde defaults to
// snake_case Rust field names, so match them exactly).
// ---------------------------------------------------------------------------

export interface BoardTicket {
  key: string;
  summary: string;
  status: string;
  story_points: number | null;
}

export interface TestCase {
  id: number;
  ticket_key: string;
  title: string;
  steps: string;
  expected: string;
  status: string;
  notes: string;
}

export interface ChatMsg {
  role: "user" | "assistant";
  content: string;
  /** Attached screenshots (data: URLs), shown in the user's bubble. */
  images?: string[];
}

export interface PrRef {
  number: number;
  repo: string;
  title: string;
  state: string;
  url: string;
  /** Follow-up Q&A about this PR (loaded from + persisted to the DB). */
  chat?: ChatMsg[];
  /** Cached AI summary ("Ringkas + apa yang dites"), loaded from the DB. */
  summary?: string;
}

export interface JiraField {
  id: string;
  name: string;
}

export interface JiraProject {
  key: string;
  name: string;
}

export interface JiraUser {
  account_id: string;
  display_name: string;
}

export interface JiraTransition {
  id: string;
  name: string;
  to_status: string;
}

export interface AppConfig {
  jira_base_url: string;
  jira_email: string;
  jira_token: string;
  jira_story_point_field: string;
  jira_project: string;
  jira_assignee: string;
  jira_sprint_scope: string;
  github_token: string;
  gemini_api_key: string;
  ai_language: string;
}
