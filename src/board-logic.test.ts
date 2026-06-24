import { describe, it, expect } from "vitest";
import { statusRank, displayColumn, orderedColumns } from "./board-logic";
import type { BoardTicket } from "./types";

const ticket = (status: string): BoardTicket => ({
  key: "X-1",
  summary: "s",
  status,
  story_points: null,
});

describe("statusRank", () => {
  it("ranks by preferred order", () => {
    expect(statusRank("To Do")).toBeLessThan(statusRank("Done"));
    expect(statusRank("Ready for QA")).toBeLessThan(statusRank("QA In Progress"));
  });

  it("prefers the more specific keyword (qa in progress before in progress)", () => {
    expect(statusRank("QA In Progress")).toBeLessThan(statusRank("In Progress"));
  });

  it("ranks unknown statuses last", () => {
    expect(statusRank("Whatever")).toBe(statusRank("also unknown"));
    expect(statusRank("Whatever")).toBeGreaterThan(statusRank("Done"));
  });
});

describe("displayColumn", () => {
  it("collapses terminal statuses into Done", () => {
    expect(displayColumn("QA Passed")).toBe("Done");
    expect(displayColumn("Closed")).toBe("Done");
    expect(displayColumn("Selesai")).toBe("Done");
  });

  it("keeps non-terminal statuses as-is", () => {
    expect(displayColumn("In Progress")).toBe("In Progress");
    expect(displayColumn("Ready for QA")).toBe("Ready for QA");
  });
});

describe("orderedColumns", () => {
  it("always includes the canonical QA columns even with no tickets", () => {
    const cols = orderedColumns([]);
    expect(cols).toEqual(expect.arrayContaining(["Ready for QA", "Today", "QA In Progress", "Done"]));
  });

  it("dedupes canonical columns case-insensitively", () => {
    const cols = orderedColumns([ticket("ready for qa")]);
    const readyVariants = cols.filter((c) => c.toLowerCase() === "ready for qa");
    expect(readyVariants).toHaveLength(1);
  });

  it("orders columns left-to-right by preferred sequence", () => {
    const cols = orderedColumns([ticket("In Progress"), ticket("To Do")]);
    expect(cols.indexOf("To Do")).toBeLessThan(cols.indexOf("In Progress"));
  });
});
