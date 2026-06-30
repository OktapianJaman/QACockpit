import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { getVersion } from "@tauri-apps/api/app";

import type {
  BoardTicket,
  TestCase,
  ChatMsg,
  PrRef,
  JiraField,
  JiraProject,
  JiraUser,
  JiraTransition,
  JiraSprint,
  AppConfig,
} from "./types";
import { CONFIG_KEYS, KNOWN_REPOS } from "./constants";
import { esc, mdInline, mdToHtml } from "./markdown";
import { fmtPoints, pointsLabel } from "./format";
import { displayColumn, orderedColumns } from "./board-logic";
import { parsePrRefsFromSummary } from "./pr-ref";
import { $, show, toast, errStr, addCopyButton, initTheme, toggleTheme } from "./dom";
import { wireBugWriter, closeBugWriter, openBugWriter } from "./bugwriter";
import { wireAnnotator, cancelAnnotator } from "./annotate";
import { wirePalette, openPalette, closePalette, type PaletteCommand } from "./palette";

// ---------------------------------------------------------------------------
// Kanban board
// ---------------------------------------------------------------------------

let boardTickets: BoardTicket[] = [];
let boardSearch = "";
// AI output language ("Indonesia" | "English"); drives UI labels that should
// match the generated content (e.g. test-case Steps/Expected). Loaded at init,
// refreshed after Settings save.
let aiLanguage = "Indonesia";
// Configured sprint scope ("" all | "active" | "backlog"); used for the board's
// empty-state hint. Loaded at init, refreshed after Settings save.
let sprintScope = "";
// Key of the card currently being dragged between columns (null = none).
let draggingKey: string | null = null;

/** Build one card (click → detail; inline points; "pindah" → transition picker).
 *  Shows its real Jira status (since a column may merge several statuses). */
function buildCard(t: BoardTicket): HTMLElement {
  const card = document.createElement("div");
  card.className = "kcard";
  // The status badge only adds info when it differs from the column it sits in
  // (e.g. a "QA Passed" card inside the merged "Done" column). When equal, it
  // would just repeat the column header, so drop it.
  const showStatus = displayColumn(t.status).toLowerCase() !== t.status.toLowerCase();
  const statusBadge = showStatus ? `<span class="kc-status">${esc(t.status)}</span>` : "";
  card.innerHTML = `
    <div class="kc-top">
      <span class="kc-key mono">${esc(t.key)}</span>
      ${statusBadge}
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

  // Drag the card to another column to move its status (like Jira).
  card.draggable = true;
  card.addEventListener("dragstart", (e) => {
    draggingKey = t.key;
    card.classList.add("dragging");
    e.dataTransfer?.setData("text/plain", t.key);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
  });
  card.addEventListener("dragend", () => {
    draggingKey = null;
    card.classList.remove("dragging");
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

  // Drop target: dropping a card here moves it toward this column's status.
  col.addEventListener("dragover", (e) => {
    if (!draggingKey) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
    col.classList.add("drag-over");
  });
  col.addEventListener("dragleave", (e) => {
    if (!col.contains(e.relatedTarget as Node)) col.classList.remove("drag-over");
  });
  col.addEventListener("drop", (e) => {
    e.preventDefault();
    col.classList.remove("drag-over");
    const key = draggingKey || e.dataTransfer?.getData("text/plain") || "";
    if (key) void handleCardDrop(status, key);
  });
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
    show(board, false); // collapse the board so its min-height doesn't push the message off-screen
    $("board-empty").textContent =
      sprintScope === "active"
        ? "Belum ada sprint aktif. Start sprint-nya di Jira dulu (Backlog → Start sprint), terus klik Sync."
        : "Belum ada tiket. Klik Sync buat narik dari Jira.";
    return;
  }
  show(board, true);

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
    await updateBoardSummary();
  } catch (e) {
    toast(`Gagal memuat board: ${errStr(e)}`, "error");
  }
}

// Sprint-scope labels matching the Settings "Lingkup Sprint" options.
const SCOPE_LABEL: Record<string, string> = {
  "": "Semua",
  active: "Sprint aktif",
  backlog: "Backlog",
  specific: "Sprint tertentu",
};

/** Show total tickets + story points for the loaded board, labeled by the
 *  configured sprint scope (so it reads "Sprint aktif · 12 tiket · 34 pts"). */
async function updateBoardSummary(): Promise<void> {
  const el = $("board-summary");
  if (boardTickets.length === 0) {
    el.innerHTML = "";
    return;
  }
  const label = SCOPE_LABEL[sprintScope] ?? "Semua";
  const pts = boardTickets.reduce((sum, t) => sum + (t.story_points ?? 0), 0);
  const sep = `<span class="bs-sep">·</span>`;
  el.innerHTML =
    `<span class="bs-scope">${esc(label)}</span>${sep}` +
    `${boardTickets.length} tiket${sep}` +
    `<strong class="bs-pts">${esc(fmtPoints(pts))} pts</strong>`;
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

/** Set a ticket to a target status by name (used by the command palette): find
 *  the Jira transition whose destination matches, then run the normal pick flow
 *  (actual-point prompt for verdicts, confirm, transition, refresh). */
async function setTicketStatus(key: string, target: string): Promise<void> {
  let trans: JiraTransition[];
  try {
    trans = await invoke<JiraTransition[]>("list_transitions", { key });
  } catch (e) {
    toast(`Gagal ambil transisi: ${errStr(e)}`, "error");
    return;
  }
  const tl = target.toLowerCase();
  const match =
    trans.find((t) => (t.to_status || "").toLowerCase() === tl) ??
    trans.find((t) => (t.to_status || "").toLowerCase().includes(tl)) ??
    trans.find((t) => t.name.toLowerCase().includes(tl));
  if (!match) {
    toast(`${key}: nggak ada transisi ke "${target}".`, "error");
    return;
  }
  await onPickTransition(key, match);
}

// Statuses offered per ticket in the command palette (e.g. "QAT-101 → QA Passed").
const PALETTE_STATUSES = ["QA Passed", "QA Failed", "QA In Progress", "Ready for QA", "Today", "Done"];

/** Build the command-palette command list from current app state. Re-read on
 *  every keystroke, so it always reflects the live board. */
function paletteCommands(): PaletteCommand[] {
  const cmds: PaletteCommand[] = [
    { title: "Sync sekarang", subtitle: "tarik tiket & PR terbaru", run: () => void doSync() },
    { title: "Refresh board", run: () => void refreshBoard() },
    { title: "Bug baru", subtitle: "buka Bug Writer", run: () => openBugWriter() },
    { title: "Ringkasan harian", subtitle: "daily summary", run: () => void openSummary() },
    { title: "Ticket Builder", subtitle: "bikin Story massal dari epic", run: () => openTicketBuilder() },
    { title: "Settings", subtitle: "kredensial Jira / GitHub / Gemini", run: () => void openSettings() },
    { title: "Ganti tema", subtitle: "gelap / terang", run: () => toggleTheme() },
  ];
  for (const t of boardTickets) {
    cmds.push({
      title: `Buka ${t.key}`,
      subtitle: t.summary,
      run: () => void openDetail(t.key),
    });
    for (const s of PALETTE_STATUSES) {
      cmds.push({
        title: `${t.key} → ${s}`,
        subtitle: t.summary,
        run: () => void setTicketStatus(t.key, s),
      });
    }
  }
  return cmds;
}

/** Handle a card dropped on a column: pick the transition that leads to that
 *  column's status. One match → run it (with the actual-point prompt for
 *  verdicts); several → show the picker; none → tell the user it's not allowed. */
async function handleCardDrop(targetCol: string, key: string): Promise<void> {
  const t = ticketByKey(key);
  if (!t || displayColumn(t.status) === targetCol) return; // unknown or same column
  let trans: JiraTransition[];
  try {
    trans = await invoke<JiraTransition[]>("list_transitions", { key });
  } catch (e) {
    toast(`Gagal ambil transisi: ${errStr(e)}`, "error");
    return;
  }
  const target = targetCol.toLowerCase();
  const matches = trans.filter(
    (tr) => displayColumn(tr.to_status || tr.name).toLowerCase() === target
  );
  if (matches.length === 1) {
    await onPickTransition(key, matches[0]);
  } else if (matches.length === 0) {
    toast(`${key} nggak bisa langsung dipindah ke "${targetCol}".`, "error");
  } else {
    showTransitionPicker(key, matches); // ambiguous → let the user choose
  }
}

// ---------------------------------------------------------------------------
// Ticket detail modal + test cases
// ---------------------------------------------------------------------------

// The ticket whose detail modal is currently open (null = closed).
let detailKey: string | null = null;
/** Jira base URL (no trailing slash) for the currently open detail, "" if unset. */
let jiraBrowseBase = "";
// PRs linked to the open ticket (a ticket can span repos, e.g. native + flutter).
let linkedPrs: PrRef[] = [];
// Ticket key whose auto-populate run currently owns the loading banner (null =
// idle). Tying it to a key avoids a stale run from ticket A hiding the banner
// after the user has already switched to ticket B. `autoBusy` is the boolean the
// manual action-button guards read.
let autoBusyKey: string | null = null;
let autoBusy = false;

/** Show/refresh the auto-populate loading banner for `key` (null = hide) and
 *  lock the action buttons so QA can't trigger a duplicate fetch/generate. */
function setAutoBusy(key: string | null, text = ""): void {
  autoBusyKey = key;
  autoBusy = key !== null;
  document.querySelector(".detail-panel")?.classList.toggle("auto-busy", autoBusy);
  show($("detail-auto"), autoBusy);
  if (autoBusy && text) $("detail-auto-text").textContent = text;
}
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
  if (autoBusy) return void toast("Sabar, tiket lagi disiapin otomatis…");
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
  // Make the key open the Jira issue in a browser, when a base URL is configured.
  jiraBrowseBase = "";
  $("detail-key").classList.remove("has-link");
  void invoke<AppConfig>("get_config")
    .then((cfg) => {
      jiraBrowseBase = (cfg.jira_base_url || "").trim().replace(/\/+$/, "");
      $("detail-key").classList.toggle("has-link", jiraBrowseBase !== "");
      $("detail-key").title = jiraBrowseBase ? "Buka di Jira" : "";
    })
    .catch(() => {});
  $("detail-summary").textContent = t?.summary || "—";
  const statusEl = $("detail-status");
  statusEl.textContent = t?.status || "—";
  show($("tc-add-form"), false);
  ($("tc-add-form") as HTMLFormElement).reset();
  $("tc-list").innerHTML = "";
  show($("tc-empty"), false);
  $("tc-counter").textContent = "";
  // Reset triage panel from any previous ticket.
  show($("triage-panel"), false);
  triageCases = [];
  $("triage-buckets").innerHTML = "";
  $("triage-detail").innerHTML = "";
  // Reset the PR tab (clear linked PRs from the previous ticket).
  linkedPrs = [];
  populateRepoDropdown();
  $("pr-list").innerHTML = "";
  show($("pr-empty"), true);
  $("pr-empty").textContent = "Tempel link PR di atas (boleh lebih dari satu), atau cari otomatis.";
  selectTab("testcases");
  show($("detail-overlay"), true);
  const cases = await loadTestCases(key);
  void refreshQaBranchBadge(); // show if the QA branch is behind develop
  // Fire-and-forget: if the summary follows the "[GTG] … #3182" convention,
  // resolve + attach the PR, summarize it, and seed test cases — all in the
  // background so the modal is interactive immediately.
  void autoPopulateFromSummary(key, cases.length);
}

/** Auto-complete a ticket from its summary convention ("[GTI]/[GTG] … #NNNN").
 *  Attaches the referenced PR(s), generates each PR's AI summary when none is
 *  cached, and seeds test cases when the ticket has none. Every step is guarded
 *  on `detailKey` (a ticket switch mid-flight aborts cleanly) and idempotent —
 *  reopening a ticket never regenerates work that already exists. */
async function autoPopulateFromSummary(key: string, existingCases: number): Promise<void> {
  const t = ticketByKey(key);
  if (!t) return;
  const refs = parsePrRefsFromSummary(t.summary);
  if (refs.length === 0) return; // ticket doesn't follow the convention

  setAutoBusy(key, "Lagi nyiapin tiket: ambil PR…");
  try {
    let added = false;
    for (const r of refs) {
      added =
        addLinkedPr({
          number: r.number,
          repo: r.repo,
          title: `PR #${r.number}`,
          state: "",
          url: `https://github.com/${r.repo}/pull/${r.number}`,
        }) || added;
    }
    if (!added || detailKey !== key) return;
    renderPrs();

    // Summarize each PR that has no cached summary yet (sequential; gentle on AI).
    const items = $("pr-list").querySelectorAll<HTMLElement>(".pr-item");
    for (let i = 0; i < linkedPrs.length; i++) {
      if (detailKey !== key) return;
      const pr = linkedPrs[i];
      const item = items[i];
      const btn = item?.querySelector<HTMLButtonElement>(".pr-summarize");
      const panel = item?.querySelector<HTMLDivElement>(".pr-review");
      if (!btn || !panel) continue;
      const st = await invoke<{ summary: string | null }>("get_pr_state", {
        repo: pr.repo,
        number: pr.number,
      }).catch(() => null);
      if (st?.summary) continue; // already summarized — keep the cache
      setAutoBusy(key, `Lagi bikin ringkasan PR #${pr.number}…`);
      await summarizePr(pr, btn, panel);
    }

    // Seed test cases only when the ticket has none (manual regen still available).
    if (detailKey === key && existingCases === 0 && linkedPrs.length > 0) {
      setAutoBusy(key, "Lagi bikin test case dari PR…");
      const prs = linkedPrs.map((p) => ({ repo: p.repo, number: p.number }));
      const created = await invoke<TestCase[]>("generate_test_cases_from_prs", {
        key,
        summary: t.summary,
        prs,
      });
      if (detailKey === key) {
        await loadTestCases(key);
        if (created.length) toast(`${created.length} test case otomatis dari PR.`);
      }
    }
  } catch (e) {
    toast(errStr(e), "error");
  } finally {
    if (autoBusyKey === key) setAutoBusy(null); // don't clobber a newer ticket's run
  }
}

/** Pill class for a test-case status. */
function tcStatusClass(status: string): string {
  if (status === "passed") return "tc-pill passed";
  if (status === "failed") return "tc-pill failed";
  if (status === "not_auto") return "tc-pill not-auto";
  if (status === "running") return "tc-pill running";
  return "tc-pill untested";
}

function tcStatusLabel(status: string): string {
  if (status === "passed") return "✅ passed";
  if (status === "failed") return "❌ failed";
  if (status === "not_auto") return "🚩 not-auto";
  if (status === "running") return "⏳ jalan…";
  return "untested";
}

/** Localized labels for the test-case detail fields, following the AI output
 *  language so they match the generated content. */
function tcLabels(): { steps: string; expected: string; notes: string; notesPlaceholder: string } {
  const en = aiLanguage === "English";
  return en
    ? { steps: "Steps", expected: "Expected result", notes: "Notes", notesPlaceholder: "Notes / actual result…" }
    : { steps: "Langkah", expected: "Hasil yang diharapkan", notes: "Catatan", notesPlaceholder: "Catatan / hasil aktual…" };
}

/** Split a steps string into individual steps. Prefers explicit line breaks;
 *  otherwise splits on inline numbered markers ("1. ", "2) ", …). Leading
 *  numbering is stripped since the list re-numbers. */
function splitSteps(steps: string): string[] {
  const s = steps.trim();
  let parts = s.split(/\r?\n+/).map((x) => x.trim()).filter(Boolean);
  if (parts.length <= 1) {
    parts = s.split(/\s+(?=\d+[.)]\s)/).map((x) => x.trim()).filter(Boolean);
  }
  return parts.map((p) => p.replace(/^\d+[.)]\s*/, "")).filter(Boolean);
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
    const L = tcLabels();
    const stepsHtml = c.steps
      ? `<div class="tc-field tc-field-steps">
           <span class="tc-label">${L.steps}</span>
           <ol class="tc-steps">${splitSteps(c.steps)
             .map((s) => `<li>${mdInline(esc(s))}</li>`)
             .join("")}</ol>
         </div>`
      : "";
    const expectedHtml = c.expected
      ? `<div class="tc-field tc-field-expected">
           <span class="tc-label">${L.expected}</span>
           <div class="tc-value">${mdInline(esc(c.expected))}</div>
         </div>`
      : "";
    // The detail panel always exists now (it hosts the editable notes field),
    // even when a case has no steps/expected.
    // A [manual] marker means the verdict was hand-set via the ✅/❌ buttons, not
    // produced by automation — surface that honestly instead of as a real result.
    const isManual = (c.verdict_reason ?? "").startsWith("[manual]");
    const reasonHtml = isManual
      ? `<div class="tc-reason manual">✋ Status di-set manual oleh QA (bukan hasil automation)</div>`
      : c.verdict_reason
        ? `<div class="tc-reason ${c.status === "not_auto" ? "not-auto" : "fail"}">
             ${c.status === "not_auto" ? "🚩" : "❌"} ${esc(c.verdict_reason)}
           </div>`
        : "";
    item.innerHTML = `
      <div class="tc-item-head">
        <span class="${tcStatusClass(c.status)}${isManual ? " manual" : ""}">${esc(tcStatusLabel(c.status))}${isManual ? " · ✋manual" : ""}</span>
        <span class="tc-title">${esc(c.title)}</span>
        <button class="tc-toggle" type="button" title="Lihat detail">▾</button>
        <div class="tc-item-actions">
          <button class="btn small primary tc-run" type="button" title="Jalankan test case (widget test)">▶</button>
          <button class="btn small tc-pass" type="button" title="Pass">✅</button>
          <button class="btn small tc-fail" type="button" title="Fail">❌</button>
          <button class="btn small tc-del" type="button" title="Hapus">🗑</button>
        </div>
      </div>
      <div class="tc-detail">
        ${reasonHtml}
        ${stepsHtml}
        ${expectedHtml}
        <div class="tc-field">
          <span class="tc-label">${L.notes}</span>
          <textarea class="tc-notes" rows="2" placeholder="${L.notesPlaceholder}">${esc(c.notes)}</textarea>
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

    item.querySelector<HTMLButtonElement>(".tc-run")?.addEventListener("click", () =>
      void runOnDevice(c.id, c.title)
    );
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
async function loadTestCases(key: string): Promise<TestCase[]> {
  try {
    const cases = await invoke<TestCase[]>("list_test_cases", { key });
    if (detailKey === key) renderTestCases(cases);
    return cases;
  } catch (e) {
    toast(`Gagal memuat test case: ${errStr(e)}`, "error");
    return [];
  }
}

async function setTestCaseStatus(id: number, status: string): Promise<void> {
  if (!detailKey) return;
  const label = status === "passed" ? "PASSED" : status === "failed" ? "FAILED" : status;
  const ok = await confirmDialog(
    `Set status ke ${label} secara MANUAL?\n\nIni override manual (bukan hasil automation) dan bakal ditandai "✋manual". Buat hasil sebenarnya, pakai tombol ▶ (run).`,
  );
  if (!ok) return;
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

// --- Device Run drawer: live execution of a test case on the device ----------

interface DevRunMsg {
  id: number;
  t: "phase" | "frame" | "log" | "verdict" | "done" | "progress" | "edit";
  key?: string;
  label?: string;
  state?: string;
  png?: string;
  msg?: string;
  verdict?: string;
  reason?: string;
  status?: string;
  pct?: number | null;
  downloaded?: number;
  total?: number;
  file?: string;
  line?: number;
  snippet?: string;
  action?: string;
}

/** id of the test case loaded in the drawer (null = none open). */
let devrunActiveId: number | null = null;
/** true while a device run is in flight (guards close / double-start). */
let devrunRunning = false;

/** Clear the drawer's run output (phases/verdict/frame) for a fresh case. */
function devrunReset(id: number, title: string): void {
  devrunActiveId = id;
  $("devrun-title").textContent = title;
  $("devrun-phases").innerHTML = "";
  $("devrun-edits").innerHTML = "";
  const v = $("devrun-verdict");
  v.className = "devrun-verdict hidden";
  v.textContent = "";
  const img = $("devrun-frame") as HTMLImageElement;
  img.removeAttribute("src");
  show($("devrun-frame-empty"), true);
}

/** Default test-login email per app (used until the user customizes it). */
const DEFAULT_EMAIL: Record<string, string> = {
  GTI: "okta+amba1@tr8.io",
  GTG: "",
};

/** Load the saved (or default) login email for the currently-selected app into
 *  the drawer's email field. Persisted per app in localStorage. */
function devrunLoadEmail(): void {
  const appKey = ($("devrun-itest-app") as HTMLSelectElement).value;
  const saved = localStorage.getItem(`qa_email_${appKey}`);
  ($("devrun-email") as HTMLInputElement).value = saved ?? DEFAULT_EMAIL[appKey] ?? "";
}

/** Populate the device dropdown from `adb devices`; disable Start when none. */
async function loadDevrunDevices(): Promise<void> {
  const sel = $("devrun-device") as HTMLSelectElement;
  const start = $("devrun-start") as HTMLButtonElement;
  try {
    const devs = await invoke<string[]>("list_adb_devices");
    sel.innerHTML = "";
    if (devs.length === 0) {
      const o = document.createElement("option");
      o.value = "";
      o.textContent = "(tidak ada device)";
      sel.appendChild(o);
      start.disabled = true;
    } else {
      for (const d of devs) {
        const o = document.createElement("option");
        o.value = d;
        o.textContent = d;
        sel.appendChild(o);
      }
      start.disabled = devrunRunning;
    }
  } catch (e) {
    toast(`Gagal baca device: ${errStr(e)}`, "error");
  }
}

function devrunPhase(key: string, label: string, state: string): void {
  const list = $("devrun-phases");
  let li = list.querySelector<HTMLElement>(`[data-phase="${key}"]`);
  if (!li) {
    li = document.createElement("li");
    li.dataset.phase = key;
    list.appendChild(li);
  }
  li.dataset.label = label;
  li.className = state === "done" ? "done" : "active";
  li.innerHTML = `<span class="devrun-ph-icon">${state === "done" ? "✔" : "⏳"}</span> ${esc(label)}`;
}

/** Show a download progress bar + % on a phase row (e.g. the Firebase APK fetch). */
function devrunProgress(key: string, pct: number | null, downloaded: number, total: number): void {
  const li = $("devrun-phases").querySelector<HTMLElement>(`[data-phase="${key}"]`);
  if (!li) return;
  const mb = (n: number): string => (n / 1048576).toFixed(1);
  const sub = total ? `${pct ?? 0}% · ${mb(downloaded)}/${mb(total)} MB` : `${mb(downloaded)} MB`;
  const width = total && pct != null ? pct : 0;
  li.className = "active";
  li.innerHTML = `<span class="devrun-ph-icon">⏳</span> ${esc(li.dataset.label ?? "")}
    <span class="devrun-pct">${esc(sub)}</span>
    <div class="devrun-bar"><div class="devrun-bar-fill" style="width:${width}%"></div></div>`;
}

/** Append a live-coding entry: a QA Key the instrumenter added/found in source. */
function devrunEdit(e: DevRunMsg): void {
  const list = $("devrun-edits");
  const act =
    e.action === "added" ? "➕ ditambahkan"
    : e.action === "present" ? "✓ sudah ada"
    : e.action === "missing" ? "⚠ file hilang"
    : e.action === "skip" ? "⚠ anchor tak ketemu"
    : (e.action ?? "");
  const li = document.createElement("li");
  li.className = `devrun-edit ${e.action ?? ""}`;
  li.innerHTML = `<div class="devrun-edit-head"><code>${esc(e.key ?? "")}</code><span class="devrun-edit-act">${esc(act)}</span></div>
    <div class="devrun-edit-loc">${esc(e.file ?? "")}${e.line ? `:${e.line}` : ""}</div>
    ${e.snippet ? `<pre class="devrun-edit-snip">${esc(e.snippet)}</pre>` : ""}`;
  list.appendChild(li);
  list.scrollTop = list.scrollHeight;
}

function devrunVerdict(verdict: string, reason: string): void {
  const map: Record<string, string> = {
    pass: "🟢 AUTO-PASS",
    fail: "🔴 AUTO-FAIL",
    not_auto: "🚩 NOT-AUTO — perlu tes manual",
  };
  const v = $("devrun-verdict");
  v.className = `devrun-verdict ${verdict}`;
  v.innerHTML = `<strong>${map[verdict] ?? map.not_auto}</strong>${
    reason ? `<div class="devrun-reason">${esc(reason)}</div>` : ""
  }`;
}

interface FbBuild {
  version: string;
  date: string;
  notes: string;
}

/** Populate the Firebase build dropdown for the selected app. The first option
 *  ("pakai yang sudah keinstall") skips download and runs the installed app. */
async function loadFirebaseBuilds(): Promise<void> {
  const sel = $("devrun-build") as HTMLSelectElement;
  const app = ($("devrun-app") as HTMLSelectElement).value;
  sel.innerHTML = '<option value="">⏳ memuat dari Firebase…</option>';
  try {
    const builds = await invoke<FbBuild[]>("list_firebase_builds", { app });
    sel.innerHTML = "";
    const skip = document.createElement("option");
    skip.value = "";
    skip.textContent = "— pakai app yang sudah keinstall —";
    sel.appendChild(skip);
    for (const b of builds) {
      const o = document.createElement("option");
      o.value = b.version;
      o.textContent = `${b.version}  ·  ${b.date}`;
      sel.appendChild(o);
    }
  } catch (e) {
    sel.innerHTML = '<option value="">(gagal load Firebase)</option>';
    toast(`Gagal load build Firebase: ${errStr(e)}`, "error");
  }
}

/** ▶ on a test case: open the drawer in config state (pick device, then Start). */
async function runOnDevice(id: number, title: string): Promise<void> {
  if (devrunRunning) {
    toast("Masih ada run yang jalan — tunggu selesai.", "error");
    return;
  }
  devrunReset(id, title);
  devrunLoadEmail();
  show($("devrun-overlay"), true);
  await loadDevrunDevices();
}

/** Toggle the drawer between idle (Start, controls enabled) and running (Stop). */
function devrunSetRunning(running: boolean): void {
  devrunRunning = running;
  show($("devrun-start"), !running);
  show($("devrun-stop"), running);
  ($("devrun-device") as HTMLSelectElement).disabled = running;
  ($("devrun-itest-app") as HTMLSelectElement).disabled = running;
  ($("devrun-email") as HTMLInputElement).disabled = running;
}

/** Start the run for the case currently in the drawer, on the chosen device. */
async function devrunStart(): Promise<void> {
  if (devrunActiveId === null || devrunRunning) return;
  const id = devrunActiveId;
  const serial = ($("devrun-device") as HTMLSelectElement).value;
  if (!serial) {
    toast("Pilih device dulu (atau nyalakan emulator).", "error");
    return;
  }
  devrunSetRunning(true);
  $("devrun-phases").innerHTML = "";
  $("devrun-edits").innerHTML = "";
  const v = $("devrun-verdict");
  v.className = "devrun-verdict hidden";
  v.textContent = "";
  const appKey = ($("devrun-itest-app") as HTMLSelectElement).value;
  const email = ($("devrun-email") as HTMLInputElement).value.trim();
  localStorage.setItem(`qa_email_${appKey}`, email); // remember per app
  try {
    // Flutter widget-test track: triage by case title → run the real widget test
    // (deterministic `flutter test`, no device). Command name kept for compat.
    await invoke("run_integration_test", { id, serial, appKey, email });
  } catch (e) {
    devrunVerdict("not_auto", errStr(e));
  } finally {
    devrunSetRunning(false);
    if (detailKey) await loadTestCases(detailKey);
  }
}

/** Stop the in-flight run (kills the bridge subprocess); the run promise then
 *  resolves and re-enables the controls. */
async function devrunStop(): Promise<void> {
  if (devrunActiveId === null || !devrunRunning) return;
  try {
    await invoke("stop_device_run", { id: devrunActiveId });
    toast("Menghentikan run…");
  } catch (e) {
    toast(`Gagal stop: ${errStr(e)}`, "error");
  }
}

/** Wire the drawer controls + the streamed `device_run` events (once at startup). */
function wireDeviceRun(): void {
  $("devrun-close")?.addEventListener("click", () => {
    if (devrunRunning) {
      toast("Run masih jalan — tunggu selesai.", "error");
      return;
    }
    show($("devrun-overlay"), false);
  });
  $("devrun-refresh")?.addEventListener("click", () => void loadDevrunDevices());
  $("devrun-itest-app")?.addEventListener("change", devrunLoadEmail);
  $("devrun-itest-app")?.addEventListener("change", () => void refreshQaBranchBadge());
  $("devrun-app")?.addEventListener("change", () => void loadFirebaseBuilds());
  $("devrun-build-refresh")?.addEventListener("click", () => void loadFirebaseBuilds());
  $("devrun-start")?.addEventListener("click", () => void devrunStart());
  $("devrun-stop")?.addEventListener("click", () => void devrunStop());
  void listen<DevRunMsg>("device_run", (ev) => {
    const p = ev.payload;
    if (devrunActiveId !== null && p.id !== devrunActiveId) return;
    if (p.t === "frame" && p.png) {
      ($("devrun-frame") as HTMLImageElement).src = p.png;
      show($("devrun-frame-empty"), false);
    } else if (p.t === "phase") {
      devrunPhase(p.key ?? "", p.label ?? "", p.state ?? "");
    } else if (p.t === "progress") {
      devrunProgress(p.key ?? "", p.pct ?? null, p.downloaded ?? 0, p.total ?? 0);
    } else if (p.t === "edit") {
      devrunEdit(p);
    } else if (p.t === "verdict") {
      devrunVerdict(p.verdict ?? "not_auto", p.reason ?? "");
    }
  });

  // Ticket triage stream: one row per case as it resolves.
  void listen<TriageMsg>("triage", (ev) => {
    const p = ev.payload;
    if (p.t === "start") {
      triageCases = [];
      ($("triage-progress") as HTMLElement).textContent = `0 / ${p.total ?? 0}`;
      renderTriage();
    } else if (p.t === "pr_context") {
      // Retrieval upgrade: classification is grounded in the ticket's real PR diff.
      const t = $("triage-title") as HTMLElement;
      t.textContent = `${t.textContent?.split(" · ")[0] ?? t.textContent} · 📎 diff PR (${p.files ?? 0} file)`;
    } else if (p.t === "running") {
      ($("triage-progress") as HTMLElement).textContent =
        `${p.done ?? 0} / ${p.total ?? 0} — lagi cek: ${p.title ?? ""}`;
    } else if (p.t === "case" && p.case) {
      triageCases.push(p.case);
      ($("triage-progress") as HTMLElement).textContent = `${p.done ?? 0} / ${p.total ?? 0}`;
      renderTriage();
    } else if (p.t === "done") {
      ($("triage-progress") as HTMLElement).textContent = `selesai — ${p.total ?? 0} test case`;
      renderTriage();
    }
  });

  // Auto-gen ("✨ Bikin test") progress stream — live coding panel.
  void listen<GenMsg>("gen_run", (ev) => {
    const p = ev.payload;
    if (p.id === undefined) return;
    const host = genLive(p.id);
    const setStatus = (s: string) => {
      const el = host?.querySelector<HTMLElement>(".gen-status");
      if (el) el.textContent = s;
    };
    if (p.t === "phase") {
      setStatus(`⚙️ ${p.label}…`);
    } else if (p.t === "attempt") {
      setStatus(`🔁 percobaan ${p.n}/${p.total} — nulis test…`);
      const errEl = host?.querySelector<HTMLElement>(".gen-error");
      if (errEl) errEl.textContent = ""; // clear previous attempt's error
    } else if (p.t === "step") {
      setStatus(p.msg ?? "");
    } else if (p.t === "code") {
      const code = host?.querySelector<HTMLElement>(".gen-code");
      if (code) {
        code.textContent = p.dart ?? "";
        code.scrollTop = code.scrollHeight; // follow the "typing"
      }
    } else if (p.t === "error") {
      const errEl = host?.querySelector<HTMLElement>(".gen-error");
      if (errEl) errEl.textContent = `compile/test error:\n${p.error ?? ""}`;
    } else if (p.t === "done") {
      const c = triageCases.find((x) => x.id === p.id);
      if (c && p.verdict === "pass") {
        c.bucket = "auto";
        c.verdict = "pass";
        c.reason = "Test auto-generated (AI 2.5 Pro), lulus & tersimpan.";
        renderTriage();
        toast("Test berhasil dibikin & lulus ✅");
        if (detailKey) void loadTestCases(detailKey);
      } else {
        // Generator couldn't build it honestly → reclassify as Manual with the reason.
        const c = triageCases.find((x) => x.id === p.id);
        if (c) {
          c.bucket = "manual";
          c.reason = p.reason ?? "Auto-gen gagal — perlu tes manual.";
          renderTriage();
          toast("Nggak bisa auto-build — ditandai Manual (jujur, bukan ijo palsu).");
        } else {
          setStatus(`❌ ${p.reason ?? "gagal — perlu manual"}`);
        }
      }
    }
  });

  // Bulk generation tally (the "⚙️ Generate semua buildable" run).
  void listen<GenBulkMsg>("gen_bulk", (ev) => {
    const p = ev.payload;
    const prog = $("gen-bulk-progress");
    if (p.t === "start") {
      show(prog, true);
      prog.textContent = `Generate ${p.total ?? 0} case buildable… 0 selesai`;
    } else if (p.t === "case") {
      prog.textContent = `(${(p.done ?? 0) + 1}/${p.total ?? 0}) lagi generate: ${p.title ?? ""}`;
    } else if (p.t === "case_done") {
      prog.textContent = `${p.done ?? 0}/${p.total ?? 0} selesai — ✅ ${p.passed ?? 0} lulus · 🖐 ${p.manual ?? 0} manual`;
      // Reflect the per-case result in the triage list immediately.
      const c = triageCases.find((x) => x.id === p.id);
      if (c) {
        if (p.verdict === "pass") { c.bucket = "auto"; c.verdict = "pass"; c.reason = "Auto-gen lulus & tersimpan."; }
        else { c.bucket = "manual"; }
        renderTriage();
      }
    } else if (p.t === "done") {
      prog.textContent = `Selesai — dari ${p.total ?? 0} buildable: ✅ ${p.passed ?? 0} lulus, 🖐 ${p.manual ?? 0} manual/draft.`;
      toast(`Bulk generate kelar: ${p.passed ?? 0} lulus, ${p.manual ?? 0} manual.`);
      if (detailKey) void loadTestCases(detailKey);
    }
  });
}

// --- Ticket triage --------------------------------------------------------
interface TriageCase {
  id: number;
  title: string;
  bucket: string; // auto | spec_drift | buildable | manual | unknown | config
  verdict: string; // pass | fail | not_auto
  reason: string;
}
interface TriageMsg {
  t: "start" | "running" | "case" | "done" | "pr_context";
  total?: number;
  done?: number;
  id?: number;
  title?: string;
  case?: TriageCase;
  files?: number;
}

interface GenBulkMsg {
  t: "start" | "case" | "case_done" | "done";
  ticket?: string;
  id?: number;
  title?: string;
  verdict?: string;
  done?: number;
  total?: number;
  passed?: number;
  manual?: number;
}

interface GenMsg {
  t: "phase" | "attempt" | "step" | "code" | "error" | "verdict" | "done";
  id?: number;
  label?: string;
  state?: string;
  n?: number;
  total?: number;
  msg?: string;
  dart?: string;
  error?: string;
  attempt?: number;
  verdict?: string;
  reason?: string;
}

/** Ensure the live-coding panel exists inside a buildable row's progress slot. */
function genLive(id: number): HTMLElement | null {
  const host = document.querySelector<HTMLElement>(`[data-gen="${id}"]`);
  if (!host) return null;
  // Auto-expand the row's detail so live progress/code is visible.
  document.getElementById(`trd-${id}`)?.classList.remove("hidden");
  if (!host.querySelector(".gen-code")) {
    host.innerHTML =
      `<div class="gen-status"></div>` +
      `<pre class="gen-code"></pre>` +
      `<div class="gen-error"></div>`;
  }
  return host;
}

let triageCases: TriageCase[] = [];

// Map a case to a display group: auto-pass, real bug, spec-drift, buildable, manual.
const TRIAGE_GROUPS: { key: string; label: string; cls: string; match: (c: TriageCase) => boolean }[] = [
  { key: "pass", label: "✅ Auto-pass", cls: "g-pass", match: (c) => c.bucket === "auto" && c.verdict === "pass" },
  { key: "fail", label: "❌ FAIL (bug!)", cls: "g-fail", match: (c) => c.bucket === "auto" && c.verdict === "fail" },
  { key: "auto", label: "▶ Bisa di-run", cls: "g-auto", match: (c) => c.bucket === "auto" },
  { key: "spec_drift", label: "🚩 Spec ≠ build", cls: "g-drift", match: (c) => c.bucket === "spec_drift" },
  { key: "buildable", label: "🔧 Buildable", cls: "g-build", match: (c) => c.bucket === "buildable" },
  { key: "manual", label: "✋ Manual", cls: "g-manual", match: (c) => c.bucket === "manual" },
  { key: "unknown", label: "❔ Belum di-triage", cls: "g-unknown", match: () => true },
];

function groupOf(c: TriageCase): { key: string; label: string; cls: string } {
  return TRIAGE_GROUPS.find((g) => g.match(c))!;
}

// Sort order for the table: actionable items (bugs, buildable) first, the
// already-green auto-pass cases last — you scan what needs work, not what's done.
const GROUP_PRIO: Record<string, number> = {
  fail: 0, buildable: 1, spec_drift: 2, manual: 3, auto: 4, pass: 5, unknown: 6,
};

function renderTriage(): void {
  const buckets = $("triage-buckets");
  const detail = $("triage-detail");
  buckets.innerHTML = "";
  // Summary chips (counts per group, declaration order wins on overlap).
  const counts = new Map<string, number>();
  for (const c of triageCases) {
    const g = groupOf(c);
    counts.set(g.key, (counts.get(g.key) ?? 0) + 1);
  }
  for (const g of TRIAGE_GROUPS) {
    const n = counts.get(g.key) ?? 0;
    if (n === 0) continue;
    const chip = document.createElement("span");
    chip.className = `triage-chip ${g.cls}`;
    chip.textContent = `${g.label}: ${n}`;
    buckets.appendChild(chip);
  }
  // Bulk "Generate semua buildable" button only when there's >1 to do.
  const buildableN = counts.get("buildable") ?? 0;
  const genAll = $<HTMLButtonElement>("tc-gen-all");
  show(genAll, buildableN > 1);
  if (buildableN > 1) genAll.textContent = `⚙️ Generate semua buildable (${buildableN})`;

  // Table: one row per case, sorted actionable-first. Code/error live in a
  // collapsible detail (expanded on click, and auto-expanded while generating)
  // so the table stays a scannable map instead of a wall of streaming code.
  const rows = [...triageCases].sort(
    (a, b) => (GROUP_PRIO[groupOf(a).key] ?? 9) - (GROUP_PRIO[groupOf(b).key] ?? 9),
  );
  detail.innerHTML =
    `<div class="tr-table">` +
    rows.map((c) => {
      const g = groupOf(c);
      const icon = g.label.split(" ")[0];               // leading emoji glyph
      const tag = g.label.replace(/^\S+\s*/, "");        // text after the glyph
      const act = c.bucket === "buildable"
        ? `<button class="btn small primary gen-btn" data-id="${c.id}">✨ Bikin</button>`
        : `<button class="tr-toggle" data-id="${c.id}" title="Lihat detail">▸</button>`;
      return (
        `<div class="tr-row" data-id="${c.id}">` +
          `<span class="tr-status ${g.cls}" title="${esc(tag)}">${icon}</span>` +
          `<span class="tr-main">` +
            `<span class="tr-title">${esc(c.title)}</span>` +
            (c.reason ? `<span class="tr-reason">${esc(c.reason)}</span>` : "") +
          `</span>` +
          `<span class="tr-bucket ${g.cls}">${esc(tag)}</span>` +
          `<span class="tr-act">${act}</span>` +
        `</div>` +
        `<div class="tr-detail hidden" id="trd-${c.id}">` +
          (c.reason ? `<div class="tr-reason-full">${esc(c.reason)}</div>` : "") +
          `<div class="gen-progress" data-gen="${c.id}"></div>` +
        `</div>`
      );
    }).join("") +
    `</div>`;

  const toggle = (id: string | undefined) => {
    if (!id) return;
    document.getElementById(`trd-${id}`)?.classList.toggle("hidden");
  };
  detail.querySelectorAll<HTMLButtonElement>(".gen-btn").forEach((btn) => {
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      void generateCaseTest(Number(btn.dataset.id), btn);
    });
  });
  detail.querySelectorAll<HTMLButtonElement>(".tr-toggle").forEach((b) => {
    b.addEventListener("click", (e) => { e.stopPropagation(); toggle(b.dataset.id); });
  });
  detail.querySelectorAll<HTMLElement>(".tr-row").forEach((row) => {
    row.addEventListener("click", () => toggle(row.dataset.id));
  });
}

/** "✨ Bikin test": auto-generate a Flutter test for a buildable case (Gemini 2.5
 *  Pro + compile/repair loop). Progress streams via the "gen_run" event. */
async function generateCaseTest(id: number, btn: HTMLButtonElement): Promise<void> {
  btn.disabled = true;
  btn.textContent = "✨ Generating…";
  document.getElementById(`trd-${id}`)?.classList.remove("hidden"); // show live detail
  const prog = document.querySelector<HTMLElement>(`[data-gen="${id}"]`);
  if (prog) prog.textContent = "mulai…";
  try {
    await invoke("generate_case_test", { id });
  } catch (err) {
    if (prog) prog.textContent = `gagal: ${errStr(err)}`;
    btn.disabled = false;
    btn.textContent = "✨ Bikin test";
  }
}

/** "⚙️ Generate semua buildable": bulk-generate tests for every buildable case
 *  in the open ticket. Per-case live updates reuse the gen_run stream; overall
 *  tally comes via gen_bulk. */
async function generateAllBuildable(): Promise<void> {
  if (!detailKey) return;
  const n = triageCases.filter((c) => c.bucket === "buildable").length;
  if (n === 0) return;
  const btn = $<HTMLButtonElement>("tc-gen-all");
  btn.disabled = true;
  const prog = $("gen-bulk-progress");
  show(prog, true);
  prog.textContent = `Mulai generate ${n} case buildable… (paralel 3 sekaligus)`;
  try {
    await invoke("generate_ticket_tests", { ticket: detailKey, max: null });
  } catch (err) {
    toast(`Bulk generate gagal: ${errStr(err)}`, "error");
  } finally {
    btn.disabled = false;
    if (detailKey) await loadTestCases(detailKey);
  }
}

/** "▶ Run semua test": re-run every generated widget test in ONE batched
 * flutter invocation (~3× faster than per-file) — a fast regression check. */
async function runAllGenerated(): Promise<void> {
  const btn = $<HTMLButtonElement>("tc-run-all");
  const out = $("run-all-result");
  btn.disabled = true;
  const was = btn.textContent;
  btn.textContent = "▶ Lagi run…";
  show(out, true);
  out.textContent = "Build + run semua test ter-generate (satu invocation)…";
  const t0 = performance.now();
  try {
    const r = await invoke<{ total: number; passed: number; failed: number; all_pass: boolean; tail?: string; reason?: string }>(
      "run_generated_tests",
      { appKey: currentAppKey() },
    );
    const secs = ((performance.now() - t0) / 1000).toFixed(1);
    if (r.reason) {
      out.textContent = r.reason;
    } else {
      const ok = r.all_pass || r.failed === 0;
      out.className = `run-all-result ${ok ? "ok" : "fail"}`;
      out.textContent = `${ok ? "✓" : "✗"} ${r.passed} lulus · ${r.failed} gagal · ${r.total} total — ${secs}s`;
    }
  } catch (err) {
    out.className = "run-all-result fail";
    out.textContent = `Gagal: ${errStr(err)}`;
  } finally {
    btn.disabled = false;
    btn.textContent = was;
  }
}

/** "🔎 Triage tiket": classify + run every case in the open ticket, show the breakdown. */
async function triageTicket(): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const btn = $<HTMLButtonElement>("tc-triage");
  show($("triage-panel"), true);
  ($("triage-title") as HTMLElement).textContent = `Triage ${key}`;
  triageCases = [];
  renderTriage();
  btn.disabled = true;
  btn.textContent = "🔎 Triage jalan…";
  // Auto cases run for real (pass/fail) via widget tests; device selection is
  // vestigial (the retired on-device track) and not needed for widget tests.
  const serial = ($("devrun-device") as HTMLSelectElement | null)?.value || undefined;
  const appKey = ($("devrun-itest-app") as HTMLSelectElement | null)?.value || undefined;
  const email = ($("devrun-email") as HTMLInputElement | null)?.value.trim() || undefined;
  try {
    await invoke<TriageCase[]>("triage_ticket", { ticket: key, serial, appKey, email, fast: false });
    if (detailKey === key) await loadTestCases(key); // refresh row pills
  } catch (err) {
    toast(`Triage gagal: ${errStr(err)}`, "error");
  } finally {
    btn.disabled = false;
    btn.textContent = "🔎 Triage tiket";
  }
}

interface QaBranchStatus {
  repo: string;
  branch: string;
  behind: number;
  ahead: number;
  dirty: boolean;
  error: string;
}

/** Which app's repo the test-case actions target. The ticket's own [GTI]/[GTG]
 *  tag wins (a GTG ticket must hit the GTG repo regardless of the device-run
 *  selector); fall back to the selector, then GTI. */
function currentAppKey(): string {
  if (detailKey) {
    const m = ticketByKey(detailKey)?.summary.match(/\[(GTI|GTG)\]/i);
    if (m) return m[1].toUpperCase();
  }
  return ($("devrun-itest-app") as HTMLSelectElement | null)?.value || "GTI";
}

/** Refresh the QA-branch badge: shows current branch + how far behind develop. */
async function refreshQaBranchBadge(): Promise<void> {
  const badge = $("qa-branch-badge");
  try {
    const st = await invoke<QaBranchStatus>("qa_branch_status", { appKey: currentAppKey() });
    if (st.error) {
      badge.className = "qa-branch-badge warn";
      badge.textContent = `⚠ ${st.error}`;
    } else if (st.behind > 0) {
      badge.className = "qa-branch-badge warn";
      badge.textContent = `⤓ ${st.branch}: ${st.behind} commit di belakang develop${st.dirty ? " · tree kotor" : ""}`;
    } else {
      badge.className = "qa-branch-badge ok";
      badge.textContent = `✓ ${st.branch}: sinkron sama develop`;
    }
    show(badge, true);
  } catch {
    show(badge, false);
  }
}

/** "⤓ Sync dari develop": one-way merge of origin/develop into the QA branch. */
async function syncDevelop(): Promise<void> {
  const btn = $<HTMLButtonElement>("tc-sync-develop");
  btn.disabled = true;
  btn.textContent = "⤓ Sync jalan…";
  try {
    const msg = await invoke<string>("sync_qa_branch", { appKey: currentAppKey() });
    toast(msg);
  } catch (err) {
    toast(`Sync gagal: ${errStr(err)}`, "error");
  } finally {
    btn.disabled = false;
    btn.textContent = "⤓ Sync dari develop";
    void refreshQaBranchBadge();
  }
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
  if (autoBusy) return void toast("Sabar, tiket lagi disiapin otomatis…");
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
        <a class="pr-ref mono" href="${esc(pr.url)}" title="Buka PR di GitHub">#${pr.number} · ${esc(pr.repo)} ↗</a>
        <span class="${prStateClass(pr.state)}">${esc(pr.state)}</span>
        <button class="pr-remove" type="button" title="Hapus dari daftar">✕</button>
      </div>
      <span class="pr-title">${esc(pr.title)}</span>
      <div class="pr-item-actions">
        <button class="btn small primary pr-summarize" type="button">✨ Ringkas + apa yang dites</button>
        <button class="btn small pr-gen-tc" type="button">✨ Buat test case dari PR ini</button>
      </div>
      <div class="pr-review hidden"></div>
      <div class="pr-chat">
        <div class="pr-chat-log"></div>
        <div class="pr-chat-preview hidden"></div>
        <form class="pr-chat-form">
          <button class="pr-chat-attach" type="button" title="Lampirkan gambar">📎</button>
          <input class="pr-chat-file" type="file" accept="image/*" hidden />
          <input class="pr-chat-input" type="text" autocomplete="off"
            placeholder="Tanya AI soal PR ini… (boleh tempel/lampirkan gambar)" />
          <button class="btn small primary pr-chat-send" type="submit">Tanya</button>
        </form>
      </div>`;

    item.querySelector<HTMLAnchorElement>(".pr-ref")?.addEventListener("click", (e) => {
      e.preventDefault();
      if (pr.url) void openUrl(pr.url).catch(() => toast("Gagal buka link.", "error"));
    });

    const chatLog = item.querySelector<HTMLDivElement>(".pr-chat-log")!;
    const input = item.querySelector<HTMLInputElement>(".pr-chat-input")!;
    const fileInput = item.querySelector<HTMLInputElement>(".pr-chat-file")!;
    const preview = item.querySelector<HTMLDivElement>(".pr-chat-preview")!;
    const chatBox = item.querySelector<HTMLDivElement>(".pr-chat")!;
    const btn = item.querySelector<HTMLButtonElement>(".pr-summarize");
    const panel = item.querySelector<HTMLDivElement>(".pr-review");
    renderPrChat(pr, chatLog);
    void hydratePr(pr, chatLog, panel!);

    // Pending screenshots for the next question (cleared after sending).
    let pendingImages: string[] = [];
    const showPreview = (): void => {
      if (pendingImages.length === 0) {
        preview.innerHTML = "";
        show(preview, false);
        return;
      }
      preview.innerHTML = pendingImages
        .map(
          (src, i) =>
            `<span class="pr-chat-thumb"><img src="${src}" alt="lampiran" /><button type="button" class="pr-chat-preview-x" data-i="${i}" title="Hapus gambar">✕</button></span>`
        )
        .join("");
      show(preview, true);
      preview.querySelectorAll<HTMLButtonElement>(".pr-chat-preview-x").forEach((x) =>
        x.addEventListener("click", () => {
          pendingImages.splice(Number(x.dataset.i), 1);
          showPreview();
        })
      );
    };
    const addImageFile = (file: File | null | undefined): void => {
      if (!file || !file.type.startsWith("image/")) return;
      const reader = new FileReader();
      reader.onload = () => {
        pendingImages.push(String(reader.result));
        showPreview();
      };
      reader.readAsDataURL(file);
    };
    const addImageFiles = (files: FileList | null | undefined): void => {
      if (!files) return;
      for (const f of files) addImageFile(f);
    };

    item
      .querySelector<HTMLButtonElement>(".pr-chat-attach")
      ?.addEventListener("click", () => fileInput.click());
    fileInput.addEventListener("change", () => {
      addImageFiles(fileInput.files);
      fileInput.value = "";
    });
    input.addEventListener("paste", (e) => {
      const items = (e as ClipboardEvent).clipboardData?.items;
      if (!items) return;
      let took = false;
      for (const it of items) {
        if (it.type.startsWith("image/")) {
          addImageFile(it.getAsFile());
          took = true;
        }
      }
      if (took) e.preventDefault();
    });
    // Drag & drop image files onto the chat area.
    chatBox.addEventListener("dragover", (e) => {
      e.preventDefault();
      chatBox.classList.add("dragover");
    });
    chatBox.addEventListener("dragleave", () => chatBox.classList.remove("dragover"));
    chatBox.addEventListener("drop", (e) => {
      e.preventDefault();
      chatBox.classList.remove("dragover");
      addImageFiles((e as DragEvent).dataTransfer?.files);
    });

    item.querySelector<HTMLFormElement>(".pr-chat-form")?.addEventListener("submit", (e) => {
      e.preventDefault();
      const q = input.value.trim();
      if (!q && pendingImages.length === 0) return;
      const imgs = pendingImages;
      input.value = "";
      pendingImages = [];
      showPreview();
      void askPr(pr, q, imgs, chatLog);
    });

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
  if (autoBusy) return void toast("Sabar, tiket lagi disiapin otomatis…");
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
  if (autoBusy) return void toast("Sabar, tiket lagi disiapin otomatis…");
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
  if (autoBusy) return void toast("Sabar, tiket lagi disiapin otomatis…");
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
  const channel = new Channel<string>();
  let acc = "";
  let started = false;
  channel.onmessage = (chunk) => {
    if (!started) {
      started = true;
      panel.classList.remove("loading");
    }
    acc += chunk;
    panel.innerHTML = mdToHtml(acc);
  };
  try {
    const review = await invoke<string>("summarize_pr", {
      key,
      summary,
      repo: pr.repo,
      number: pr.number,
      onChunk: channel,
    });
    pr.summary = review;
    panel.classList.remove("loading");
    panel.innerHTML = mdToHtml(review);
    addCopyButton(panel, review);
  } catch (e) {
    panel.classList.add("hidden");
    toast(errStr(e), "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

/** Load a PR's persisted summary + chat from the DB and render them, unless the
 *  in-memory state is already populated (e.g. a chat happened this session). */
async function hydratePr(
  pr: PrRef,
  log: HTMLDivElement,
  panel: HTMLDivElement
): Promise<void> {
  try {
    const st = await invoke<{ summary: string | null; chat: ChatMsg[] }>("get_pr_state", {
      repo: pr.repo,
      number: pr.number,
    });
    if (st.summary && !pr.summary) {
      pr.summary = st.summary;
      panel.classList.remove("hidden", "loading");
      panel.innerHTML = mdToHtml(st.summary);
      addCopyButton(panel, st.summary);
    }
    if (st.chat.length > 0 && (pr.chat ?? []).length === 0) {
      pr.chat = st.chat;
      renderPrChat(pr, log);
    }
  } catch {
    /* DB read is best-effort; ignore */
  }
}

/** Render the follow-up Q&A log for one PR into its chat-log element. */
function renderPrChat(pr: PrRef, log: HTMLDivElement): void {
  log.innerHTML = "";
  for (const msg of pr.chat ?? []) {
    const row = document.createElement("div");
    row.className = `pr-chat-msg pr-chat-${msg.role}`;
    if (msg.role === "assistant") {
      row.innerHTML = mdToHtml(msg.content);
      addCopyButton(row, msg.content);
    } else {
      const imgs = (msg.images ?? [])
        .map((s) => `<img class="pr-chat-img" src="${s}" alt="lampiran" />`)
        .join("");
      const txt = msg.content ? `<span>${esc(msg.content)}</span>` : "";
      row.innerHTML = imgs + txt;
    }
    log.appendChild(row);
  }
  log.scrollTop = log.scrollHeight;
}

/** Ask a follow-up question about a PR (multi-turn, grounded in the diff).
 *  `images` are data: URL screenshots attached to this question. */
async function askPr(
  pr: PrRef,
  question: string,
  images: string[],
  log: HTMLDivElement
): Promise<void> {
  pr.chat = pr.chat ?? [];
  pr.chat.push({
    role: "user",
    content: question,
    images: images.length ? images : undefined,
  });
  renderPrChat(pr, log);
  await streamPrAnswer(pr, images, log);
}

/** Stream the AI's answer for the last question in `pr.chat`. On failure, shows
 *  an inline "Coba lagi" button that re-runs this same step. */
async function streamPrAnswer(
  pr: PrRef,
  images: string[],
  log: HTMLDivElement
): Promise<void> {
  if (!detailKey) return;
  const key = detailKey;
  const summary = ticketByKey(key)?.summary || "";

  const bubble = document.createElement("div");
  bubble.className = "pr-chat-msg pr-chat-assistant loading";
  bubble.textContent = "Lagi mikir…";
  log.appendChild(bubble);
  log.scrollTop = log.scrollHeight;

  const channel = new Channel<string>();
  let acc = "";
  channel.onmessage = (chunk) => {
    acc += chunk;
    bubble.classList.remove("loading");
    bubble.innerHTML = mdToHtml(acc);
    log.scrollTop = log.scrollHeight;
  };

  try {
    const answer = await invoke<string>("ask_pr", {
      key,
      summary,
      repo: pr.repo,
      number: pr.number,
      history: (pr.chat ?? []).map((m) => ({ role: m.role, content: m.content })),
      images,
      onChunk: channel,
    });
    pr.chat!.push({ role: "assistant", content: answer });
    renderPrChat(pr, log);
  } catch (e) {
    bubble.remove();
    const retry = document.createElement("div");
    retry.className = "pr-chat-msg pr-chat-assistant error";
    retry.innerHTML = `<span>${esc(errStr(e))}</span> `;
    const rbtn = document.createElement("button");
    rbtn.type = "button";
    rbtn.className = "pr-chat-retry";
    rbtn.textContent = "Coba lagi";
    rbtn.addEventListener("click", () => {
      retry.remove();
      void streamPrAnswer(pr, images, log);
    });
    retry.appendChild(rbtn);
    log.appendChild(retry);
    log.scrollTop = log.scrollHeight;
  }
}

/** "✨ Buat test case dari PR ini": draft cases from the PR diff, then switch
 *  to the Test Cases tab and reload. */
async function generateTestCasesFromPr(
  pr: PrRef,
  btn: HTMLButtonElement
): Promise<void> {
  if (autoBusy) return void toast("Sabar, tiket lagi disiapin otomatis…");
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
  "jira_sprint",
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

/** Seed the sprint select with just the saved id (so save round-trips before
 *  the live list loads). Empty = a single "(pilih sprint)" placeholder. */
function seedSprintDropdown(current: string): void {
  const sel = $("cfg-jira_sprint") as HTMLSelectElement;
  const opts = [`<option value=""${current ? "" : " selected"}>(pilih sprint)</option>`];
  if (current) opts.push(`<option value="${esc(current)}" selected>Sprint ${esc(current)}</option>`);
  sel.innerHTML = opts.join("");
}

/** Show the sprint picker row only when the sprint scope is "specific". */
function toggleSprintRow(): void {
  const scope = ($("cfg-jira_sprint_scope") as HTMLSelectElement).value;
  show($("jira-sprint-row"), scope === "specific");
}

/** Repopulate the sprint select from the project's board (active + future
 *  sprints). Keeps a saved sprint selectable even if it's not in the list. */
async function loadSprints(project: string, current: string): Promise<void> {
  const sel = $("cfg-jira_sprint") as HTMLSelectElement;
  const sprints = await invoke<JiraSprint[]>("list_jira_sprints", { project });
  const opts = [`<option value=""${current ? "" : " selected"}>(pilih sprint)</option>`];
  let matched = false;
  for (const s of sprints) {
    const id = String(s.id);
    const isSel = id === current;
    if (isSel) matched = true;
    const tag = s.state === "active" ? " · aktif" : "";
    opts.push(`<option value="${esc(id)}"${isSel ? " selected" : ""}>${esc(s.name)}${tag}</option>`);
  }
  if (current && !matched) {
    opts.push(`<option value="${esc(current)}" selected>Sprint ${esc(current)}</option>`);
  }
  sel.innerHTML = opts.join("");
}

async function openSettings(): Promise<void> {
  try {
    const cfg = await invoke<AppConfig>("get_config");
    for (const k of CONFIG_KEYS) {
      if (JIRA_DROPDOWN_KEYS.includes(k)) continue; // seeded via dedicated helpers
      ($(`cfg-${k}`) as HTMLInputElement).value = cfg[k] ?? "";
    }
    // Secrets are never returned by the backend; show a hint when one is saved
    // so the empty field doesn't read as "no token". Leaving it blank on save
    // keeps the stored token (only a typed value replaces it).
    const savedHint = (id: string, present: boolean | undefined): void => {
      const el = $(id) as HTMLInputElement;
      el.placeholder = present ? "•••••••• (tersimpan — isi untuk ganti)" : "••••••••";
    };
    savedHint("cfg-jira_token", cfg.has_jira_token);
    savedHint("cfg-github_token", cfg.has_github_token);
    savedHint("cfg-gemini_api_key", cfg.has_gemini_key);
    seedFieldDropdown(cfg.jira_story_point_field ?? "");
    seedProjectDropdown(cfg.jira_project ?? "");
    seedAssigneeDropdown(cfg.jira_assignee ?? "");
    seedSprintDropdown(cfg.jira_sprint ?? "");
    toggleSprintRow();
    $("jira-fields-hint").textContent = "";
  } catch (e) {
    toast(`Gagal muat pengaturan: ${errStr(e)}`, "error");
  }
  show($("settings-overlay"), true);

  // Auto-load the Jira dropdowns if creds are present, so the user doesn't have
  // to click "Muat dari Jira" every time they open Settings.
  try {
    const cfg = await invoke<AppConfig>("get_config");
    if (cfg.jira_base_url && cfg.jira_email && cfg.has_jira_token) {
      await loadFromJira();
    }
  } catch {
    /* seeded values remain; ignore */
  }
}

function closeSettings(): void {
  show($("settings-overlay"), false);
}

/** Native folder picker for a local Flutter repo path field. */
async function pickRepoPath(inputId: string, title: string): Promise<void> {
  try {
    const input = $(inputId) as HTMLInputElement;
    const picked = await openDialog({
      directory: true,
      multiple: false,
      title,
      defaultPath: input.value.trim() || undefined,
    });
    if (typeof picked === "string") {
      input.value = picked;
    }
  } catch (e) {
    toast(`Gagal buka folder picker: ${errStr(e)}`, "error");
  }
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
    aiLanguage = cfg.ai_language || "Indonesia";
    sprintScope = cfg.jira_sprint_scope ?? "";
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

    // --- Sprints (only when "Sprint tertentu" is the chosen scope) ---
    if (cfg.jira_sprint_scope === "specific") {
      await loadSprints(projSel.value, cfg.jira_sprint);
    }

    toast("Pilihan dari Jira dimuat.");
  } catch (e) {
    toast(`Gagal muat dari Jira: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.textContent = prev;
  }
}

// ---------------------------------------------------------------------------
// Ticket Builder
// ---------------------------------------------------------------------------

interface BuilderRow {
  source_ticket: string;
  title: string;
  pr_number: string;
  pr_url: string;
  assignee: string;
}
interface ParsedBlob {
  epic: string;
  app: string;
  rows: BuilderRow[];
}
interface StoryResult {
  title: string;
  key: string | null;
  url: string | null;
  error: string | null;
}

function openTicketBuilder(): void {
  ($("tb-blob") as HTMLTextAreaElement).value = "";
  $("tb-rows").innerHTML = "";
  show($("tb-table-wrap"), false);
  show($("tb-loading"), false);
  $("tb-results").innerHTML = "";
  show($("ticket-overlay"), true);
}

function closeTicketBuilder(): void {
  show($("ticket-overlay"), false);
}

/** Render the editable rows table from parsed data. */
function renderBuilderRows(rows: BuilderRow[]): void {
  const tbody = $("tb-rows");
  tbody.innerHTML = rows
    .map(
      (r) => `
      <tr data-pr-url="${esc(r.pr_url)}">
        <td><input type="checkbox" class="tb-pick" checked /></td>
        <td><input class="tb-c tb-src" value="${esc(r.source_ticket)}" placeholder="—" /></td>
        <td><input class="tb-c tb-title" value="${esc(r.title)}" /></td>
        <td><input class="tb-c tb-pr" value="${esc(r.pr_number)}" /></td>
        <td><input class="tb-c tb-asg" value="${esc(r.assignee)}" /></td>
      </tr>`
    )
    .join("");
  show($("tb-table-wrap"), rows.length > 0);
}

async function parseTicketBlob(): Promise<void> {
  const blob = ($("tb-blob") as HTMLTextAreaElement).value;
  if (!blob.trim()) {
    toast("Tempel daftar PR-nya dulu.", "error");
    return;
  }
  const btn = $<HTMLButtonElement>("tb-parse");
  const prev = btn.textContent;
  btn.disabled = true;
  btn.classList.add("busy");
  btn.textContent = "Parsing…";
  show($("tb-table-wrap"), false);
  show($("tb-loading"), true);
  try {
    const parsed = await invoke<ParsedBlob>("parse_ticket_blob", { blob });
    if (parsed.epic) ($("tb-epic") as HTMLInputElement).value = parsed.epic;
    if (parsed.app) ($("tb-app") as HTMLInputElement).value = parsed.app;
    renderBuilderRows(parsed.rows || []);
    $("tb-results").innerHTML = "";
    if (!parsed.rows || parsed.rows.length === 0) toast("AI nggak nemu baris PR.", "error");
  } catch (e) {
    toast(`Gagal parse: ${errStr(e)}`, "error");
  } finally {
    show($("tb-loading"), false);
    btn.disabled = false;
    btn.classList.remove("busy");
    btn.textContent = prev;
  }
}

/** Collect the checked rows from the table. */
function collectBuilderRows(): BuilderRow[] {
  return Array.from($("tb-rows").querySelectorAll<HTMLTableRowElement>("tr"))
    .filter((tr) => (tr.querySelector(".tb-pick") as HTMLInputElement)?.checked)
    .map((tr) => ({
      source_ticket: (tr.querySelector(".tb-src") as HTMLInputElement).value.trim(),
      title: (tr.querySelector(".tb-title") as HTMLInputElement).value.trim(),
      pr_number: (tr.querySelector(".tb-pr") as HTMLInputElement).value.trim(),
      pr_url: tr.dataset.prUrl || "",
      assignee: (tr.querySelector(".tb-asg") as HTMLInputElement).value.trim(),
    }));
}

async function createStoryTickets(): Promise<void> {
  const epic = ($("tb-epic") as HTMLInputElement).value.trim();
  const app = ($("tb-app") as HTMLInputElement).value.trim();
  const rows = collectBuilderRows();
  if (!epic) {
    toast("Isi Epic key dulu.", "error");
    return;
  }
  if (rows.length === 0) {
    toast("Centang minimal satu baris.", "error");
    return;
  }
  const btn = $<HTMLButtonElement>("tb-create");
  const prev = btn.textContent;
  btn.disabled = true;
  btn.classList.add("busy");
  btn.textContent = `Membuat ${rows.length}…`;
  try {
    const results = await invoke<StoryResult[]>("create_story_tickets", { epic, app, rows });
    const ok = results.filter((r) => r.key).length;
    $("tb-results").innerHTML =
      `<h3 class="bw-h">Hasil — ${ok}/${results.length} dibuat</h3>` +
      results
        .map((r) =>
          r.key
            ? `<div class="tb-res ok">✓ <a class="ext-link" data-url="${esc(r.url || "")}">${esc(r.key)}</a> — ${esc(r.title)}</div>`
            : `<div class="tb-res err">✗ ${esc(r.title)} — ${esc(r.error || "gagal")}</div>`
        )
        .join("");
    toast(`${ok}/${results.length} tiket dibuat.`);
    if (ok > 0) void refreshBoard();
  } catch (e) {
    toast(`Gagal buat tiket: ${errStr(e)}`, "error");
  } finally {
    btn.disabled = false;
    btn.classList.remove("busy");
    btn.textContent = prev;
  }
}

function wireTicketBuilder(): void {
  $("ticket-btn").addEventListener("click", openTicketBuilder);
  $("tb-close").addEventListener("click", closeTicketBuilder);
  $("ticket-overlay").addEventListener("click", (e) => {
    if (e.target === $("ticket-overlay")) closeTicketBuilder();
  });
  $("tb-parse").addEventListener("click", () => void parseTicketBlob());
  $("tb-create").addEventListener("click", () => void createStoryTickets());
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

  // Global keyboard shortcuts: Esc closes the top open modal; Cmd/Ctrl+F
  // focuses the board search.
  document.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") {
      e.preventDefault();
      const s = $<HTMLInputElement>("board-search");
      s.focus();
      s.select();
      return;
    }
    if (e.key === "Escape") {
      const overlays: Array<[string, () => void]> = [
        ["palette-overlay", closePalette],
        ["annotate-overlay", cancelAnnotator],
        ["bugwriter-overlay", closeBugWriter],
        ["ticket-overlay", closeTicketBuilder],
        ["summary-overlay", closeSummary],
        ["settings-overlay", closeSettings],
        ["detail-overlay", closeDetail],
        ["transition-overlay", closeTransitionPicker],
      ];
      for (const [id, close] of overlays) {
        if (!$(id).classList.contains("hidden")) {
          close();
          return;
        }
      }
    }
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
  $("cfg-gti_path-pick").addEventListener("click", () =>
    void pickRepoPath("cfg-gti_path", "Pilih folder repo GTI (gotradeindoapp)")
  );
  $("cfg-gtg_path-pick").addEventListener("click", () =>
    void pickRepoPath("cfg-gtg_path", "Pilih folder repo GTG (tradecharlieflutter)")
  );
  $("update-check").addEventListener("click", () => void checkForUpdate(true));
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
  // Sprints belong to a project's board too, so refresh them when the picker is
  // visible (scope = "specific").
  $("cfg-jira_project").addEventListener("change", () => {
    const project = ($("cfg-jira_project") as HTMLSelectElement).value;
    const current = ($("cfg-jira_assignee") as HTMLSelectElement).value;
    void loadAssignees(project, current).catch((e) =>
      toast(`Gagal muat assignee: ${errStr(e)}`, "error")
    );
    if (($("cfg-jira_sprint_scope") as HTMLSelectElement).value === "specific") {
      const sprint = ($("cfg-jira_sprint") as HTMLSelectElement).value;
      void loadSprints(project, sprint).catch((e) =>
        toast(`Gagal muat sprint: ${errStr(e)}`, "error")
      );
    }
  });

  // Toggling the sprint scope shows/hides the sprint picker; switching to
  // "specific" lazily loads the project's sprints.
  $("cfg-jira_sprint_scope").addEventListener("change", () => {
    toggleSprintRow();
    if (($("cfg-jira_sprint_scope") as HTMLSelectElement).value !== "specific") return;
    const project = ($("cfg-jira_project") as HTMLSelectElement).value;
    const sprint = ($("cfg-jira_sprint") as HTMLSelectElement).value;
    void loadSprints(project, sprint).catch((e) =>
      toast(`Gagal muat sprint: ${errStr(e)}`, "error")
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
  $("detail-key").addEventListener("click", () => {
    if (!detailKey || !jiraBrowseBase) return;
    const url = `${jiraBrowseBase}/browse/${detailKey}`;
    void openUrl(url).catch(() => toast("Gagal buka link Jira.", "error"));
  });
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
  $("tc-triage").addEventListener("click", () => void triageTicket());
  $("tc-gen-all").addEventListener("click", () => void generateAllBuildable());
  $("tc-run-all").addEventListener("click", () => void runAllGenerated());
  $("tc-sync-develop").addEventListener("click", () => void syncDevelop());
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
  wireAnnotator();
  wireDeviceRun();
  wirePalette(paletteCommands);
  $("palette-btn").addEventListener("click", openPalette);
  wireSummary();
  wireTicketBuilder();
}

/** Check GitHub for a newer release; if found, offer to download + install +
 *  restart. On the silent startup check (`manual=false`) offline / no-manifest
 *  errors are swallowed; the manual "Cek update" button (`manual=true`) reports
 *  "already up to date" and surfaces errors as toasts. */
async function checkForUpdate(manual = false): Promise<void> {
  try {
    if (manual) toast("Mengecek update…");
    const update = await check();
    if (!update) {
      if (manual) toast("Kamu sudah pakai versi terbaru. 🎉");
      return;
    }
    const ok = await confirmDialog(
      `Ada versi baru QA Cockpit (v${update.version}). Update sekarang?`
    );
    if (!ok) return;
    toast("Mengunduh update…");
    await update.downloadAndInstall();
    toast("Update selesai — merestart aplikasi…");
    await relaunch();
  } catch (e) {
    if (manual) toast(`Gagal cek update: ${errStr(e)}`, "error");
    else console.error("Update check failed:", e);
  }
}

async function init(): Promise<void> {
  initTheme();
  wireEvents();
  try {
    const cfg = await invoke<AppConfig>("get_config");
    aiLanguage = cfg.ai_language || "Indonesia";
    sprintScope = cfg.jira_sprint_scope ?? "";
  } catch {
    /* keep defaults */
  }
  await refreshBoard();
  getVersion()
    .then((v) => ($("app-version").textContent = `v${v}`))
    .catch(() => ($("app-version").textContent = ""));
  void checkForUpdate();
}

window.addEventListener("DOMContentLoaded", () => {
  void init();
});
