// ---------------------------------------------------------------------------
// Bug Writer — compose a bug report (optionally AI-assisted) from a description
// + screenshot, then file it as a Jira issue.
// ---------------------------------------------------------------------------

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { $, show, toast, errStr } from "./dom";
import { esc } from "./markdown";
import type { AppConfig, JiraProject, JiraUser } from "./types";

// Attached screenshots as data URLs (empty = none). Multiple are supported:
// all are attached to the Jira issue and sent to the AI for context.
let bwImages: string[] = [];

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

/** Re-render the thumbnail strip from `bwImages`. Each thumb has a ✕ remover. */
function renderBwThumbs(): void {
  const wrap = $("bw-thumbs");
  wrap.innerHTML = "";
  for (let i = 0; i < bwImages.length; i++) {
    const thumb = document.createElement("div");
    thumb.className = "bw-thumb";
    const img = document.createElement("img");
    img.src = bwImages[i];
    img.alt = `screenshot ${i + 1}`;
    const rm = document.createElement("button");
    rm.type = "button";
    rm.className = "bw-thumb-rm";
    rm.title = "Hapus gambar ini";
    rm.textContent = "✕";
    rm.dataset.idx = String(i);
    thumb.appendChild(img);
    thumb.appendChild(rm);
    wrap.appendChild(thumb);
  }
  show(wrap, bwImages.length > 0);
}

function clearBwImages(): void {
  bwImages = [];
  ($("bw-file") as HTMLInputElement).value = "";
  renderBwThumbs();
}

function removeBwImage(index: number): void {
  bwImages.splice(index, 1);
  renderBwThumbs();
}

/** Accept every image in a File list / array, appending to the strip. */
async function acceptBwImagesFrom(files: FileList | File[] | null): Promise<void> {
  const imgs = files ? Array.from(files).filter((f) => f.type.startsWith("image/")) : [];
  if (imgs.length === 0) return;
  for (const file of imgs) {
    try {
      bwImages.push(await fileToDataUrl(file));
    } catch (e) {
      toast(`Gagal baca gambar: ${errStr(e)}`, "error");
    }
  }
  renderBwThumbs();
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
  clearBwImages();
  show($("bw-result"), false);
  show($("bugwriter-overlay"), true);
  // Default the output language to the global AI language (still overridable here).
  void invoke<AppConfig>("get_config")
    .then((cfg) => {
      if (cfg.ai_language) ($("bw-language") as HTMLSelectElement).value = cfg.ai_language;
    })
    .catch(() => {});
  void populateBwProjects();
}

export function closeBugWriter(): void {
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
  if (!text.trim() && bwImages.length === 0) {
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
      images: bwImages,
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
      images: bwImages,
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

export function wireBugWriter(): void {
  $("bugwriter-btn").addEventListener("click", openBugWriter);
  $("bw-close").addEventListener("click", closeBugWriter);
  $("bugwriter-overlay").addEventListener("click", (e) => {
    if (e.target === $("bugwriter-overlay")) closeBugWriter();
  });

  // Screenshot: click to pick, drag-drop, or paste — all append to the strip.
  const drop = $("bw-drop");
  drop.addEventListener("click", () => ($("bw-file") as HTMLInputElement).click());
  $("bw-file").addEventListener("change", (e) =>
    void acceptBwImagesFrom((e.target as HTMLInputElement).files)
  );
  // Remove a single thumbnail (event-delegated on its ✕ button).
  $("bw-thumbs").addEventListener("click", (e) => {
    const btn = (e.target as HTMLElement).closest(".bw-thumb-rm") as HTMLElement | null;
    if (btn?.dataset.idx) removeBwImage(Number(btn.dataset.idx));
  });
  drop.addEventListener("dragover", (e) => {
    e.preventDefault();
    drop.classList.add("bw-drag");
  });
  drop.addEventListener("dragleave", () => drop.classList.remove("bw-drag"));
  drop.addEventListener("drop", (e) => {
    e.preventDefault();
    drop.classList.remove("bw-drag");
    void acceptBwImagesFrom((e as DragEvent).dataTransfer?.files ?? null);
  });
  // Paste anywhere while the overlay is open — accepts multiple pasted images.
  $("bugwriter-overlay").addEventListener("paste", (e) => {
    const items = (e as ClipboardEvent).clipboardData?.items;
    if (!items) return;
    const files: File[] = [];
    for (const it of Array.from(items)) {
      if (it.kind === "file" && it.type.startsWith("image/")) {
        const file = it.getAsFile();
        if (file) files.push(file);
      }
    }
    if (files.length > 0) {
      e.preventDefault();
      void acceptBwImagesFrom(files);
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
