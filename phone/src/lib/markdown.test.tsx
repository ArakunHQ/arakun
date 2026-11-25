import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Markdown } from "./markdown";

// Expected markup comes from md() in public/app.js.
const html = (src: string) => render(<Markdown src={src} />).container.innerHTML;

describe("Markdown", () => {
  it("renders headings, capped at h3", () => {
    expect(html("# Title\n\n## Sub\n\n#### Deep")).toBe("<h1>Title</h1><h2>Sub</h2><h3>Deep</h3>");
  });
  it("renders unordered and ordered lists", () => {
    expect(html("- a\n- b **bold**\n\n1. one\n2) two")).toBe(
      "<ul><li>a</li><li>b <b>bold</b></li></ul><ol><li>one</li><li>two</li></ol>",
    );
  });
  it("renders a fenced block with a language as plain code", () => {
    expect(html("```rust\nfn main() {}\n```")).toBe("<pre><code>fn main() {}</code></pre>");
  });
  it("renders inline code, italics and bold", () => {
    expect(html("use `x < y` and _it_ and *em* and **b**")).toBe(
      "<p>use <code>x &lt; y</code> and <i>it</i> and <i>em</i> and <b>b</b></p>",
    );
  });
  it("opens links in a new tab with rel noopener", () => {
    expect(html("see [site](https://example.com/a?b=1&c=2) now")).toBe(
      '<p>see <a href="https://example.com/a?b=1&amp;c=2" target="_blank" rel="noopener">site</a> now</p>',
    );
  });
  it("does not make a link of a javascript: url", () => {
    const { container } = render(<Markdown src="[x](javascript:alert(1))" />);
    expect(container.querySelector("a")).toBeNull();
  });
  it("renders quotes and keeps line breaks", () => {
    expect(html("> quote\n> more")).toBe("<blockquote><p>quote<br>more</p></blockquote>");
  });
  it("renders a table with a bar and right-aligned figures", () => {
    const { container } = render(<Markdown src={"| A | B |\n|---|---|\n| x | 12% |\n| y | 3 |"} />);
    expect(container.querySelector(".tbar span")?.textContent).toBe("2 rows · 2 columns");
    expect([...container.querySelectorAll("th")].map((c) => c.textContent)).toEqual(["A", "B"]);
    const cells = [...container.querySelectorAll("td")];
    expect(cells.map((c) => c.textContent)).toEqual(["x", "12%", "y", "3"]);
    expect(cells.map((c) => c.className)).toEqual(["", "num", "", "num"]);
    expect(container.querySelector("button.tablex")?.getAttribute("aria-label")).toBe("Open the table");
  });
  it("gives long table cells room", () => {
    const { container } = render(<Markdown src={`| A |\n|---|\n| ${"w".repeat(30)} |`} />);
    expect(container.querySelector("td")?.className).toBe("long");
  });
  it("calls onTable with the table when Open is pressed", () => {
    const onTable = vi.fn();
    const { container } = render(<Markdown src={"| A |\n|---|\n| x |"} onTable={onTable} />);
    fireEvent.click(container.querySelector("button.tablex")!);
    expect(onTable).toHaveBeenCalledWith(container.querySelector("table"));
  });
  it("renders an image link as a picture and reports a tap", () => {
    const onPic = vi.fn();
    const { container } = render(<Markdown src="![shot](/image/abc?x=1&y=2) and ![d](https://e.com/p.png)" onPic={onPic} />);
    const imgs = container.querySelectorAll("img.pic");
    expect(imgs).toHaveLength(2);
    expect(imgs[0].getAttribute("src")).toBe("/image/abc?x=1&y=2");
    expect(imgs[0].getAttribute("alt")).toBe("shot");
    expect(imgs[0].getAttribute("loading")).toBe("lazy");
    fireEvent.click(imgs[1]);
    expect(onPic).toHaveBeenCalledWith("https://e.com/p.png", "d");
  });
  it("shows a script tag as text", () => {
    const { container } = render(<Markdown src={"<script>alert(1)</script> hi\nnext line"} />);
    expect(container.querySelector("script")).toBeNull();
    expect(container.innerHTML).toBe("<p>&lt;script&gt;alert(1)&lt;/script&gt; hi<br>next line</p>");
  });
  it("shows html in a heading, a cell and a link label as text", () => {
    const { container } = render(<Markdown src={"# <img src=x onerror=1>\n\n| <b>h</b> |\n|---|\n| <i>c</i> |\n\n[<u>l</u>](https://a.b)"} />);
    expect(container.querySelector("img, b, i, u")).toBeNull();
  });
  it("asks onDiagram for a closed fence, and falls back to code", () => {
    const onDiagram = vi.fn((lang: string) => (lang === "chart" ? <figure>drawn</figure> : null));
    const { container } = render(<Markdown src={"```chart\nx\n```\n\n```text\ny\n```"} onDiagram={onDiagram} />);
    expect(onDiagram).toHaveBeenCalledTimes(2);
    expect(container.querySelector("figure")?.textContent).toBe("drawn");
    expect(container.querySelector("pre code")?.textContent).toBe("y");
  });
  it("does not draw a fence that is still streaming in", () => {
    const onDiagram = vi.fn(() => <figure />);
    const { container } = render(<Markdown src={"```chart\nx"} onDiagram={onDiagram} />);
    expect(onDiagram).not.toHaveBeenCalled();
    expect(container.querySelector("pre code")?.textContent).toBe("x");
  });
});
