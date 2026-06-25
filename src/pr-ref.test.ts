import { describe, it, expect } from "vitest";
import { parsePrRefsFromSummary } from "./pr-ref";

describe("parsePrRefsFromSummary", () => {
  it("parses the [GTG] tag + #number from a real summary", () => {
    const s = "[UAT] [GTG] feat(kyc): OCR auto-fill — multi-country scan, review screen + flow #3182";
    expect(parsePrRefsFromSummary(s)).toEqual([
      { repo: "tr8team/tradecharlieflutter", number: 3182 },
    ]);
  });

  it("maps [GTI] to the gotradeindoapp repo", () => {
    expect(parsePrRefsFromSummary("[GTI] fix login #42")).toEqual([
      { repo: "tr8team/gotradeindoapp", number: 42 },
    ]);
  });

  it("ignores non-repo bracket tags like [UAT] and is case-insensitive", () => {
    expect(parsePrRefsFromSummary("[uat] [gtg] tweak #7")).toEqual([
      { repo: "tr8team/tradecharlieflutter", number: 7 },
    ]);
  });

  it("collects multiple PR numbers under the one repo tag, deduped", () => {
    expect(parsePrRefsFromSummary("[GTG] big feature #10 and #12 (re #10)")).toEqual([
      { repo: "tr8team/tradecharlieflutter", number: 10 },
      { repo: "tr8team/tradecharlieflutter", number: 12 },
    ]);
  });

  it("returns [] when there is no repo tag", () => {
    expect(parsePrRefsFromSummary("feat: something #3182")).toEqual([]);
  });

  it("returns [] when there is no PR number", () => {
    expect(parsePrRefsFromSummary("[GTG] feat: something")).toEqual([]);
  });

  it("returns [] for an empty summary", () => {
    expect(parsePrRefsFromSummary("")).toEqual([]);
  });
});
