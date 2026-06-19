import { invoke } from "@tauri-apps/api/core";

// ---------------------------------------------------------------------------
// Backend types (mirror src-tauri/src/commands.rs — serde defaults to
// snake_case Rust field names, so match them exactly).
// ---------------------------------------------------------------------------

type Fairness = "Fair" | "UnderPointed" | "OverPointed" | "Untracked";

interface DashboardHeader {
  deserved_total: number;
  assigned_total: number;
  net_work_secs: number;
}

interface TicketRow {
  key: string;
  summary: string;
  status: string;
  story_points: number | null;
  worked_secs: number;
  deserved: number;
  assigned: number;
  fairness: Fairness;
}

interface TimelineRow {
  id: number;
  app: string;
  title: string;
  start: string;
  end: string;
  minutes: number;
  is_idle: boolean;
  ticket_key: string | null;
}

interface PrRow {
  number: number;
  repo: string;
  title: string;
  state: string;
  url: string;
  updated: string;
}

interface TicketOption {
  key: string;
  summary: string;
}

interface Dashboard {
  day: string;
  header: DashboardHeader;
  tickets: TicketRow[];
  all_tickets: TicketOption[];
  timeline: TimelineRow[];
  prs: PrRow[];
  notes: string;
  ai_summary: string;
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
// State
// ---------------------------------------------------------------------------

let currentDay = "";
let dashboard: Dashboard | null = null;
let recording = false;

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

/** Seconds → "Hh Mm" (or "Mm" when under an hour). */
function formatSecs(secs: number): string {
  const total = Math.max(0, Math.round(secs / 60));
  const h = Math.floor(total / 60);
  const m = total % 60;
  return h > 0 ? `${h}j ${m}m` : `${m}m`;
}

/** Round a points value to 1 decimal place, dropping a trailing .0. */
function fmtPoints(n: number): string {
  const r = Math.round(n * 10) / 10;
  return Number.isInteger(r) ? String(r) : r.toFixed(1);
}

/** RFC3339 timestamp → local "HH:MM"; falls back to raw on parse failure. */
function fmtTime(ts: string): string {
  const d = new Date(ts);
  if (isNaN(d.getTime())) return ts;
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
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
// Rendering
// ---------------------------------------------------------------------------

function renderHeader(h: DashboardHeader): void {
  $("hdr-deserved").textContent = fmtPoints(h.deserved_total);
  $("hdr-worktime").textContent = formatSecs(h.net_work_secs);
}

/** Minimal, safe markdown → HTML: escapes first, then **bold**, *italic*,
 *  `code`, and paragraph/line breaks. */
function mdToHtml(raw: string): string {
  let s = esc(raw);
  s = s.replace(/`([^`]+)`/g, "<code>$1</code>");
  s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  s = s.replace(/\*([^*]+)\*/g, "<em>$1</em>");
  s = s.replace(/\n{2,}/g, "</p><p>");
  s = s.replace(/\n/g, "<br>");
  return `<p>${s}</p>`;
}

function renderAiSummary(text: string): void {
  const el = $("ai-summary");
  if (text && text.trim()) {
    el.innerHTML = mdToHtml(text);
    el.classList.remove("empty");
  } else {
    el.textContent = 'Belum ada ringkasan. Klik "Buat ringkasan" buat bikin.';
    el.classList.add("empty");
  }
}

/** Animated placeholder shown while Gemma is composing the summary. */
function showAiSkeleton(): void {
  const el = $("ai-summary");
  el.classList.remove("empty");
  el.innerHTML = `
    <div class="ai-loading">
      <span class="spinner"></span>
      <span>Menyusun ringkasan… (model lokal, bisa agak lama)</span>
    </div>
    <div class="skeleton-lines">
      <div class="sk-line"></div>
      <div class="sk-line"></div>
      <div class="sk-line short"></div>
    </div>`;
}

function renderTickets(tickets: TicketRow[]): void {
  const body = $("ticket-body");
  show($("ticket-empty"), tickets.length === 0);
  if (tickets.length === 0) {
    body.innerHTML = "";
    return;
  }
  body.innerHTML = tickets
    .map((t) => {
      const untouched = t.worked_secs <= 0;
      const jam = untouched ? "—" : esc(formatSecs(t.worked_secs));
      // Points YOU earned from real work (hours × 2). No Jira-point comparison.
      const poin = untouched ? "—" : esc(fmtPoints(t.deserved));
      return `
        <tr${untouched ? ' class="row-untouched"' : ""}>
          <td class="mono">${esc(t.key)}</td>
          <td class="ellipsis" title="${esc(t.summary)}">${esc(t.summary || "—")}</td>
          <td class="num">${jam}</td>
          <td class="num">${poin}</td>
          <td>
            <div class="status-cell">
              <span class="status-text" title="${esc(t.status)}">${esc(t.status || "—")}</span>
              <button class="btn-shift" data-key="${esc(t.key)}" title="Geser status di Jira">⤳</button>
            </div>
          </td>
        </tr>`;
    })
    .join("");

  // Wire each row's "shift status" button to the transition flow.
  body.querySelectorAll<HTMLButtonElement>(".btn-shift").forEach((btn) => {
    btn.addEventListener("click", () => {
      const key = btn.dataset.key ?? "";
      if (key) void shiftStatus(key);
    });
  });
}

/** Distinct, sorted Jira statuses present in the ticket set. */
function distinctStatuses(tickets: TicketRow[]): string[] {
  return [...new Set(tickets.map((t) => t.status).filter(Boolean))].sort();
}

/** Fill the in-table status filter from the current tickets, keeping selection. */
function populateTicketStatusFilter(tickets: TicketRow[]): void {
  const sel = $("ticket-status-filter") as HTMLSelectElement;
  const cur = sel.value;
  const statuses = distinctStatuses(tickets);
  sel.innerHTML = ['<option value="">Semua status</option>']
    .concat(statuses.map((s) => `<option value="${esc(s)}">${esc(s)}</option>`))
    .join("");
  if (cur && statuses.includes(cur)) sel.value = cur;
}

/** Re-render the ticket table applying the in-table search + status filter. */
function refreshTicketTable(): void {
  if (!dashboard) return;
  const q = ($("ticket-search") as HTMLInputElement).value.trim().toLowerCase();
  const status = ($("ticket-status-filter") as HTMLSelectElement).value;
  const filtered = dashboard.tickets.filter((t) => {
    const matchQ =
      !q || t.key.toLowerCase().includes(q) || t.summary.toLowerCase().includes(q);
    const matchS = !status || t.status === status;
    return matchQ && matchS;
  });
  $("ticket-empty").textContent =
    dashboard.tickets.length > 0
      ? "Tidak ada tiket yang cocok sama filter."
      : "Belum ada tiket. Coba Sync dulu.";
  renderTickets(filtered);
}

interface TimelineGroup {
  ids: number[];
  app: string;
  title: string;
  start: string;
  end: string;
  secs: number;
  is_idle: boolean;
  ticket_key: string | null;
}

/** Collapse consecutive blocks of the same window into one row. */
function groupTimeline(timeline: TimelineRow[]): TimelineGroup[] {
  const groups: TimelineGroup[] = [];
  for (const b of timeline) {
    const last = groups[groups.length - 1];
    if (last && last.app === b.app && last.title === b.title && last.is_idle === b.is_idle) {
      last.ids.push(b.id);
      last.end = b.end;
      last.secs += b.minutes * 60;
      if (!last.ticket_key && b.ticket_key) last.ticket_key = b.ticket_key;
    } else {
      groups.push({
        ids: [b.id],
        app: b.app,
        title: b.title,
        start: b.start,
        end: b.end,
        secs: b.minutes * 60,
        is_idle: b.is_idle,
        ticket_key: b.ticket_key,
      });
    }
  }
  return groups;
}

function renderTimeline(timeline: TimelineRow[], options: TicketOption[]): void {
  const wrap = $("timeline");
  show($("timeline-empty"), timeline.length === 0);
  if (timeline.length === 0) {
    wrap.innerHTML = "";
    return;
  }

  const keys = options.map((t) => t.key);
  // Newest first: groups are built oldest→newest, so reverse for display.
  const groups = groupTimeline(timeline).reverse();

  wrap.innerHTML = groups
    .map((g) => {
      const opts = ['<option value="">— belum ditempel —</option>']
        .concat(
          options.map(
            (t) =>
              `<option value="${esc(t.key)}"${
                t.key === g.ticket_key ? " selected" : ""
              }>${esc(t.key)}${t.summary ? " — " + esc(t.summary) : ""}</option>`
          )
        )
        .join("");
      const extra =
        g.ticket_key && !keys.includes(g.ticket_key)
          ? `<option value="${esc(g.ticket_key)}" selected>${esc(g.ticket_key)}</option>`
          : "";
      const mins = Math.round(g.secs / 60);
      const dur = mins > 0 ? `${mins}m` : "<1m";
      const picker =
        keys.length === 0
          ? `<select class="tl-pick" disabled title="Sambungkan Jira dulu untuk menempelkan tiket"><option>tiket?</option></select>`
          : `<select class="tl-pick" data-blocks="${g.ids.join(",")}" title="Tempelkan aktivitas ini ke tiket Jira">${opts}${extra}</select>`;
      return `
        <div class="tl-row${g.is_idle ? " idle" : ""}">
          <div class="tl-rail"><span class="tl-dot"></span></div>
          <div class="tl-body">
            <div class="tl-line1">
              <span class="tl-app">${esc(g.app || "—")}</span>
              <span class="tl-dur${g.is_idle ? " idle" : ""}">${g.is_idle ? "idle" : dur}</span>
            </div>
            <div class="tl-name ellipsis" title="${esc(g.title)}">${esc(g.title || "—")}</div>
            <div class="tl-line2">
              <span class="tl-clock">${esc(fmtTime(g.start))}–${esc(fmtTime(g.end))}</span>
              ${picker}
            </div>
          </div>
        </div>`;
    })
    .join("");

  wrap.querySelectorAll<HTMLSelectElement>(".tl-pick").forEach((sel) => {
    sel.addEventListener("change", () => {
      const ids = (sel.dataset.blocks ?? "").split(",").map(Number).filter((n) => !isNaN(n));
      void onTicketCorrection(ids, sel.value);
    });
  });
}

function renderPrs(prs: PrRow[]): void {
  const list = $("pr-list");
  show($("pr-empty"), prs.length === 0);
  if (prs.length === 0) {
    list.innerHTML = "";
    return;
  }
  list.innerHTML = prs
    .map((p) => {
      const state = p.state.toLowerCase();
      return `
        <a class="pr-item" href="${esc(p.url)}" target="_blank" rel="noopener">
          <div class="pr-top">
            <span class="pr-num mono">#${p.number}</span>
            <span class="pr-repo">${esc(p.repo)}</span>
            <span class="chip pr-${esc(state)}">${esc(p.state || "—")}</span>
          </div>
          <div class="pr-title ellipsis" title="${esc(p.title)}">${esc(p.title || "—")}</div>
        </a>`;
    })
    .join("");
}

function renderNotes(notes: string): void {
  ($("note-area") as HTMLTextAreaElement).value = notes ?? "";
}

function render(d: Dashboard): void {
  renderHeader(d.header);
  renderAiSummary(d.ai_summary);
  populateTicketStatusFilter(d.tickets);
  refreshTicketTable();
  renderTimeline(d.timeline, d.all_tickets);
  renderPrs(d.prs);
  renderNotes(d.notes);
}

// ---------------------------------------------------------------------------
// Data loading
// ---------------------------------------------------------------------------

async function loadDashboard(): Promise<void> {
  try {
    dashboard = await invoke<Dashboard>("get_dashboard", { day: currentDay });
    render(dashboard);
  } catch (e) {
    toast(`Gagal memuat dashboard: ${errStr(e)}`, "error");
  }
}

async function refreshRecorderStatus(): Promise<void> {
  try {
    recording = await invoke<boolean>("recorder_status");
  } catch {
    recording = false;
  }
  renderRecorderBtn();
}

function renderRecorderBtn(): void {
  const label = $("rec-label");
  const btn = $("rec-btn");
  if (recording) {
    // Currently recording → clicking will STOP. Show a live indicator + the action.
    label.textContent = "● Merekam — Stop";
    btn.classList.add("recording");
  } else {
    // Currently stopped → clicking will START.
    label.textContent = "▶ Mulai Rekam";
    btn.classList.remove("recording");
  }
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

async function toggleRecorder(): Promise<void> {
  try {
    if (recording) {
      await invoke("recorder_stop");
      recording = false;
      toast("Perekam dihentikan.");
    } else {
      await invoke("recorder_start");
      recording = true;
      toast("Perekam jalan.");
    }
  } catch (e) {
    toast(`Gagal mengubah perekam: ${errStr(e)}`, "error");
    await refreshRecorderStatus();
    return;
  }
  renderRecorderBtn();
}

async function doRefresh(): Promise<void> {
  const btn = $<HTMLButtonElement>("refresh-btn");
  btn.disabled = true;
  const prev = btn.textContent;
  btn.textContent = "Refresh…";
  try {
    // recompute flushes the recorder's in-memory sample buffer to the DB
    // (and rebuilds ticket_time) before we read the dashboard, so live
    // activity shows up without having to stop the recorder first.
    await invoke("recompute", { day: currentDay });
    await loadDashboard();
  } catch (e) {
    toast(`Gagal refresh: ${errStr(e)}`, "error");
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
    await loadDashboard();
  } catch (e) {
    toast(`Sync gagal: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.textContent = "Sync";
  }
}

async function onTicketCorrection(blockIds: number[], ticketKey: string): Promise<void> {
  try {
    // Tauri auto-maps snake_case command params (block_id/ticket_key) to camelCase.
    // A timeline row may represent several merged blocks → correct each.
    for (const blockId of blockIds) {
      await invoke("set_ticket_for_block", { blockId, ticketKey });
    }
    await invoke("recompute", { day: currentDay });
    await loadDashboard();
    toast("Koreksi tiket tersimpan.");
  } catch (e) {
    toast(`Gagal koreksi tiket: ${errStr(e)}`, "error");
    await loadDashboard();
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

/** Entry point from a ticket row's "⤳" button: fetch transitions and let the
 *  user pick one, confirm, then perform it and re-sync. */
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

/** Confirm and perform a chosen transition, then re-sync the dashboard. */
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
    await doSync();
    await loadDashboard();
  } catch (e) {
    toast(`Gagal ubah status: ${errStr(e)}`, "error");
  }
}

async function generateAi(): Promise<void> {
  const btn = $<HTMLButtonElement>("ai-btn");
  btn.disabled = true;
  const prevLabel = btn.textContent;
  btn.textContent = "Menyusun…";
  showAiSkeleton();
  try {
    await invoke<string>("generate_ai_summary", { day: currentDay });
    await loadDashboard();
    toast("Ringkasan AI dibuat.");
  } catch (e) {
    toast(`Gagal bikin ringkasan: ${errStr(e)}`, "error");
    renderAiSummary(dashboard?.ai_summary ?? "");
  } finally {
    btn.disabled = false;
    btn.textContent = prevLabel;
  }
}

async function saveNote(): Promise<void> {
  const body = ($("note-area") as HTMLTextAreaElement).value;
  try {
    await invoke("save_note", { day: currentDay, body });
    const hint = $("note-saved");
    show(hint, true);
    window.setTimeout(() => show(hint, false), 2000);
  } catch (e) {
    toast(`Gagal simpan catatan: ${errStr(e)}`, "error");
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
// Permission banner
// ---------------------------------------------------------------------------

async function checkPermission(): Promise<void> {
  try {
    const ok = await invoke<boolean>("screen_recording_ok");
    show($("perm-banner"), !ok);
  } catch {
    // If the check itself fails, don't nag.
    show($("perm-banner"), false);
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

function wireEvents(): void {
  $("rec-btn").addEventListener("click", () => void toggleRecorder());
  $("sync-btn").addEventListener("click", () => void doSync());
  $("refresh-btn").addEventListener("click", () => void doRefresh());
  $("ai-btn").addEventListener("click", () => void generateAi());
  $("ticket-search").addEventListener("input", () => refreshTicketTable());
  $("ticket-status-filter").addEventListener("change", () => refreshTicketTable());
  $("note-save").addEventListener("click", () => void saveNote());
  $("note-area").addEventListener("blur", () => void saveNote());

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

  $("perm-dismiss").addEventListener("click", () => show($("perm-banner"), false));

  const dateInput = $<HTMLInputElement>("date-input");
  dateInput.addEventListener("change", () => {
    if (dateInput.value) {
      currentDay = dateInput.value;
      void loadDashboard();
    }
  });
}

async function init(): Promise<void> {
  wireEvents();
  try {
    currentDay = await invoke<string>("today");
  } catch {
    currentDay = new Date().toISOString().slice(0, 10);
  }
  $<HTMLInputElement>("date-input").value = currentDay;

  await Promise.all([loadDashboard(), refreshRecorderStatus(), checkPermission()]);
}

window.addEventListener("DOMContentLoaded", () => {
  void init();
});
