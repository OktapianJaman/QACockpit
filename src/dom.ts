// ---------------------------------------------------------------------------
// DOM helpers, theme, toast & errors — shared UI utilities.
// ---------------------------------------------------------------------------

import { THEME_KEY } from "./constants";

export function $<T extends HTMLElement = HTMLElement>(id: string): T {
  const el = document.getElementById(id);
  if (!el) throw new Error(`element #${id} tidak ditemukan`);
  return el as T;
}

export function show(el: HTMLElement, visible: boolean): void {
  el.classList.toggle("hidden", !visible);
}

// --- Theme (light / dark), persisted in localStorage ---
type Theme = "dark" | "light";

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

export function initTheme(): void {
  const saved = (localStorage.getItem(THEME_KEY) as Theme | null) ?? "dark";
  applyTheme(saved);
}

export function toggleTheme(): void {
  const next: Theme = currentTheme() === "light" ? "dark" : "light";
  localStorage.setItem(THEME_KEY, next);
  applyTheme(next);
}

// --- Toast / errors ---
let toastTimer: number | undefined;

export function toast(msg: string, kind: "info" | "error" = "info"): void {
  const el = $("toast");
  el.textContent = msg;
  el.classList.remove("error", "info");
  el.classList.add(kind);
  show(el, true);
  if (toastTimer) window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => show(el, false), kind === "error" ? 6000 : 3500);
}

export function errStr(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

/** Append a small "Copy" button to `container` that copies `text` to the
 *  clipboard. `container` should be position:relative (the button is absolute). */
export function addCopyButton(container: HTMLElement, text: string): void {
  const btn = document.createElement("button");
  btn.type = "button";
  btn.className = "copy-btn";
  btn.textContent = "Copy";
  btn.title = "Salin teks";
  btn.addEventListener("click", () => {
    void navigator.clipboard.writeText(text).then(
      () => {
        btn.textContent = "Tersalin ✓";
        window.setTimeout(() => (btn.textContent = "Copy"), 1500);
      },
      () => toast("Gagal menyalin.", "error")
    );
  });
  container.appendChild(btn);
}
