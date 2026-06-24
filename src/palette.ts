// ---------------------------------------------------------------------------
// Command palette (⌘K / Ctrl-K) — type an intent, run it. A generic UI: the
// caller supplies the command list via wirePalette(getCommands). The palette
// fuzzy-filters by the typed query (all whitespace-separated tokens must appear
// in the command's title/subtitle) and runs the selected command on Enter.
// ---------------------------------------------------------------------------

import { $, show } from "./dom";
import { esc } from "./markdown";

export interface PaletteCommand {
  /** Primary label, also the main match target. */
  title: string;
  /** Dim secondary line (e.g. a ticket summary); also matched. */
  subtitle?: string;
  run: () => void | Promise<void>;
}

let getCommands: () => PaletteCommand[] = () => [];
let results: PaletteCommand[] = [];
let selected = 0;

/** Every whitespace token of the query must be a substring of the haystack. */
function matches(query: string, haystack: string): boolean {
  const h = haystack.toLowerCase();
  return query
    .toLowerCase()
    .split(/\s+/)
    .filter(Boolean)
    .every((tok) => h.includes(tok));
}

function render(): void {
  const list = $("palette-list");
  if (results.length === 0) {
    list.innerHTML = `<li class="palette-empty">Nggak ada yang cocok</li>`;
    return;
  }
  list.innerHTML = results
    .map(
      (c, i) =>
        `<li class="palette-item${i === selected ? " active" : ""}" data-idx="${i}">` +
        `<span class="palette-title">${esc(c.title)}</span>` +
        (c.subtitle ? `<span class="palette-sub">${esc(c.subtitle)}</span>` : "") +
        `</li>`
    )
    .join("");
  list.querySelector(".palette-item.active")?.scrollIntoView({ block: "nearest" });
}

function update(): void {
  const q = ($("palette-input") as HTMLInputElement).value.trim();
  const all = getCommands();
  results = (q === "" ? all : all.filter((c) => matches(q, `${c.title} ${c.subtitle ?? ""}`))).slice(
    0,
    50
  );
  selected = 0;
  render();
}

export function openPalette(): void {
  const input = $("palette-input") as HTMLInputElement;
  input.value = "";
  update();
  show($("palette-overlay"), true);
  input.focus();
}

export function closePalette(): void {
  show($("palette-overlay"), false);
}

function runSelected(i: number): void {
  const cmd = results[i];
  if (!cmd) return;
  closePalette();
  void cmd.run();
}

/** Wire the palette once (call from app init). `commands` is re-read on every
 *  open/keystroke so it always reflects current board state. */
export function wirePalette(commands: () => PaletteCommand[]): void {
  getCommands = commands;
  const input = $("palette-input") as HTMLInputElement;

  // Global ⌘K / Ctrl-K toggles the palette.
  document.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
      e.preventDefault();
      if ($("palette-overlay").classList.contains("hidden")) openPalette();
      else closePalette();
    }
  });

  input.addEventListener("input", update);
  input.addEventListener("keydown", (e) => {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      selected = Math.min(selected + 1, results.length - 1);
      render();
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      selected = Math.max(selected - 1, 0);
      render();
    } else if (e.key === "Enter") {
      e.preventDefault();
      runSelected(selected);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closePalette();
    }
  });

  $("palette-list").addEventListener("click", (e) => {
    const li = (e.target as HTMLElement).closest(".palette-item") as HTMLElement | null;
    if (li?.dataset.idx !== undefined) runSelected(Number(li.dataset.idx));
  });
  $("palette-overlay").addEventListener("click", (e) => {
    if (e.target === $("palette-overlay")) closePalette();
  });
}
