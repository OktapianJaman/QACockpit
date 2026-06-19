# QA Cockpit — Pivot to All-in-One QA Workspace

**Date:** 2026-06-19
**Status:** Direction agreed; sequenced build pending
**Supersedes:** the screen-recording + point-fairness model (retired)

## Why pivot

The background screen recorder was the weak link: idle-detection bugs, a messy
timeline, tiny "hours×2" point values, tedious manual tagging, and macOS
permission friction — high effort, low payoff. A QA's value isn't measured in
time logs; the valuable artifacts are **test cases, bug evidence, ticket flow,
and PR understanding**. Also: the user's QA Jira tickets carry no story points,
so the fairness comparison had no data.

## New identity

From a **passive time tracker** → a **QA command center**: one place a QA runs
their testing work end to end.

## Unifying structure

- **Kanban board = home.** Columns = QA statuses (e.g. Ready for QA → QA In
  Progress → QA Passed / QA Failed). Cards = assigned tickets.
- **Each card opens a ticket detail view** with tabs:
  - **Test Cases** — write / store / run (pass-fail) test cases; AI-generate from the ticket.
  - **PR** — fetch the ticket's PR(s); AI summary of the diff + "what to test".
  - **Evidence** — on-demand screenshot → annotate → attach to the ticket/bug.

## The four pillars (user said "semua")

1. **Kanban + drag-drop + set points** — visual ticket flow; drag to transition
   status (reuses the Jira transitions API already built); edit points inline.
2. **Test-case management** — per-ticket test cases in local SQLite, run/track
   pass-fail; AI (Gemma) generates draft cases from the ticket summary/description.
3. **PR summary on-demand** — for a ticket's PR, AI reads the diff and outputs a
   summary + risk/what-to-test. No continuous polling.
4. **Bug evidence** — capture a screenshot on demand, optionally annotate, attach
   to a ticket. (Repurposes the "camera" from creepy background recording to
   useful on-demand evidence.)

## Keep vs drop

- **Keep:** Jira sync (ticket pull, filters), Jira status transitions, Gemma
  local AI client, Tauri shell, SQLite, settings/config.
- **Drop / retire:** background recorder, idle detection, timeline,
  `activity_blocks`, `ticket_time`, hours×2 point model, the daily
  earned-points header.

## Build sequence (one spine first)

1. **Kanban foundation** — board from synced tickets, columns by status,
   drag-drop → transition (with confirm), inline point edit. Strip the recorder/
   timeline/points UI. This is the new home.
2. **Ticket detail + Test Cases** — detail panel; test-case CRUD in SQLite;
   AI-generate from ticket.
3. **PR tab** — fetch ticket PR + AI summary / what-to-test.
4. **Evidence tab** — on-demand screenshot + attach.

## Open questions

- Screenshot capture mechanism on macOS from Tauri (screencapture CLI vs API);
  annotation scope for v1 (maybe none — just capture + attach).
- How a ticket maps to its PR(s) (branch name contains the key? PR title? manual link?).
- Test-case storage shape (steps, expected, actual, status, run history).
- Do we still need GitHub at all, or only fetch a PR when the user opens the PR tab?
