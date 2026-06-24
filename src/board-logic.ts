// ---------------------------------------------------------------------------
// Pure board column logic (status ranking + column ordering). No DOM.
// ---------------------------------------------------------------------------

import { STATUS_ORDER, DONE_KEYWORDS, ALWAYS_COLUMNS } from "./constants";
import type { BoardTicket } from "./types";

/** Rank a status by the preferred order; unmatched statuses rank last. */
export function statusRank(status: string): number {
  const s = status.toLowerCase();
  for (let i = 0; i < STATUS_ORDER.length; i++) {
    const kw = STATUS_ORDER[i];
    if (s === kw || s.includes(kw)) return i;
  }
  return STATUS_ORDER.length;
}

/** Map a raw Jira status to its display column — terminal ones → "Done". */
export function displayColumn(status: string): string {
  const s = status.toLowerCase();
  return DONE_KEYWORDS.some((k) => s.includes(k)) ? "Done" : status;
}

/** DISPLAY columns to render: those present + the always-on QA columns, deduped
 *  case-insensitively, ordered by preferred sequence then alpha. */
export function orderedColumns(tickets: BoardTicket[]): string[] {
  const cols = new Set(tickets.map((t) => displayColumn(t.status)).filter(Boolean));
  const lower = new Set([...cols].map((c) => c.toLowerCase()));
  for (const c of ALWAYS_COLUMNS) {
    if (!lower.has(c.toLowerCase())) cols.add(c);
  }
  return [...cols].sort((a, b) => {
    const r = statusRank(a) - statusRank(b);
    return r !== 0 ? r : a.localeCompare(b);
  });
}
