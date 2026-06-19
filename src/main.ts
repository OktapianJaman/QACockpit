import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Backend types (mirror src-tauri/src/commands.rs — serde defaults to
// snake_case Rust field names, so match them exactly).
// ---------------------------------------------------------------------------

interface BoardTicket {
  key: string;
  summary: string;
  status: string;
  story_points: number | null;
}

interface TestCase {
  id: number;
  ticket_key: string;
  title: string;
  steps: string;
  expected: string;
  status: string;
}

interface PrRef {
  number: number;
  repo: string;
  title: string;
  state: string;
  url: string;
}

interface JiraField {
  id: string;
  name: string;
}

interface JiraProject {
  key: string;
  name: string;
}

interface JiraUser {
  account_id: string;
  display_name: string;
}

interface JiraTransition {
  id: string;
  name: string;
  to_status: string;
}

interface AppConfig {
  jira_base_url: string;
  jira_email: string;
  jira_token: string;
  jira_story_point_field: string;
  jira_project: string;
  jira_assignee: string;
  jira_sprint_scope: string;
  github_token: string;
  gemma_model: string;
}

const CONFIG_KEYS: (keyof AppConfig)[] = [
  "jira_base_url",
  "jira_email",
  "jira_token",
  "jira_story_point_field",
  "jira_project",
  "jira_assignee",
  "jira_sprint_scope",
  "github_token",
  "gemma_model",
];

// ---------------------------------------------------------------------------
// DOM helpers
// ---------------------------------------------------------------------------

function $<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`element #${id} tidak ditemukan`);
  return el as T;
}

function show(el: HTMLElement, visible: boolean): void {
  el.classList.toggle("hidden", !visible);
}

/** Escape text destined for innerHTML interpolation. */
function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/**
 * Render AI text as minimal, safe HTML: escape first, then turn `**bold**`
 * into <strong> and preserve line breaks. No other markup is interpreted.
 */
function mdToHtml(s: string): string {
  return esc(s)
    .replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>")
    .replace(/\n/g, "<br>");
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/** Round a points value to 1 decimal place, dropping a trailing .0. */
function fmtPoints(n: number): string {
  const r = Math.round(n * 10) / 10;
  return Number.isInteger(r) ? String(r) : r.toFixed(1);
}

// ---------------------------------------------------------------------------
// Toast / errors
// ---------------------------------------------------------------------------

let toastTimer: number | undefined;

function toast(msg: string, kind: "info" | "error" = "info"): void {
  const el = $("toast");
  el.textContent = msg;
  el.classList.remove("error", "info");
  el.classList.add(kind);
  show(el, true);
  if (toastTimer) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => show(el, false), kind === "error" ? 6000 : 3500);
}

function errStr(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

// ---------------------------------------------------------------------------
// Kanban board
// ---------------------------------------------------------------------------

// Preferred left-to-right column order. A status fills a slot if, case-
// insensitive, it equals or contains the keyword. Order matters: more specific
// keywords (e.g. "qa in progress") must come before broader ones ("in progress").
const STATUS_ORDER = [
  "to do",
  "ready for qa",
  "qa in progress",
  "in progress",
  "qa passed",
  "qa failed",
  "done",
];

let boardTickets: BoardTicket[] = [];
let boardSearch = "";

/** Rank a status by the preferred order; unmatched statuses rank last. */
function statusRank(status: string): number {
  const s = status.toLowerCase();
  for (let i = 0; i < STATUS_ORDER.length; i++) {
    const kw = STATUS_ORDER[i];
    if (s === kw || s.includes(kw)) return i;
  }
  return STATUS_ORDER.length;
}

function pointsLabel(pts: number | null): string {
  return pts == null ? "— pts" : `${fmtPoints(pts)} pts`;
}

/** Build one table row for a ticket (click key/title → detail; inline points;
 *  status button → transition picker). No drag — WKWebView DnD is unreliable. */
function buildRow(t: BoardTicket): HTMLElement {
  const tr = document.createElement("tr");
  tr.className = "brow";
  tr.innerHTML = `
    <td class="bk mono">${esc(t.key)}</td>
    <td class="bj">${esc(t.summary || "—")}</td>
    <td class="bp"><button class="ct-points" type="button" title="Klik untuk ubah story point">${esc(
      pointsLabel(t.story_points)
    )}</button></td>
    <td class="bs"><button class="status-btn" type="button" title="Klik untuk ganti status">${esc(
      t.status || "—"
    )} ▾</button></td>`;

  tr.querySelector(".bk")?.addEventListener("click", () => void openDetail(t.key));
  tr.querySelector(".bj")?.addEventListener("click", () => void openDetail(t.key));
  const ptsBtn = tr.querySelector<HTMLButtonElement>(".ct-points");
  ptsBtn?.addEventListener("click", () => startPointEdit(t, ptsBtn));
  tr.querySelector<HTMLButtonElement>(".status-btn")?.addEventListener(
    "click",
    () => void shiftStatus(t.key)
  );
  return tr;
}

/** Turn the points badge into a number input; commit on Enter/blur. */
function startPointEdit(t: BoardTicket, badge: HTMLButtonElement): void {
  const input = document.createElement("input");
  input.type = "number";
  input.className = "ct-points-input";
  input.step = "0.5";
  input.min = "0";
  input.value = t.story_points == null ? "" : String(t.story_points);
  badge.replaceWith(input);
  input.focus();
  input.select();

  let committed = false;
  const commit = (): void => {
    if (committed) return;
    committed = true;
    const raw = input.value.trim();
    const points = raw === "" ? null : Number(raw);
    if (points !== null && Number.isNaN(points)) {
      void refreshBoard();
      return;
    }
    void savePoints(t.key, points);
  };

  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      input.blur();
    } else if (e.key === "Escape") {
      committed = true;
      void refreshBoard();
    }
  });
  input.addEventListener("blur", commit);
}

async function savePoints(key: string, points: number | null): Promise<void> {
  try {
    await invoke("set_story_points", { key, points });
    toast(`Poin ${key} → ${points == null ? "—" : fmtPoints(points)}`);
  } catch (e) {
    toast(`Gagal simpan poin: ${errStr(e)}`, "error");
  }
  await refreshBoard();
}

/** Render the ticket table, grouped/sorted by status and filtered by search. */
function renderBoard(tickets: BoardTicket[]): void {
  const board = $("board");
  show($("board-empty"), tickets.length === 0);
  if (tickets.length === 0) {
    board.innerHTML = "";
    return;
  }

  const q = boardSearch.trim().toLowerCase();
  const rows = tickets
    .filter(
      (t) => !q || t.key.toLowerCase().includes(q) || t.summary.toLowerCase().includes(q)
    )
    .sort((a, b) => {
      const r = statusRank(a.status) - statusRank(b.status);
      return r !== 0 ? r : a.key.localeCompare(b.key);
    });

  board.innerHTML = `
    <table class="board-table">
      <thead>
        <tr><th>Tiket</th><th>Judul</th><th class="num">Poin</th><th>Status</th></tr>
      </thead>
      <tbody id="board-tbody"></tbody>
    </table>`;
  const tb = $("board-tbody");
  if (rows.length === 0) {
    tb.innerHTML = `<tr><td colspan="4" class="board-noresult">Nggak ada tiket yang cocok.</td></tr>`;
    return;
  }
  for (const t of rows) tb.appendChild(buildRow(t));
}

async function refreshBoard(): Promise<void> {
  try {
    boardTickets = await invoke<BoardTicket[]>("list_board_tickets");
    renderBoard(boardTickets);
  } catch (e) {
    toast(`Gagal memuat board: ${errStr(e)}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async function doRefresh(): Promise<void> {
  const btn = $<HTMLButtonElement>("refresh-btn");
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Refresh…";
  try {
    await refreshBoard();
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

async function doSync(): Promise<void> {
  const btn = $<HTMLButtonElement>("sync-btn");
  btn.disabled = true;
  btn.textContent = "Sync…";
  try {
    const res = await invoke<{ tickets: number; prs: number }>("sync_now");
    toast(`Sync beres: ${res.tickets} tiket, ${res.prs} PR.`);
    await refreshBoard();
  } catch (e) {
    toast(`Sync gagal: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.textContent = "Sync";
  }
}

// ---------------------------------------------------------------------------
// Confirm dialog + Jira status transitions
// ---------------------------------------------------------------------------

// Resolver for the currently-open confirm dialog (window.confirm is unreliable
// in Tauri, so we roll our own promise-based modal). Listeners wired once.
let confirmResolve: ((ok: boolean) => void) | null = null;

function settleConfirm(ok: boolean): void {
  show($("confirm-overlay"), false);
  const r = confirmResolve;
  confirmResolve = null;
  if (r) r(ok);
}

/** Show the confirm modal; resolves true on OK, false on Cancel/backdrop. */
function confirmDialog(message: string): Promise<boolean> {
  // If one is already open, cancel it first.
  if (confirmResolve) settleConfirm(false);
  $("confirm-msg").textContent = message;
  show($("confirm-overlay"), true);
  return new Promise<boolean>((resolve) => {
    confirmResolve = resolve;
  });
}

function closeTransitionPicker(): void {
  show($("transition-overlay"), false);
  $("transition-list").innerHTML = "";
}

/** Entry point from a ticket's "shift status" action: fetch transitions and
 *  let the user pick one, confirm, then perform it and refresh. */
async function shiftStatus(key: string): Promise<void> {
  let trans: JiraTransition[];
  try {
    trans = await invoke<JiraTransition[]>("list_transitions", { key });
  } catch (e) {
    toast(`Gagal ambil transisi: ${errStr(e)}`, "error");
    return;
  }
  if (trans.length === 0) {
    toast(`Tidak ada transisi tersedia untuk ${key}.`);
    return;
  }
  showTransitionPicker(key, trans);
}

/** Render the pick-transition modal with one button per transition. */
function showTransitionPicker(key: string, trans: JiraTransition[]): void {
  $("transition-title").textContent = `Geser status ${key}`;
  const list = $("transition-list");
  list.innerHTML = trans
    .map(
      (t, i) =>
        `<button class="btn" data-idx="${i}">${esc(t.name)}${
          t.to_status ? " → " + esc(t.to_status) : ""
        }</button>`
    )
    .join("");
  list.querySelectorAll<HTMLButtonElement>(".btn").forEach((btn) => {
    btn.addEventListener("click", () => {
      const idx = Number(btn.dataset.idx);
      const t = trans[idx];
      if (t) void onPickTransition(key, t);
    });
  });
  show($("transition-overlay"), true);
}

/** Confirm and perform a chosen transition, then refresh the board. */
async function onPickTransition(key: string, t: JiraTransition): Promise<void> {
  closeTransitionPicker();
  const target = t.to_status || t.name;
  const ok = await confirmDialog(
    `Geser ${key} ke "${target}"? Ini mengubah status di Jira beneran.`
  );
  if (!ok) return;
  try {
    // Tauri maps snake_case command params (transition_id) to camelCase.
    await invoke("transition_issue", { key, transitionId: t.id });
    toast(`Status ${key} diubah.`);
    await refreshBoard();
  } catch (e) {
    toast(`Gagal ubah status: ${errStr(e)}`, "error");
  }
}

// ---------------------------------------------------------------------------
// Ticket detail modal + test cases
// ---------------------------------------------------------------------------

// The ticket whose detail modal is currently open (null = closed).
let detailKey: string | null = null;

/** Find a loaded board ticket by key (module already holds them). */
function ticketByKey(key: string): BoardTicket | undefined {
  return boardTickets.find((t) => t.key === key);
}

function closeDetail(): void {
  detailKey = null;
  show($("detail-overlay"), false);
  show($("tc-add-form"), false);
}

/** Switch the detail modal between the "testcases" and "pr" tabs. */
function selectTab(tab: "testcases" | "pr"): void {
  for (const t of ["testcases", "pr"] as const) {
    $(`tab-${t}`).classList.toggle("active", t === tab);
    show($(`panel-${t}`), t === tab);
  }
}

/** Open the detail modal for a ticket: header, summary, status, test cases. */
async function openDetail(key: string): Promise<void> {
  detailKey = key;
  const t = ticketByKey(key);
  $("detail-key").textContent = key;
  $("detail-summary").textContent = t?.summary || "—";
  const statusEl = $("detail-status");
  statusEl.textContent = t?.status || "—";
  show($("tc-add-form"), false);
  ($("tc-add-form") as HTMLFormElement).reset();
  $("tc-list").innerHTML = "";
  show($("tc-empty"), false);
  $("tc-counter").textContent = "";
  // Reset the PR tab to its empty state and default back to Test Cases.
  $("pr-list").innerHTML = "";
  show($("pr-empty"), true);
  $("pr-empty").textContent = "Belum dicari. Klik Cari PR.";
  selectTab("testcases");
  show($("detail-overlay"), true);
  await loadTestCases(key);
}

/** Pill class for a test-case status. */
function tcStatusClass(status: string): string {
  if (status === "passed") return "tc-pill passed";
  if (status === "failed") return "tc-pill failed";
  return "tc-pill untested";
}

function tcStatusLabel(status: string): string {
  if (status === "passed") return "✅ passed";
  if (status === "failed") return "❌ failed";
  return "untested";
}

/** Render the test-case list + counter for the open ticket. */
function renderTestCases(cases: TestCase[]): void {
  const list = $("tc-list");
  show($("tc-empty"), cases.length === 0);

  const passed = cases.filter((c) => c.status === "passed").length;
  const failed = cases.filter((c) => c.status === "failed").length;
  $("tc-counter").textContent =
    cases.length === 0
      ? ""
      : `${cases.length} test case · ${passed} ✅ · ${failed} ❌`;

  list.innerHTML = "";
  for (const c of cases) {
    const item = document.createElement("div");
    item.className = "tc-item";
    item.innerHTML = `
      <div class="tc-item-head">
        <span class="${tcStatusClass(c.status)}">${esc(tcStatusLabel(c.status))}</span>
        <span class="tc-title">${esc(c.title)}</span>
      </div>
      ${c.steps ? `<div class="tc-field"><span class="tc-label">Langkah:</span> ${esc(c.steps)}</div>` : ""}
      ${c.expected ? `<div class="tc-field"><span class="tc-label">Harapan:</span> ${esc(c.expected)}</div>` : ""}
      <div class="tc-item-actions">
        <button class="btn small tc-pass" type="button">✅ Pass</button>
        <button class="btn small tc-fail" type="button">❌ Fail</button>
        <button class="btn small tc-del" type="button" title="Hapus">🗑</button>
      </div>`;

    item.querySelector<HTMLButtonElement>(".tc-pass")?.addEventListener("click", () =>
      void setTestCaseStatus(c.id, "passed")
    );
    item.querySelector<HTMLButtonElement>(".tc-fail")?.addEventListener("click", () =>
      void setTestCaseStatus(c.id, "failed")
    );
    item.querySelector<HTMLButtonElement>(".tc-del")?.addEventListener("click", () =>
      void deleteTestCase(c.id, c.title)
    );

    list.appendChild(item);
  }
}

/** Load + render the test cases for a key (no-op if the modal moved on). */
async function loadTestCases(key: string): Promise<void> {
  try {
    const cases = await invoke<TestCase[]>("list_test_cases", { key });
    if (detailKey === key) renderTestCases(cases);
  } catch (e) {
    toast(`Gagal memuat test case: ${errStr(e)}`, "error");
  }
}

async function setTestCaseStatus(id: number, status: string): Promise<void> {
  if (!detailKey) return;
  try {
    await invoke("set_test_case_status", { id, status });
  } catch (e) {
    toast(`Gagal ubah status test case: ${errStr(e)}`, "error");
  }
  await loadTestCases(detailKey);
}

async function deleteTestCase(id: number, title: string): Promise<void> {
  if (!detailKey) return;
  const ok = await confirmDialog(`Hapus test case "${title}"?`);
  if (!ok) return;
  try {
    await invoke("delete_test_case", { id });
    toast("Test case dihapus.");
  } catch (e) {
    toast(`Gagal hapus test case: ${errStr(e)}`, "error");
  }
  await loadTestCases(detailKey);
}

/** Submit the manual add form for the open ticket. */
async function addTestCase(e: Event): Promise<void> {
  e.preventDefault();
  if (!detailKey) return;
  const title = ($("tc-title") as HTMLInputElement).value.trim();
  if (!title) {
    toast("Judul test case wajib diisi.", "error");
    return;
  }
  const steps = ($("tc-steps") as HTMLInputElement).value.trim();
  const expected = ($("tc-expected") as HTMLInputElement).value.trim();
  try {
    await invoke("add_test_case", { key: detailKey, title, steps, expected });
    ($("tc-add-form") as HTMLFormElement).reset();
    show($("tc-add-form"), false);
    toast("Test case ditambahkan.");
  } catch (err) {
    toast(`Gagal tambah test case: ${errStr(err)}`, "error");
  }
  await loadTestCases(detailKey);
}

/** "✨ Generate pakai AI": draft cases from the ticket summary (slow, local). */
async function generateTestCases(): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const summary = ticketByKey(key)?.summary || "";
  const btn = $<HTMLButtonElement>("tc-generate");
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Lagi bikin test case… (model lokal, agak lama)";
  try {
    const cases = await invoke<TestCase[]>("generate_test_cases", { key, summary });
    if (detailKey === key) renderTestCases(cases);
    toast("Test case dibuat oleh AI.");
  } catch (e) {
    toast(`Gagal generate: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

// ---------------------------------------------------------------------------
// PR tab (find a ticket's PRs + on-demand AI review)
// ---------------------------------------------------------------------------

/** CSS class for a PR state chip. */
function prStateClass(state: string): string {
  const s = state.toLowerCase();
  if (s === "open") return "pr-state open";
  if (s === "closed") return "pr-state closed";
  return "pr-state";
}

/** Render the searched PRs, each with a "summarize" button. */
function renderPrs(prs: PrRef[]): void {
  const list = $("pr-list");
  list.innerHTML = "";

  if (prs.length === 0) {
    show($("pr-empty"), true);
    $("pr-empty").textContent = detailKey
      ? `Nggak nemu PR yang nyebut ${detailKey} di GitHub.`
      : "Nggak nemu PR.";
    return;
  }
  show($("pr-empty"), false);

  for (const pr of prs) {
    const item = document.createElement("div");
    item.className = "pr-item";
    item.innerHTML = `
      <div class="pr-item-head">
        <span class="pr-ref mono">#${pr.number} · ${esc(pr.repo)}</span>
        <span class="${prStateClass(pr.state)}">${esc(pr.state)}</span>
      </div>
      <span class="pr-title">${esc(pr.title)}</span>
      <button class="btn small primary pr-summarize" type="button">✨ Ringkas + apa yang dites</button>
      <div class="pr-review hidden"></div>`;

    const btn = item.querySelector<HTMLButtonElement>(".pr-summarize");
    const panel = item.querySelector<HTMLDivElement>(".pr-review");
    btn?.addEventListener("click", () => void summarizePr(pr, btn, panel!));

    list.appendChild(item);
  }
}

/** Summarize a PR pasted as a GitHub URL (reliable alternative to auto-search). */
async function summarizeFromLink(): Promise<void> {
  const input = $<HTMLInputElement>("pr-link");
  const url = input.value.trim();
  const m = url.match(/github\.com\/([^/\s]+)\/([^/\s]+)\/pull\/(\d+)/i);
  if (!m) {
    toast("Link PR-nya nggak valid. Contoh: https://github.com/owner/repo/pull/123", "error");
    return;
  }
  const pr: PrRef = {
    number: Number(m[3]),
    repo: `${m[1]}/${m[2]}`,
    title: `PR #${m[3]}`,
    state: "",
    url,
  };
  renderPrs([pr]);
  const btn = $("pr-list").querySelector<HTMLButtonElement>(".pr-summarize");
  const panel = $("pr-list").querySelector<HTMLDivElement>(".pr-review");
  if (btn && panel) await summarizePr(pr, btn, panel);
}

/** "🔍 Cari PR": search GitHub for PRs that mention the ticket key. */
async function searchPrs(): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const btn = $<HTMLButtonElement>("pr-search");
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Lagi cari PR…";
  try {
    const prs = await invoke<PrRef[]>("list_ticket_prs", { key });
    if (detailKey === key) renderPrs(prs);
  } catch (e) {
    toast(errStr(e), "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

/** Fetch a PR's diff and render the local-model summary / what-to-test. */
async function summarizePr(
  pr: PrRef,
  btn: HTMLButtonElement,
  panel: HTMLDivElement
): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const summary = ticketByKey(key)?.summary || "";
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Lagi baca PR…";
  panel.classList.remove("hidden");
  panel.classList.add("loading");
  panel.textContent = "Lagi baca PR & nyusun… (model lokal, agak lama)";
  try {
    const review = await invoke<string>("summarize_pr", {
      key,
      summary,
      repo: pr.repo,
      number: pr.number,
    });
    panel.classList.remove("loading");
    panel.innerHTML = mdToHtml(review);
  } catch (e) {
    panel.classList.add("hidden");
    toast(errStr(e), "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

async function populateModelDropdown(current: string): Promise<void> {
  const sel = $("cfg-gemma_model") as HTMLSelectElement;
  const hint = $("gemma-hint");
  let models: string[] = [];
  try {
    models = await invoke<string[]>("list_models");
  } catch {
    models = [];
  }
  // Always keep the saved value selectable even if LM Studio is offline.
  if (current && !models.includes(current)) models.unshift(current);
  if (models.length === 0) {
    sel.innerHTML = `<option value="">(LM Studio tidak terdeteksi)</option>`;
    hint.textContent = "Nyalakan LM Studio lalu buka Settings lagi untuk memuat daftar model.";
  } else {
    sel.innerHTML = models
      .map((m) => `<option value="${esc(m)}"${m === current ? " selected" : ""}>${esc(m)}</option>`)
      .join("");
    hint.textContent = "Daftar model diambil dari LM Studio.";
  }
}

// The three Jira selects need their saved value shown even before "Muat dari
// Jira" is clicked, so saving never loses it. They are excluded from the generic
// loop (like gemma_model) and seeded by these helpers — an empty <select> must
// have options before its value can be set.
const JIRA_DROPDOWN_KEYS: (keyof AppConfig)[] = [
  "jira_story_point_field",
  "jira_project",
  "jira_assignee",
];

const DEFAULT_STORY_POINT_FIELD = "customfield_10016";

/** Seed the story-point-field select with just the saved id (so save round-trips). */
function seedFieldDropdown(current: string): void {
  const sel = $("cfg-jira_story_point_field") as HTMLSelectElement;
  const id = current || DEFAULT_STORY_POINT_FIELD;
  sel.innerHTML = `<option value="${esc(id)}" selected>${esc(id)}</option>`;
}

/** Seed the project select with an empty "(semua project)" + the saved key. */
function seedProjectDropdown(current: string): void {
  const sel = $("cfg-jira_project") as HTMLSelectElement;
  const opts = [`<option value=""${current ? "" : " selected"}>(semua project)</option>`];
  if (current) opts.push(`<option value="${esc(current)}" selected>${esc(current)}</option>`);
  sel.innerHTML = opts.join("");
}

/** Seed the assignee select with an empty "(kamu sendiri)" + the saved id. */
function seedAssigneeDropdown(current: string): void {
  const sel = $("cfg-jira_assignee") as HTMLSelectElement;
  const opts = [`<option value=""${current ? "" : " selected"}>(kamu sendiri)</option>`];
  if (current) opts.push(`<option value="${esc(current)}" selected>${esc(current)}</option>`);
  sel.innerHTML = opts.join("");
}

async function openSettings(): Promise<void> {
  try {
    const cfg = await invoke<AppConfig>("get_config");
    for (const k of CONFIG_KEYS) {
      if (k === "gemma_model") continue; // handled as a dropdown below
      if (JIRA_DROPDOWN_KEYS.includes(k)) continue; // seeded via dedicated helpers
      ($(`cfg-${k}`) as HTMLInputElement).value = cfg[k] ?? "";
    }
    seedFieldDropdown(cfg.jira_story_point_field ?? "");
    seedProjectDropdown(cfg.jira_project ?? "");
    seedAssigneeDropdown(cfg.jira_assignee ?? "");
    $("jira-fields-hint").textContent = "";
    await populateModelDropdown(cfg.gemma_model ?? "");
  } catch (e) {
    toast(`Gagal muat pengaturan: ${errStr(e)}`, "error");
  }
  show($("settings-overlay"), true);

  // Auto-load the Jira dropdowns if creds are present, so the user doesn't have
  // to click "Muat dari Jira" every time they open Settings.
  try {
    const cfg = await invoke<AppConfig>("get_config");
    if (cfg.jira_base_url && cfg.jira_email && cfg.jira_token) {
      await loadFromJira();
    }
  } catch {
    /* seeded values remain; ignore */
  }
}

function closeSettings(): void {
  show($("settings-overlay"), false);
}

/** Read the settings form into an AppConfig (works for <input> and <select>). */
function readConfigFromForm(): AppConfig {
  const cfg = {} as AppConfig;
  for (const k of CONFIG_KEYS) {
    cfg[k] = ($(`cfg-${k}`) as HTMLInputElement | HTMLSelectElement).value.trim();
  }
  if (!cfg.jira_story_point_field) cfg.jira_story_point_field = DEFAULT_STORY_POINT_FIELD;
  return cfg;
}

async function saveSettings(): Promise<void> {
  const cfg = readConfigFromForm();
  try {
    await invoke("set_config", { cfg });
    toast("Pengaturan tersimpan.");
    closeSettings();
  } catch (e) {
    toast(`Gagal simpan pengaturan: ${errStr(e)}`, "error");
  }
}

/** Repopulate the assignee select for a given project (empty project → "kamu sendiri" only). */
async function loadAssignees(project: string, current: string): Promise<void> {
  const sel = $("cfg-jira_assignee") as HTMLSelectElement;
  const users = await invoke<JiraUser[]>("list_jira_assignees", { project });
  const opts = [`<option value=""${current ? "" : " selected"}>(kamu sendiri)</option>`];
  let matched = false;
  for (const u of users) {
    const isSel = u.account_id === current;
    if (isSel) matched = true;
    opts.push(
      `<option value="${esc(u.account_id)}"${isSel ? " selected" : ""}>${esc(u.display_name)}</option>`
    );
  }
  // Keep a saved assignee that isn't in the (project-scoped) list selectable.
  if (current && !matched) {
    opts.push(`<option value="${esc(current)}" selected>${esc(current)}</option>`);
  }
  sel.innerHTML = opts.join("");
}

/** "Muat dari Jira": save creds first, then populate the 3 dropdowns from Jira. */
async function loadFromJira(): Promise<void> {
  const btn = $<HTMLButtonElement>("jira-load-btn");
  const prev = btn.textContent;
  btn.disabled = true;
  btn.textContent = "Memuat…";
  try {
    // Persist the form so the backend has the latest creds before we fetch.
    const cfg = readConfigFromForm();
    await invoke("set_config", { cfg });

    // --- Story point fields ---
    const fields = await invoke<JiraField[]>("list_jira_fields");
    const fieldSel = $("cfg-jira_story_point_field") as HTMLSelectElement;
    const savedField = cfg.jira_story_point_field;
    let fieldMatched = false;
    const fieldOpts = fields.map((f) => {
      const isSel = f.id === savedField;
      if (isSel) fieldMatched = true;
      return `<option value="${esc(f.id)}"${isSel ? " selected" : ""}>${esc(f.name)} (${esc(f.id)})</option>`;
    });
    if (savedField && !fieldMatched) {
      fieldOpts.unshift(`<option value="${esc(savedField)}" selected>${esc(savedField)}</option>`);
    }
    fieldSel.innerHTML = fieldOpts.join("");
    $("jira-fields-hint").textContent =
      "Pilih field yang menyimpan story point (mis. Actual sprint point).";

    // --- Projects ---
    const projects = await invoke<JiraProject[]>("list_jira_projects");
    const projSel = $("cfg-jira_project") as HTMLSelectElement;
    const savedProject = cfg.jira_project;
    const projOpts = [
      `<option value=""${savedProject ? "" : " selected"}>(semua project)</option>`,
    ];
    let projMatched = false;
    for (const p of projects) {
      const isSel = p.key === savedProject;
      if (isSel) projMatched = true;
      projOpts.push(
        `<option value="${esc(p.key)}"${isSel ? " selected" : ""}>${esc(p.key)} — ${esc(p.name)}</option>`
      );
    }
    if (savedProject && !projMatched) {
      projOpts.push(`<option value="${esc(savedProject)}" selected>${esc(savedProject)}</option>`);
    }
    projSel.innerHTML = projOpts.join("");

    // --- Assignees (scoped to the currently-selected project) ---
    await loadAssignees(projSel.value, cfg.jira_assignee);

    toast("Pilihan dari Jira dimuat.");
  } catch (e) {
    toast(`Gagal muat dari Jira: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

function wireEvents(): void {
  $("sync-btn").addEventListener("click", () => void doSync());
  $("refresh-btn").addEventListener("click", () => void doRefresh());
  $("board-search").addEventListener("input", (e) => {
    boardSearch = (e.target as HTMLInputElement).value;
    renderBoard(boardTickets);
  });

  $("gear-btn").addEventListener("click", () => void openSettings());
  $("settings-close").addEventListener("click", closeSettings);
  $("settings-cancel").addEventListener("click", closeSettings);
  $("settings-form").addEventListener("submit", (e) => {
    e.preventDefault();
    void saveSettings();
  });
  $("settings-overlay").addEventListener("click", (e) => {
    if (e.target === $("settings-overlay")) closeSettings();
  });

  $("jira-load-btn").addEventListener("click", () => void loadFromJira());
  // When the project changes, reload assignees scoped to it (keep current pick).
  $("cfg-jira_project").addEventListener("change", () => {
    const project = ($("cfg-jira_project") as HTMLSelectElement).value;
    const current = ($("cfg-jira_assignee") as HTMLSelectElement).value;
    void loadAssignees(project, current).catch((e) =>
      toast(`Gagal muat assignee: ${errStr(e)}`, "error")
    );
  });

  // Confirm modal (promise-based; resolves on OK/Cancel/backdrop).
  $("confirm-ok").addEventListener("click", () => settleConfirm(true));
  $("confirm-cancel").addEventListener("click", () => settleConfirm(false));
  $("confirm-overlay").addEventListener("click", (e) => {
    if (e.target === $("confirm-overlay")) settleConfirm(false);
  });

  // Pick-transition modal.
  $("transition-close").addEventListener("click", closeTransitionPicker);
  $("transition-overlay").addEventListener("click", (e) => {
    if (e.target === $("transition-overlay")) closeTransitionPicker();
  });

  // Ticket detail modal.
  $("detail-close").addEventListener("click", closeDetail);
  $("detail-overlay").addEventListener("click", (e) => {
    if (e.target === $("detail-overlay")) closeDetail();
  });
  $("detail-shift").addEventListener("click", () => {
    const key = detailKey;
    if (key) void shiftStatus(key);
  });
  $("tab-testcases").addEventListener("click", () => selectTab("testcases"));
  $("tab-pr").addEventListener("click", () => selectTab("pr"));
  $("pr-search").addEventListener("click", () => void searchPrs());
  $("pr-link-go").addEventListener("click", () => void summarizeFromLink());
  $("pr-link").addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") void summarizeFromLink();
  });
  $("tc-generate").addEventListener("click", () => void generateTestCases());
  $("tc-add-toggle").addEventListener("click", () => {
    const form = $("tc-add-form");
    const nowHidden = form.classList.contains("hidden");
    show(form, nowHidden);
    if (nowHidden) ($("tc-title") as HTMLInputElement).focus();
  });
  $("tc-add-cancel").addEventListener("click", () => {
    ($("tc-add-form") as HTMLFormElement).reset();
    show($("tc-add-form"), false);
  });
  $("tc-add-form").addEventListener("submit", (e) => void addTestCase(e));
}

async function init(): Promise<void> {
  wireEvents();
  await refreshBoard();
}

window.addEventListener("DOMContentLoaded", () => {
  void init();
});
