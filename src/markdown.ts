// ---------------------------------------------------------------------------
// Safe markdown → HTML rendering for AI output. Pure functions (no DOM).
// ---------------------------------------------------------------------------

/** Escape text destined for innerHTML interpolation. */
export function esc(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** Inline markdown (on already-escaped text): `code`, **bold**, *italic*. */
export function mdInline(s: string): string {
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
export function mdToHtml(src: string): string {
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
