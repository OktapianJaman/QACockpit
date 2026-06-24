// ---------------------------------------------------------------------------
// Bug Writer — compose a bug report (optionally AI-assisted) from a description
// + screenshot, then file it as a Jira issue.
// ---------------------------------------------------------------------------

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { $, show, toast, errStr } from "./dom";
import { esc } from "./markdown";
import type { AppConfig, JiraProject, JiraUser } from "./types";

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

export function wireBugWriter(): void {
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
