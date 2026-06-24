// ---------------------------------------------------------------------------
// Formatting helpers. Pure functions (no DOM).
// ---------------------------------------------------------------------------

/** Round a points value to 1 decimal place, dropping a trailing .0. */
export function fmtPoints(n: number): string {
  const r = Math.round(n * 10) / 10;
  return Number.isInteger(r) ? String(r) : r.toFixed(1);
}

/** Human label for a (possibly missing) story-point value. */
export function pointsLabel(pts: number | null): string {
  return pts == null ? "— pts" : `${fmtPoints(pts)} pts`;
}
