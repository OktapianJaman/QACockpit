import { describe, it, expect } from "vitest";
import { fmtPoints, pointsLabel } from "./format";

describe("fmtPoints", () => {
  it("drops a trailing .0 for whole numbers", () => {
    expect(fmtPoints(3)).toBe("3");
    expect(fmtPoints(3.0)).toBe("3");
  });
  it("rounds to one decimal place", () => {
    expect(fmtPoints(2.45)).toBe("2.5");
    expect(fmtPoints(2.44)).toBe("2.4");
  });
});

describe("pointsLabel", () => {
  it("shows an em-dash for missing points", () => {
    expect(pointsLabel(null)).toBe("— pts");
  });
  it("formats present points", () => {
    expect(pointsLabel(5)).toBe("5 pts");
    expect(pointsLabel(1.5)).toBe("1.5 pts");
  });
});
