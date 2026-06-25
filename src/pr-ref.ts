// ---------------------------------------------------------------------------
// Resolve the PR(s) a Jira ticket refers to from its summary. The team's
// convention encodes the repo as a bracket tag (`[GTI]`/`[GTG]`) and the PR
// number(s) as `#NNNN`, e.g. "[UAT] [GTG] feat(kyc): OCR auto-fill … #3182".
// Pure + DOM-free so it can be unit-tested.
// ---------------------------------------------------------------------------

import { REPO_TAGS } from "./constants";

export interface ParsedPrRef {
  repo: string;
  number: number;
}

/** Parse a ticket summary into the PR(s) it references. Returns `[]` when no
 *  repo tag or no `#number` is present (i.e. the ticket follows no convention).
 *  All `#numbers` are attributed to the single repo tag found, deduped. */
export function parsePrRefsFromSummary(summary: string): ParsedPrRef[] {
  if (!summary) return [];

  // First bracketed tag that maps to a known repo (ignores e.g. [UAT]).
  let repo = "";
  for (const m of summary.matchAll(/\[([A-Za-z]+)\]/g)) {
    const tag = m[1].toUpperCase();
    if (REPO_TAGS[tag]) {
      repo = REPO_TAGS[tag];
      break;
    }
  }
  if (!repo) return [];

  const seen = new Set<number>();
  const refs: ParsedPrRef[] = [];
  for (const m of summary.matchAll(/#(\d+)/g)) {
    const number = Number(m[1]);
    if (number > 0 && !seen.has(number)) {
      seen.add(number);
      refs.push({ repo, number });
    }
  }
  return refs;
}
