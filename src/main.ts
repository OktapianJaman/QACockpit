import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";

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
  notes: string;
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
  gemini_api_key: string;
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
  "gemini_api_key",
];

/** Fixed list of repos used by the per-ticket PR dropdown (not user-editable). */
const KNOWN_REPOS = ["tr8team/gotradeindoapp", "tr8team/tradecharlieflutter"];

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

// --- Theme (light / dark), persisted in localStorage ---
type Theme = "dark" | "light";
const THEME_KEY = "qacockpit-theme";

function applyTheme(theme: Theme): void {
  document.documentElement.dataset.theme = theme === "light" ? "light" : "";
  const btn = document.getElementById("theme-btn");
  if (btn) {
    // The icon shows the theme you'd switch TO.
    btn.textContent = theme === "light" ? "🌙" : "☀️";
    btn.title = theme === "light" ? "Ganti ke gelap" : "Ganti ke terang";
  }
}

function currentTheme(): Theme {
  return document.documentElement.dataset.theme === "light" ? "light" : "dark";
}

function initTheme(): void {
  const saved = (localStorage.getItem(THEME_KEY) as Theme | null) ?? "dark";
  applyTheme(saved);
}

function toggleTheme(): void {
  const next: Theme = currentTheme() === "light" ? "dark" : "light";
  localStorage.setItem(THEME_KEY, next);
  applyTheme(next);
}

/** Escape text destined for innerHTML interpolation. */
function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Inline markdown (on already-escaped text): `code`, **bold**, *italic*. */
function mdInline(s: string): string {
  return s
    .replace(/`([^`]+)`/g, "<code>$1</code>")
    .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>")
    .replace(/\*([^*\n]+)\*/g, "<em>$1</em>");
}

/**
 * Render AI markdown as safe HTML (escape FIRST, then format). Handles
 * headings (#..######), horizontal rules (---), ordered/unordered lists,
 * blank-line paragraphs, and inline code/bold/italic. Nested indentation is
 * flattened (good enough for AI summaries).
 */
function mdToHtml(src: string): string {
  const lines = esc(src).split("\n");
  const out: string[] = [];
  let list: "ul" | "ol" | null = null;
  const closeList = (): void => {
    if (list) {
      out.push(`</${list}>`);
      list = null;
    }
  };
  for (const raw of lines) {
    const line = raw.trim();
    let m: RegExpMatchArray | null;
    if (/^---+$/.test(line) || /^\*\*\*+$/.test(line)) {
      closeList();
      out.push("<hr>");
    } else if ((m = line.match(/^(#{1,6})\s+(.*)$/))) {
      closeList();
      const lvl = Math.min(m[1].length + 2, 6); // # -> h3, ## -> h4, …
      out.push(`<h${lvl} class="md-h">${mdInline(m[2])}</h${lvl}>`);
    } else if ((m = line.match(/^\d+[.)]\s+(.*)$/))) {
      if (list !== "ol") {
        closeList();
        out.push("<ol>");
        list = "ol";
      }
      out.push(`<li>${mdInline(m[1])}</li>`);
    } else if ((m = line.match(/^[-*]\s+(.*)$/))) {
      if (list !== "ul") {
        closeList();
        out.push("<ul>");
        list = "ul";
      }
      out.push(`<li>${mdInline(m[1])}</li>`);
    } else if (line === "") {
      closeList();
    } else {
      closeList();
      out.push(`<p>${mdInline(line)}</p>`);
    }
  }
  closeList();
  return out.join("");
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

// Terminal/closed statuses collapse into a single "Done" column.
const DONE_KEYWORDS = ["done", "passed", "closed", "complete", "resolved", "selesai"];

/** Map a raw Jira status to its display column — terminal ones → "Done". */
function displayColumn(status: string): string {
  const s = status.toLowerCase();
  return DONE_KEYWORDS.some((k) => s.includes(k)) ? "Done" : status;
}

/** Distinct DISPLAY columns present, ordered by preferred sequence then alpha. */
function orderedColumns(tickets: BoardTicket[]): string[] {
  return [...new Set(tickets.map((t) => displayColumn(t.status)).filter(Boolean))].sort(
    (a, b) => {
      const r = statusRank(a) - statusRank(b);
      return r !== 0 ? r : a.localeCompare(b);
    }
  );
}

/** Build one card (click → detail; inline points; "pindah" → transition picker).
 *  Shows its real Jira status (since a column may merge several statuses). */
function buildCard(t: BoardTicket): HTMLElement {
  const card = document.createElement("div");
  card.className = "kcard";
  card.innerHTML = `
    <div class="kc-top">
      <span class="kc-key mono">${esc(t.key)}</span>
      <span class="kc-status">${esc(t.status)}</span>
    </div>
    <div class="kc-summary">${esc(t.summary || "—")}</div>
    <div class="kc-foot">
      <button class="ct-points" type="button" title="Klik untuk ubah story point">${esc(
        pointsLabel(t.story_points)
      )}</button>
      <button class="status-btn kc-move" type="button" title="Pindahkan status">pindah ▾</button>
    </div>`;

  card.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).closest("button")) return; // points / pindah
    void openDetail(t.key);
  });
  const ptsBtn = card.querySelector<HTMLButtonElement>(".ct-points");
  ptsBtn?.addEventListener("click", () => startPointEdit(t, ptsBtn));
  card.querySelector<HTMLButtonElement>(".kc-move")?.addEventListener(
    "click",
    () => void shiftStatus(t.key)
  );
  return card;
}

/** Build a column for one display status. Header = UPPERCASE name + count/total. */
/** Map a display column to a stage color class (lane cap + count color). */
function stageClass(status: string): string {
  const s = status.toLowerCase();
  if (/done|passed|closed|complete|resolved|selesai/.test(s)) return "stage-done";
  if (/fail/.test(s)) return "stage-failed";
  if (/progress/.test(s)) return "stage-progress";
  if (/ready/.test(s)) return "stage-ready";
  return "";
}

function buildColumn(status: string, cards: BoardTicket[], total: number): HTMLElement {
  const col = document.createElement("section");
  col.className = `column ${stageClass(status)}`.trim();
  const head = document.createElement("div");
  head.className = "column-head";
  head.innerHTML = `
    <span class="col-name" title="${esc(status)}">${esc(status)}</span>
    <span class="col-count">${cards.length}/${total}</span>`;
  col.appendChild(head);
  const body = document.createElement("div");
  body.className = "column-body";
  if (cards.length === 0) {
    body.innerHTML = `<div class="col-empty">—</div>`;
  } else {
    for (const t of cards) body.appendChild(buildCard(t));
  }
  col.appendChild(body);
  return col;
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

/** Render the board as columns (one per status), filtered by search. Columns
 *  stay stable (from the full set); only the cards inside are filtered. */
function renderBoard(tickets: BoardTicket[]): void {
  const board = $("board");
  show($("board-empty"), tickets.length === 0);
  if (tickets.length === 0) {
    board.innerHTML = "";
    return;
  }

  const q = boardSearch.trim().toLowerCase();
  const match = (t: BoardTicket): boolean =>
    !q || t.key.toLowerCase().includes(q) || t.summary.toLowerCase().includes(q);

  board.innerHTML = "";
  for (const col of orderedColumns(tickets)) {
    const cards = tickets
      .filter((t) => displayColumn(t.status) === col && match(t))
      .sort((a, b) => a.key.localeCompare(b.key));
    // count badge "X/Y" = cards in this column / total tickets on the board.
    board.appendChild(buildColumn(col, cards, tickets.length));
  }
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

async function doSync(): Promise<void> {
  const btn = $<HTMLButtonElement>("sync-btn");
  btn.disabled = true;
  btn.classList.add("busy");
  btn.textContent = "Sync…";
  try {
    const res = await invoke<{ tickets: number; prs: number }>("sync_now");
    toast(`Sync beres: ${res.tickets} tiket, ${res.prs} PR.`);
    await refreshBoard();
  } catch (e) {
    toast(`Sync gagal: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.classList.remove("busy");
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

// --- actual-point prompt (shown before a transition) ---
type PointResult = { cancelled: boolean; points: number | null };
let pointResolve: ((r: PointResult) => void) | null = null;

function settlePoint(r: PointResult): void {
  show($("point-overlay"), false);
  const res = pointResolve;
  pointResolve = null;
  res?.(r);
}

/** Ask for the actual point before moving; prefilled with the current value. */
function promptActualPoint(message: string, current: number | null): Promise<PointResult> {
  if (pointResolve) settlePoint({ cancelled: true, points: current });
  $("point-msg").textContent = message;
  const input = $("point-input") as HTMLInputElement;
  input.value = current == null ? "" : String(current);
  show($("point-overlay"), true);
  input.focus();
  input.select();
  return new Promise<PointResult>((resolve) => {
    pointResolve = resolve;
  });
}

/** Read the point input and settle the prompt as confirmed. */
function confirmPointPrompt(): void {
  const raw = ($("point-input") as HTMLInputElement).value.trim();
  let points: number | null = raw === "" ? null : Number(raw);
  if (points !== null && Number.isNaN(points)) points = null;
  settlePoint({ cancelled: false, points });
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
  const current = ticketByKey(key)?.story_points ?? null;
  // Ask for the actual point when finishing a ticket — any pass/fail verdict or
  // a done/completed status (e.g. "QA Passed", "Pass QA", "QA Failed", "Fail QA",
  // "Task Done", "Task Completed"). Other moves just confirm. Keyword-based so
  // word order doesn't matter.
  const isVerdict = /pass|fail|done|complete/i.test(target);
  let points: number | null = current;
  if (isVerdict) {
    const res = await promptActualPoint(
      `Geser ${key} ke "${target}". Isi actual point QA-nya (opsional).`,
      current
    );
    if (res.cancelled) return;
    points = res.points;
  } else {
    const ok = await confirmDialog(`Geser ${key} ke "${target}"? Mengubah status di Jira.`);
    if (!ok) return;
  }
  try {
    if (isVerdict && points !== current) {
      await invoke("set_story_points", { key, points });
    }
    // Tauri maps snake_case command params (transition_id) to camelCase.
    // Pass to_status so the local DB mirror updates → board reflects it.
    await invoke("transition_issue", { key, transitionId: t.id, toStatus: target });
    toast(
      `Status ${key} diubah${isVerdict && points != null ? ` · ${points} pts` : ""}.`
    );
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
// PRs linked to the open ticket (a ticket can span repos, e.g. native + flutter).
let linkedPrs: PrRef[] = [];
// Repos for the PR repo dropdown — a fixed, hardcoded list.
const knownRepos: string[] = [...KNOWN_REPOS];

/** Fill the PR repo dropdown from knownRepos. */
function populateRepoDropdown(): void {
  const sel = $("pr-repo") as HTMLSelectElement;
  if (knownRepos.length === 0) {
    sel.innerHTML = `<option value="">(set repo di Settings)</option>`;
  } else {
    sel.innerHTML = knownRepos
      .map((r) => `<option value="${esc(r)}">${esc(r)}</option>`)
      .join("");
  }
}

/** "+ Tambah PR" via the repo dropdown + number input. */
async function addPrFromPicker(): Promise<void> {
  const repo = ($("pr-repo") as HTMLSelectElement).value.trim();
  const numEl = $("pr-num") as HTMLInputElement;
  const number = Number(numEl.value.trim());
  if (!repo) {
    toast("Pilih repo dulu (atau set daftar repo di Settings).", "error");
    return;
  }
  if (!number || number < 1) {
    toast("Isi nomor PR-nya (mis. 3231).", "error");
    return;
  }
  const pr: PrRef = {
    number,
    repo,
    title: `PR #${number}`,
    state: "",
    url: `https://github.com/${repo}/pull/${number}`,
  };
  const isNew = addLinkedPr(pr);
  numEl.value = "";
  renderPrs();
  if (!isNew) {
    toast("PR itu udah ada di daftar.");
    return;
  }
  const items = $("pr-list").querySelectorAll<HTMLElement>(".pr-item");
  const last = items[items.length - 1];
  const btn = last?.querySelector<HTMLButtonElement>(".pr-summarize");
  const panel = last?.querySelector<HTMLDivElement>(".pr-review");
  if (btn && panel) await summarizePr(pr, btn, panel);
}

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
  // Reset the PR tab (clear linked PRs from the previous ticket).
  linkedPrs = [];
  populateRepoDropdown();
  $("pr-list").innerHTML = "";
  show($("pr-empty"), true);
  $("pr-empty").textContent = "Tempel link PR di atas (boleh lebih dari satu), atau cari otomatis.";
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
    item.className = `tc-item tc-${c.status}`;
    // The detail panel always exists now (it hosts the editable notes field),
    // even when a case has no steps/expected.
    item.innerHTML = `
      <div class="tc-item-head">
        <span class="${tcStatusClass(c.status)}">${esc(tcStatusLabel(c.status))}</span>
        <span class="tc-title">${esc(c.title)}</span>
        <button class="tc-toggle" type="button" title="Lihat detail">▾</button>
        <div class="tc-item-actions">
          <button class="btn small tc-pass" type="button" title="Pass">✅</button>
          <button class="btn small tc-fail" type="button" title="Fail">❌</button>
          <button class="btn small tc-del" type="button" title="Hapus">🗑</button>
        </div>
      </div>
      <div class="tc-detail">
        ${c.steps ? `<div class="tc-field"><span class="tc-label">Langkah:</span> ${esc(c.steps)}</div>` : ""}
        ${c.expected ? `<div class="tc-field"><span class="tc-label">Harapan:</span> ${esc(c.expected)}</div>` : ""}
        <div class="tc-field">
          <span class="tc-label">Catatan:</span>
          <textarea class="tc-notes" rows="2" placeholder="Catatan / hasil aktual…">${esc(c.notes)}</textarea>
        </div>
      </div>`;

    const toggle = item.querySelector<HTMLButtonElement>(".tc-toggle");
    const titleEl = item.querySelector<HTMLElement>(".tc-title");
    const doToggle = (): void => {
      item.classList.toggle("open");
    };
    toggle?.addEventListener("click", doToggle);
    titleEl?.addEventListener("click", doToggle);

    // Persist notes on blur (no reload — the value is already in the textarea).
    const notesEl = item.querySelector<HTMLTextAreaElement>(".tc-notes");
    notesEl?.addEventListener("blur", () => {
      const notes = notesEl.value;
      if (notes === c.notes) return; // unchanged
      c.notes = notes;
      void saveTestCaseNotes(c.id, notes);
    });

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

/** Save a test case's notes (fire-and-forget on blur; no list reload). */
async function saveTestCaseNotes(id: number, notes: string): Promise<void> {
  try {
    await invoke("set_test_case_notes", { id, notes });
  } catch (e) {
    toast(`Gagal simpan catatan: ${errStr(e)}`, "error");
  }
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

/** "✨ Generate pakai AI": draft cases from the ticket summary (Gemini). */
async function generateTestCases(): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const summary = ticketByKey(key)?.summary || "";
  const btn = $<HTMLButtonElement>("tc-generate");
  btn.disabled = true;
  btn.classList.add("busy");
  const prev = btn.textContent;
  btn.textContent = "Lagi bikin test case…";
  try {
    const cases = await invoke<TestCase[]>("generate_test_cases", { key, summary });
    if (detailKey === key) renderTestCases(cases);
    toast("Test case dibuat oleh AI.");
  } catch (e) {
    toast(`Gagal generate: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.classList.remove("busy");
    btn.textContent = prev;
  }
}

/** "📤 Kirim hasil ke Jira": post the ticket's test results as a Jira comment. */
async function postTestResults(): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const ok = await confirmDialog(
    `Kirim hasil test ke Jira sebagai komentar di ${key}?`
  );
  if (!ok) return;
  const btn = $<HTMLButtonElement>("tc-post-jira");
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Lagi kirim ke Jira…";
  try {
    const line = await invoke<string>("post_test_results", { key });
    toast(`Terkirim: ${line}`);
  } catch (e) {
    toast(errStr(e), "error");
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
/** Add a PR to the linked list (dedup by repo+number). Returns false if dup. */
function addLinkedPr(pr: PrRef): boolean {
  if (linkedPrs.some((p) => p.repo === pr.repo && p.number === pr.number)) return false;
  linkedPrs.push(pr);
  return true;
}

/** Render all linked PRs + a "generate from ALL" bar. */
function renderPrs(): void {
  const list = $("pr-list");
  list.innerHTML = "";

  if (linkedPrs.length === 0) {
    show($("pr-empty"), true);
    $("pr-empty").textContent =
      "Tempel link PR di atas (boleh lebih dari satu), atau cari otomatis.";
    return;
  }
  show($("pr-empty"), false);

  // Bar: generate test cases from ALL linked PRs combined.
  const bar = document.createElement("div");
  bar.className = "pr-allbar";
  bar.innerHTML = `<button class="btn small primary pr-gen-all" type="button">✨ Buat test case dari SEMUA PR (${linkedPrs.length})</button>`;
  bar
    .querySelector<HTMLButtonElement>(".pr-gen-all")
    ?.addEventListener("click", (e) =>
      void generateTestCasesFromAllPrs(e.currentTarget as HTMLButtonElement)
    );
  list.appendChild(bar);

  for (const pr of linkedPrs) {
    const item = document.createElement("div");
    item.className = "pr-item";
    item.innerHTML = `
      <div class="pr-item-head">
        <span class="pr-ref mono">#${pr.number} · ${esc(pr.repo)}</span>
        <span class="${prStateClass(pr.state)}">${esc(pr.state)}</span>
        <button class="pr-remove" type="button" title="Hapus dari daftar">✕</button>
      </div>
      <span class="pr-title">${esc(pr.title)}</span>
      <div class="pr-item-actions">
        <button class="btn small primary pr-summarize" type="button">✨ Ringkas + apa yang dites</button>
        <button class="btn small pr-gen-tc" type="button">✨ Buat test case dari PR ini</button>
      </div>
      <div class="pr-review hidden"></div>`;

    const btn = item.querySelector<HTMLButtonElement>(".pr-summarize");
    const panel = item.querySelector<HTMLDivElement>(".pr-review");
    btn?.addEventListener("click", () => void summarizePr(pr, btn, panel!));
    item
      .querySelector<HTMLButtonElement>(".pr-gen-tc")
      ?.addEventListener("click", (e) =>
        void generateTestCasesFromPr(pr, e.currentTarget as HTMLButtonElement)
      );
    item.querySelector<HTMLButtonElement>(".pr-remove")?.addEventListener("click", () => {
      linkedPrs = linkedPrs.filter((p) => !(p.repo === pr.repo && p.number === pr.number));
      renderPrs();
    });

    list.appendChild(item);
  }
}

/** Add a PR pasted as a GitHub URL to the list, then summarize it. */
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
  const isNew = addLinkedPr(pr);
  input.value = "";
  renderPrs();
  if (!isNew) {
    toast("PR itu udah ada di daftar.");
    return;
  }
  // Auto-summarize the just-added PR (last item in the list).
  const items = $("pr-list").querySelectorAll<HTMLElement>(".pr-item");
  const last = items[items.length - 1];
  const btn = last?.querySelector<HTMLButtonElement>(".pr-summarize");
  const panel = last?.querySelector<HTMLDivElement>(".pr-review");
  if (btn && panel) await summarizePr(pr, btn, panel);
}

/** "🔍 Cari PR": search GitHub for PRs mentioning the key; add them to the list. */
async function searchPrs(): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const btn = $<HTMLButtonElement>("pr-search");
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Lagi cari PR…";
  try {
    const prs = await invoke<PrRef[]>("list_ticket_prs", { key });
    if (detailKey !== key) return;
    if (prs.length === 0) {
      toast(`Nggak nemu PR yang nyebut ${key} di GitHub.`);
    }
    for (const pr of prs) addLinkedPr(pr);
    renderPrs();
  } catch (e) {
    toast(errStr(e), "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

/** Generate test cases from the COMBINED diffs of all linked PRs. */
async function generateTestCasesFromAllPrs(btn: HTMLButtonElement): Promise<void> {
  if (!detailKey || linkedPrs.length === 0) return;
  const key = detailKey;
  const summary = ticketByKey(key)?.summary || "";
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Lagi bikin test case dari semua PR…";
  try {
    const prs = linkedPrs.map((p) => ({ repo: p.repo, number: p.number }));
    const cases = await invoke<TestCase[]>("generate_test_cases_from_prs", {
      key,
      summary,
      prs,
    });
    toast(`${cases.length} test case dibuat dari ${linkedPrs.length} PR.`);
    if (detailKey === key) {
      selectTab("testcases");
      await loadTestCases(key);
    }
  } catch (e) {
    toast(errStr(e), "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

/** Fetch a PR's diff and render the Gemini summary / what-to-test. */
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
  panel.textContent = "Lagi baca PR & nyusun…";
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

/** "✨ Buat test case dari PR ini": draft cases from the PR diff, then switch
 *  to the Test Cases tab and reload. */
async function generateTestCasesFromPr(
  pr: PrRef,
  btn: HTMLButtonElement
): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const summary = ticketByKey(key)?.summary || "";
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Lagi bikin test case dari PR…";
  try {
    const cases = await invoke<TestCase[]>("generate_test_cases_from_pr", {
      key,
      summary,
      repo: pr.repo,
      number: pr.number,
    });
    toast(`${cases.length} test case dibuat dari PR.`);
    if (detailKey === key) {
      selectTab("testcases");
      await loadTestCases(key);
    }
  } catch (e) {
    toast(errStr(e), "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

// The three Jira selects need their saved value shown even before "Muat dari
// Jira" is clicked, so saving never loses it. They are excluded from the generic
// loop and seeded by these helpers — an empty <select> must have options before
// its value can be set.
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
      if (JIRA_DROPDOWN_KEYS.includes(k)) continue; // seeded via dedicated helpers
      ($(`cfg-${k}`) as HTMLInputElement).value = cfg[k] ?? "";
    }
    seedFieldDropdown(cfg.jira_story_point_field ?? "");
    seedProjectDropdown(cfg.jira_project ?? "");
    seedAssigneeDropdown(cfg.jira_assignee ?? "");
    $("jira-fields-hint").textContent = "";
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
// Daily summary
// ---------------------------------------------------------------------------

// The day (YYYY-MM-DD) the summary overlay is showing.
let summaryDay = "";

/** Render the summary body (markdown → HTML) or show the empty hint. */
function renderSummary(body: string): void {
  const hasBody = body.trim().length > 0;
  $("sum-body").innerHTML = hasBody ? mdToHtml(body) : "";
  show($("sum-body"), hasBody);
  show($("sum-empty"), !hasBody);
}

async function openSummary(): Promise<void> {
  show($("summary-overlay"), true);
  try {
    summaryDay = await invoke<string>("today");
  } catch {
    summaryDay = "";
  }
  $("sum-day").textContent = summaryDay || "—";
  try {
    const cached = await invoke<string>("get_daily_summary", { day: summaryDay });
    renderSummary(cached);
  } catch (e) {
    renderSummary("");
    toast(`Gagal muat ringkasan: ${errStr(e)}`, "error");
  }
}

function closeSummary(): void {
  show($("summary-overlay"), false);
}

async function generateSummary(): Promise<void> {
  if (!summaryDay) {
    toast("Tanggal hari ini nggak kebaca.", "error");
    return;
  }
  const btn = $<HTMLButtonElement>("sum-generate");
  const prev = btn.textContent;
  btn.disabled = true;
  btn.classList.add("busy");
  btn.textContent = "Generating…";
  try {
    const body = await invoke<string>("generate_ai_summary", { day: summaryDay });
    renderSummary(body);
  } catch (e) {
    toast(`Gagal generate ringkasan: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.classList.remove("busy");
    btn.textContent = prev;
  }
}

function wireSummary(): void {
  $("summary-btn").addEventListener("click", () => void openSummary());
  $("sum-close").addEventListener("click", closeSummary);
  $("summary-overlay").addEventListener("click", (e) => {
    if (e.target === $("summary-overlay")) closeSummary();
  });
  $("sum-generate").addEventListener("click", () => void generateSummary());
}

// ---------------------------------------------------------------------------
// Bug Writer
// ---------------------------------------------------------------------------

// The attached screenshot as a data URL (null = none).
let bwImage: string | null = null;

interface BugReport {
  title: string;
  body: string;
  raw: string;
}
interface CreatedIssue {
  key: string;
  url: string;
}

/** Read an image File/Blob into a data-URL string. */
function fileToDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(reader.error ?? new Error("gagal baca gambar"));
    reader.readAsDataURL(file);
  });
}

function setBwImage(dataUrl: string): void {
  bwImage = dataUrl;
  const img = $("bw-preview") as HTMLImageElement;
  img.src = dataUrl;
  show(img, true);
  show($("bw-drop-hint"), false);
  show($("bw-clear-img"), true);
}

function clearBwImage(): void {
  bwImage = null;
  ($("bw-file") as HTMLInputElement).value = "";
  show($("bw-preview"), false);
  show($("bw-drop-hint"), true);
  show($("bw-clear-img"), false);
}

/** Accept the first image from a File list / DataTransfer items. */
async function acceptBwImageFrom(files: FileList | null): Promise<void> {
  const file = files && Array.from(files).find((f) => f.type.startsWith("image/"));
  if (!file) return;
  try {
    setBwImage(await fileToDataUrl(file));
  } catch (e) {
    toast(`Gagal baca gambar: ${errStr(e)}`, "error");
  }
}

/** Fill the Bug Writer project dropdown (defaulting to the configured project). */
async function populateBwProjects(): Promise<void> {
  const sel = $("bw-project") as HTMLSelectElement;
  try {
    const cfg = await invoke<AppConfig>("get_config");
    const projects = await invoke<JiraProject[]>("list_jira_projects");
    const saved = cfg.jira_project ?? "";
    sel.innerHTML = projects
      .map(
        (p) =>
          `<option value="${esc(p.key)}"${p.key === saved ? " selected" : ""}>${esc(p.key)} — ${esc(p.name)}</option>`
      )
      .join("");
    await loadBwAssignees(sel.value);
  } catch (e) {
    sel.innerHTML = `<option value="">(cek kredensial Jira di Settings)</option>`;
    toast(`Gagal muat project Jira: ${errStr(e)}`, "error");
  }
}

/** Repopulate the Bug Writer assignee select for a project. */
async function loadBwAssignees(project: string): Promise<void> {
  const sel = $("bw-assignee") as HTMLSelectElement;
  try {
    const users = await invoke<JiraUser[]>("list_jira_assignees", { project });
    const opts = [`<option value="" selected>(tidak di-assign)</option>`];
    for (const u of users) {
      opts.push(`<option value="${esc(u.account_id)}">${esc(u.display_name)}</option>`);
    }
    sel.innerHTML = opts.join("");
  } catch {
    sel.innerHTML = `<option value="">(tidak di-assign)</option>`;
  }
}

function openBugWriter(): void {
  // Reset to a clean input state every open.
  ($("bw-text") as HTMLTextAreaElement).value = "";
  clearBwImage();
  show($("bw-result"), false);
  show($("bugwriter-overlay"), true);
  void populateBwProjects();
}

function closeBugWriter(): void {
  show($("bugwriter-overlay"), false);
}

/** Collect the checked section keys (in display order). */
function bwSelectedSections(): string[] {
  return Array.from(
    $("bugwriter-overlay").querySelectorAll<HTMLInputElement>(
      ".bw-sections input[type=checkbox]:checked"
    )
  ).map((c) => c.value);
}

async function generateBug(): Promise<void> {
  const text = ($("bw-text") as HTMLTextAreaElement).value;
  if (!text.trim() && !bwImage) {
    toast("Isi deskripsi bug atau lampirkan screenshot dulu.", "error");
    return;
  }
  const sections = bwSelectedSections();
  if (sections.length === 0) {
    toast("Pilih minimal satu section.", "error");
    return;
  }
  const language = ($("bw-language") as HTMLSelectElement).value;
  const btn = $<HTMLButtonElement>("bw-generate");
  const prev = btn.textContent;
  btn.disabled = true;
  btn.classList.add("busy");
  btn.textContent = "Generating…";
  try {
    const report = await invoke<BugReport>("generate_bug_report", {
      text,
      imageBase64: bwImage ?? undefined,
      language,
      sections,
    });
    ($("bw-title") as HTMLInputElement).value = report.title;
    ($("bw-body") as HTMLTextAreaElement).value = report.body;
    show($("bw-result"), true);
    $("bw-result").scrollIntoView({ behavior: "smooth", block: "nearest" });
  } catch (e) {
    toast(`Gagal generate: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.classList.remove("busy");
    btn.textContent = prev;
  }
}

async function createBug(): Promise<void> {
  const projectKey = ($("bw-project") as HTMLSelectElement).value.trim();
  const summary = ($("bw-title") as HTMLInputElement).value.trim();
  const body = ($("bw-body") as HTMLTextAreaElement).value;
  const priority = ($("bw-priority") as HTMLSelectElement).value || undefined;
  const assigneeId = ($("bw-assignee") as HTMLSelectElement).value || undefined;
  if (!projectKey) {
    toast("Pilih project Jira dulu.", "error");
    return;
  }
  if (!summary) {
    toast("Title bug nggak boleh kosong.", "error");
    return;
  }
  const btn = $<HTMLButtonElement>("bw-create");
  const prev = btn.textContent;
  btn.disabled = true;
  btn.classList.add("busy");
  btn.textContent = "Mengirim…";
  try {
    const issue = await invoke<CreatedIssue>("create_jira_bug", {
      projectKey,
      summary,
      body,
      priority,
      assigneeId,
      imageBase64: bwImage ?? undefined,
    });
    toast(`Bug dibuat: ${issue.key}`);
    void openUrl(issue.url).catch(() => {
      /* toast already shows the key if opening the browser fails */
    });
    closeBugWriter();
  } catch (e) {
    toast(`Gagal buat bug: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.classList.remove("busy");
    btn.textContent = prev;
  }
}

function wireBugWriter(): void {
  $("bugwriter-btn").addEventListener("click", openBugWriter);
  $("bw-close").addEventListener("click", closeBugWriter);
  $("bugwriter-overlay").addEventListener("click", (e) => {
    if (e.target === $("bugwriter-overlay")) closeBugWriter();
  });

  // Screenshot: click to pick, drag-drop, or paste.
  const drop = $("bw-drop");
  drop.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).id !== "bw-clear-img") ($("bw-file") as HTMLInputElement).click();
  });
  $("bw-file").addEventListener("change", (e) =>
    void acceptBwImageFrom((e.target as HTMLInputElement).files)
  );
  $("bw-clear-img").addEventListener("click", (e) => {
    e.stopPropagation();
    clearBwImage();
  });
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("bw-drag");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("bw-drag"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("bw-drag");
    void acceptBwImageFrom((e as DragEvent).dataTransfer?.files ?? null);
  });
  // Paste anywhere while the overlay is open.
  $("bugwriter-overlay").addEventListener("paste", (e) => {
    const items = (e as ClipboardEvent).clipboardData?.items;
    if (!items) return;
    for (const it of Array.from(items)) {
      if (it.kind === "file" && it.type.startsWith("image/")) {
        const file = it.getAsFile();
        if (file) {
          e.preventDefault();
          void acceptBwImageFrom(({ 0: file, length: 1, item: () => file } as unknown) as FileList);
        }
        return;
      }
    }
  });

  $("bw-project").addEventListener("change", () =>
    void loadBwAssignees(($("bw-project") as HTMLSelectElement).value)
  );
  $("bw-generate").addEventListener("click", () => void generateBug());
  $("bw-create").addEventListener("click", () => void createBug());
  $("bw-copy").addEventListener("click", () => {
    const title = ($("bw-title") as HTMLInputElement).value;
    const body = ($("bw-body") as HTMLTextAreaElement).value;
    void navigator.clipboard
      .writeText(`${title}\n\n${body}`)
      .then(() => toast("Disalin ke clipboard."))
      .catch(() => toast("Gagal menyalin.", "error"));
  });
}

/** Save the current form, then run a connection-test command and show the
 *  result inline. Used by the Jira / GitHub / Gemini "Test koneksi" buttons. */
async function runTest(btnId: string, statusId: string, command: string): Promise<void> {
  const btn = $<HTMLButtonElement>(btnId);
  const status = $(statusId);
  status.className = "test-status";
  try {
    // Test the values currently entered, not just the last-saved ones.
    await invoke("set_config", { cfg: readConfigFromForm() });
  } catch (e) {
    status.textContent = `✗ Gagal simpan: ${errStr(e)}`;
    status.className = "test-status err";
    return;
  }
  btn.disabled = true;
  btn.classList.add("busy");
  status.textContent = "Mengetes…";
  try {
    const msg = await invoke<string>(command);
    status.textContent = `✓ ${msg}`;
    status.className = "test-status ok";
  } catch (e) {
    status.textContent = `✗ ${errStr(e)}`;
    status.className = "test-status err";
  } finally {
    btn.disabled = false;
    btn.classList.remove("busy");
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

function wireEvents(): void {
  $("sync-btn").addEventListener("click", () => void doSync());
  $("board-search").addEventListener("input", (e) => {
    boardSearch = (e.target as HTMLInputElement).value;
    renderBoard(boardTickets);
  });

  // External links (e.g. "where to get a token") open in the system browser,
  // never inside the app's own webview.
  document.body.addEventListener("click", (e) => {
    const link = (e.target as HTMLElement).closest<HTMLElement>(".ext-link");
    if (!link) return;
    e.preventDefault();
    const url = link.dataset.url;
    if (url) void openUrl(url).catch(() => toast("Gagal buka link.", "error"));
  });

  $("theme-btn").addEventListener("click", toggleTheme);

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
  $("jira-test-btn").addEventListener("click", () =>
    void runTest("jira-test-btn", "jira-test-status", "test_jira_connection")
  );
  $("gh-test-btn").addEventListener("click", () =>
    void runTest("gh-test-btn", "gh-test-status", "test_github_connection")
  );
  $("gemini-test-btn").addEventListener("click", () =>
    void runTest("gemini-test-btn", "gemini-test-status", "test_gemini_connection")
  );
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
  $("point-ok").addEventListener("click", () => confirmPointPrompt());
  $("point-cancel").addEventListener("click", () =>
    settlePoint({ cancelled: true, points: null })
  );
  $("point-input").addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") confirmPointPrompt();
  });
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
  $("pr-add").addEventListener("click", () => void addPrFromPicker());
  $("pr-num").addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") void addPrFromPicker();
  });
  $("pr-link").addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") void summarizeFromLink();
  });
  $("tc-generate").addEventListener("click", () => void generateTestCases());
  $("tc-post-jira").addEventListener("click", () => void postTestResults());
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

  wireBugWriter();
  wireSummary();
}

async function init(): Promise<void> {
  initTheme();
  wireEvents();
  await refreshBoard();
}

window.addEventListener("DOMContentLoaded", () => {
  void init();
});
