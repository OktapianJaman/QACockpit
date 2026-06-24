import { describe, it, expect } from "vitest";
import { esc, mdInline, mdToHtml } from "./markdown";

describe("esc", () => {
  it("escapes HTML-significant characters", () => {
    expect(esc(`<a href="x">&</a>`)).toBe("&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;");
  });
  it("leaves plain text untouched", () => {
    expect(esc("hello world")).toBe("hello world");
  });
});

describe("mdInline", () => {
  it("renders code, bold, italic", () => {
    expect(mdInline("`x` **b** *i*")).toBe("<code>x</code> <strong>b</strong> <em>i</em>");
  });
  it("leaves unmatched markers alone", () => {
    expect(mdInline("a * b")).toBe("a * b");
  });
});

describe("mdToHtml", () => {
  it("escapes before formatting (no raw HTML injection)", () => {
    expect(mdToHtml("<script>alert(1)</script>")).toBe(
      "<p>&lt;script&gt;alert(1)&lt;/script&gt;</p>"
    );
  });

  it("renders headings with shifted levels", () => {
    expect(mdToHtml("# Title")).toBe('<h3 class="md-h">Title</h3>');
    expect(mdToHtml("## Sub")).toBe('<h4 class="md-h">Sub</h4>');
  });

  it("caps heading level at h6", () => {
    expect(mdToHtml("##### Deep")).toBe('<h6 class="md-h">Deep</h6>');
    expect(mdToHtml("###### Deeper")).toBe('<h6 class="md-h">Deeper</h6>');
  });

  it("renders ordered and unordered lists", () => {
    expect(mdToHtml("- a\n- b")).toBe("<ul><li>a</li><li>b</li></ul>");
    expect(mdToHtml("1. a\n2. b")).toBe("<ol><li>a</li><li>b</li></ol>");
  });

  it("closes a list when switching to a paragraph", () => {
    expect(mdToHtml("- a\n\ntext")).toBe("<ul><li>a</li></ul><p>text</p>");
  });

  it("renders horizontal rules", () => {
    expect(mdToHtml("---")).toBe("<hr>");
  });

  it("applies inline formatting inside list items and paragraphs", () => {
    expect(mdToHtml("- **bold**")).toBe("<ul><li><strong>bold</strong></li></ul>");
  });
});
