// phone/diagrams.js drawing ```chart and ```mermaid blocks, checked against
// what the desktop accepts (the tests in core/src/agent/chart.rs).
// Run with `node --test phone/test/`.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { test } from "node:test";

const require = createRequire(import.meta.url);
const D = require("../diagrams.js");

test("a JSON chart with two series", () => {
  const { chart } = D.parse(
    "chart",
    '{"type":"line","title":"Latency","unit":"ms","labels":["a","b"],"series":[{"name":"x","data":[1,2]},{"name":"y","data":[3]}]}',
  );
  assert.equal(chart.kind, "line");
  assert.equal(chart.title, "Latency");
  assert.deepEqual(chart.labels, ["a", "b"]);
  assert.deepEqual(chart.series.map((s) => s.name), ["x", "y"]);
  assert.deepEqual(chart.series[0].values, [1, 2]);
  const { chart: one } = D.parse("chart", '{"data":[{"label":"a","value":3}]}');
  assert.deepEqual(one.labels, ["a"]);
  assert.deepEqual(one.series[0].values, [3]);
  const html = D.render("chart", '{"type":"bar","labels":["Mon","Tue","Wed"],"series":[{"name":"p50","data":[12,9,14]},{"name":"p95","data":[30,22,41]}]}');
  assert.match(html, /<svg class="dgsvg dgbars"/);
  assert.match(html, /Chart · 3 groups/);
  assert.match(html, /class="dglegend"/);
});

test("label: value lines carry their unit", () => {
  const { chart } = D.parse("chart", "title: Build: times\nMon: 12ms\nTue: 9.5ms");
  assert.equal(chart.kind, "bar");
  assert.equal(chart.title, "Build: times");
  assert.deepEqual(chart.labels, ["Mon", "Tue"]);
  assert.deepEqual(chart.series[0].values, [12, 9.5]);
  assert.equal(chart.unit, "ms");
  assert.equal(D.fmtValue(chart, 12), "12ms");
  assert.deepEqual(D.numberWithUnit("$1,200"), [1200, "$"]);
  assert.equal(D.fmtNum(12000), "12k");
  assert.equal(D.fmtNum(3.5), "3.5");
  assert.equal(D.niceMax(37), 50);
});

test("a mermaid pie", () => {
  const { chart } = D.parse("mermaid", 'pie title Pets\n  "Dogs" : 386\n  "Cats" : 85');
  assert.equal(chart.kind, "pie");
  assert.equal(chart.title, "Pets");
  assert.deepEqual(chart.labels, ["Dogs", "Cats"]);
  assert.deepEqual(chart.series[0].values, [386, 85]);
  const html = D.render("mermaid", 'pie title Pets\n  "Dogs" : 386\n  "Cats" : 85');
  assert.match(html, /Chart · 2 slices/);
  assert.match(html, /82%/);
});

test("a top-down flowchart with labeled edges", () => {
  const src = "flowchart TD\n  A[Start] --> B{Ok?}\n  B -->|yes| C([Done])\n  B -- no --> D(Retry) -.-> A\n  E & F --> C\n  classDef x fill:#f00";
  const { flow } = D.parse("mermaid", src);
  assert.deepEqual(flow.nodes.map((n) => n.id), ["A", "B", "C", "D", "E", "F"]);
  assert.equal(flow.nodes[1].shape, "diamond");
  assert.equal(flow.nodes[2].label, "Done");
  assert.equal(flow.edges.length, 6);
  assert.equal(flow.edges[1].label, "yes");
  assert.equal(flow.edges[2].label, "no");
  assert.ok(flow.edges[3].dotted && flow.edges[3].arrow);
  assert.ok(!flow.horizontal);
  // A starts above B, B above C.
  assert.ok(flow.placed[0].y < flow.placed[1].y && flow.placed[1].y < flow.placed[2].y);
  const html = D.render("mermaid", src);
  assert.match(html, /Diagram · 6 steps/);
  assert.match(html, />yes<\/text>/);
  assert.match(html, /class="dgbtn dgcode"/);
  assert.match(html, /class="dgbtn dgopen"/);
});

test("a wide left-to-right flowchart turns top-down to fit the phone", () => {
  const { flow } = D.parse(
    "mermaid",
    "flowchart LR\n  A[Collect the logs] --> B[Group by session] --> C[Price every call] --> D[Sum per project] --> E[Write the report]",
  );
  assert.ok(!flow.horizontal);
  assert.ok(flow.width <= D.FIT_W);
  const { flow: short } = D.parse("mermaid", "flowchart LR\n  A[Plan] --> B[Code] --> C[Ship]\n  C -. bug .-> A");
  assert.ok(short.horizontal);
});

test("blocks that do not parse stay code", () => {
  assert.equal(D.render("chart", '{"type": "bar", "data": ['), null);
  assert.equal(D.render("chart", "just some words"), null);
  assert.equal(D.render("mermaid", "sequenceDiagram\nA->>B: hi"), null);
  assert.equal(D.render("mermaid", "flowchart TD\n  A[unclosed --> B"), null);
  assert.equal(D.render("js", "const a = 1;"), null);
});

test("labels are escaped", () => {
  const flow = D.render("mermaid", 'flowchart TD\n  A["<img src=x onerror=alert(1)>"] -->|"a & <b>"| B');
  assert.ok(!flow.includes("<img"));
  assert.ok(!flow.includes("<b>"));
  assert.match(flow, /&lt;img src=x/);
  assert.match(flow, /a &amp; &lt;b&gt;/);
  const chart = D.render("chart", '{"title":"<script>x</script>","data":{"<i>":1}}');
  assert.ok(!chart.includes("<script>"));
  assert.ok(!chart.includes("<i>"));
  assert.match(chart, /&lt;script&gt;/);
});
