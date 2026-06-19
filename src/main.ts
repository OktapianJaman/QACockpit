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

// Key + source status of the card currently being dragged (HTML5 DnD).
let dragKey: string | null = null;
let dragStatus: string | null = null;

/** Rank a status by the preferred order; unmatched statuses rank last. */
function statusRank(status: string): number {
  const s = status.toLowerCase();
  for (let i = 0; i < STATUS_ORDER.length; i++) {
    const kw = STATUS_ORDER[i];
    if (s === kw || s.includes(kw)) return i;
  }
  return STATUS_ORDER.length;
}

/** Distinct statuses among tickets, ordered by preferred sequence then alpha. */
function orderedStatuses(tickets: BoardTicket[]): string[] {
  const seen = [...new Set(tickets.map((t) => t.status).filter(Boolean))];
  return seen.sort((a, b) => {
    const ra = statusRank(a);
    const rb = statusRank(b);
    if (ra !== rb) return ra - rb;
    return a.localeCompare(b);
  });
}

function pointsLabel(pts: number | null): string {
  return pts == null ? "— pts" : `${fmtPoints(pts)} pts`;
}

/** Build one card element for a ticket (draggable, inline point editing). */
function buildCard(t: BoardTicket): HTMLElement {
  const card = document.createElement("div");
  card.className = "card-ticket";
  card.draggable = true;
  card.dataset.key = t.key;
  card.dataset.status = t.status;

  card.innerHTML = `
    <div class="ct-key mono">${esc(t.key)}</div>
    <div class="ct-summary">${esc(t.summary || "—")}</div>
    <button class="ct-points" type="button" title="Klik untuk ubah story point">${esc(
      pointsLabel(t.story_points)
    )}</button>`;

  card.addEventListener("dragstart", (e) => {
    dragKey = t.key;
    dragStatus = t.status;
    card.classList.add("dragging");
    if (e.dataTransfer) {
      e.dataTransfer.effectAllowed = "move";
      e.dataTransfer.setData("text/plain", t.key);
    }
  });
  card.addEventListener("dragend", () => {
    dragKey = null;
    dragStatus = null;
    card.classList.remove("dragging");
  });

  const ptsBtn = card.querySelector<HTMLButtonElement>(".ct-points");
  ptsBtn?.addEventListener("click", () => startPointEdit(t, ptsBtn));

  // Double-click opens the explicit transition picker (alternative to drag-drop,
  // handy when the target status isn't a visible column).
  card.addEventListener("dblclick", () => void shiftStatus(t.key));

  return card;
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

/** Build a column (drop zone) for one status, filled with its cards. */
function buildColumn(status: string, tickets: BoardTicket[]): HTMLElement {
  const col = document.createElement("section");
  col.className = "column";
  col.dataset.status = status;

  const head = document.createElement("div");
  head.className = "column-head";
  head.innerHTML = `
    <span class="col-name" title="${esc(status)}">${esc(status)}</span>
    <span class="col-count">${tickets.length}</span>`;
  col.appendChild(head);

  const body = document.createElement("div");
  body.className = "column-body";
  for (const t of tickets) body.appendChild(buildCard(t));
  col.appendChild(body);

  // Drop-zone wiring.
  col.addEventListener("dragover", (e) => {
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    col.classList.add("drag-over");
  });
  col.addEventListener("dragleave", (e) => {
    // Only clear when the pointer actually leaves the column, not a child.
    if (!col.contains(e.relatedTarget as Node | null)) col.classList.remove("drag-over");
  });
  col.addEventListener("drop", (e) => {
    e.preventDefault();
    col.classList.remove("drag-over");
    const key = dragKey ?? e.dataTransfer?.getData("text/plain") ?? "";
    const from = dragStatus;
    if (key) void onDrop(key, from, status);
  });

  return col;
}

/** Handle a card dropped onto a column: find a matching Jira transition. */
async function onDrop(key: string, fromStatus: string | null, toStatus: string): Promise<void> {
  if (fromStatus !== null && fromStatus.toLowerCase() === toStatus.toLowerCase()) {
    return; // same column — nothing to do
  }
  let trans: JiraTransition[];
  try {
    trans = await invoke<JiraTransition[]>("list_transitions", { key });
  } catch (e) {
    toast(`Gagal ambil transisi: ${errStr(e)}`, "error");
    await refreshBoard();
    return;
  }
  const t = trans.find((x) => x.to_status.toLowerCase() === toStatus.toLowerCase());
  if (!t) {
    toast(`Nggak ada transisi langsung ke "${toStatus}" di Jira.`, "error");
    await refreshBoard();
    return;
  }
  if (await confirmDialog(`Pindahkan ${key} ke "${toStatus}"?`)) {
    try {
      await invoke("transition_issue", { key, transitionId: t.id });
      toast("Status diubah");
    } catch (e) {
      toast(`Gagal ubah status: ${errStr(e)}`, "error");
    }
  }
  await refreshBoard();
}

function renderBoard(tickets: BoardTicket[]): void {
  const board = $("board");
  board.innerHTML = "";
  show($("board-empty"), tickets.length === 0);
  if (tickets.length === 0) return;

  const statuses = orderedStatuses(tickets);
  for (const status of statuses) {
    const inCol = tickets.filter((t) => t.status === status);
    board.appendChild(buildColumn(status, inCol));
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
}

async function init(): Promise<void> {
  wireEvents();
  await refreshBoard();
}

window.addEventListener("DOMContentLoaded", () => {
  void init();
});
