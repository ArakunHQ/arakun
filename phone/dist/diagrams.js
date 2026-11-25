// Charts and diagrams drawn from fenced blocks in agent replies, as the desktop
// draws them (core/src/agent/chart.rs): ```chart (JSON, or `label: value` lines)
// and ```mermaid (flowcharts and pies). The same input is accepted and the
// flowchart is laid out by the same rules; here it is drawn as inline SVG.
// Anything that does not parse stays a code block: render() returns null.
"use strict";
(function () {
  // Wrapped in a function: the other page scripts are classic scripts too,
  // and names such as `esc` at the top level would clash with theirs.

  const esc = (s) =>
    String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

  // The desktop's category colors (theme::CATEGORY) in its order: blue, violet,
  // green, amber, rose, cyan, coral, sand, so neighbors stay far apart.
  const PALETTE = ["#86a6f2", "#b394f5", "#6fd3a0", "#ffb547", "#ee8fc0", "#5cc8db", "#f5826f", "#d9c27e"];
  const color = (i) => PALETTE[i % PALETTE.length];

  // Rust's str::lines(): no empty line after a final newline, and none at all for "".
  function lines(s) {
    if (s === "") return [];
    const out = s.split("\n");
    if (out[out.length - 1] === "") out.pop();
    return out.map((l) => l.replace(/\r$/, ""));
  }

  const utf8Len = (s) => new TextEncoder().encode(s).length;

  // ---------------------------------------------------------------------------
  // Charts

  function parseChart(text) {
    let chart;
    let json;
    let isJson = true;
    try {
      json = JSON.parse(text.trim());
    } catch (e) {
      isJson = false;
    }
    chart = isJson ? chartFromJson(json) : chartFromLines(text);
    if (!chart) return null;
    const n = Math.max(0, ...chart.series.map((s) => s.values.length));
    const finite = chart.series.every((s) => s.values.every(Number.isFinite));
    return n > 0 && finite ? chart : null;
  }

  function kindOf(s) {
    switch (String(s).trim().toLowerCase()) {
      case "line":
        return "line";
      case "area":
        return "area";
      case "pie":
      case "donut":
      case "doughnut":
        return "pie";
      case "hbar":
      case "barh":
      case "horizontal":
      case "row":
        return "hbar";
      default:
        return "bar";
    }
  }

  // A number and whatever unit trails it: `12.5ms` -> [12.5, "ms"].
  function numberWithUnit(s) {
    s = s.trim().replace(/^"+|"+$/g, "");
    let prefix = "";
    if (s.startsWith("$")) {
      prefix = "$";
      s = s.slice(1);
    }
    let end = 0;
    while (end < s.length && (/[0-9.,]/.test(s[end]) || (end === 0 && s[end] === "-"))) end++;
    const digits = s.slice(0, end).replace(/,/g, "");
    // What Rust's f64 parse takes from these characters.
    if (!/^-?(\d+\.?\d*|\.\d+)$/.test(digits)) return null;
    return [Number(digits), prefix || s.slice(end).trim()];
  }

  function num(v) {
    if (typeof v === "number") return v;
    if (typeof v === "string") {
      const got = numberWithUnit(v);
      return got ? got[0] : null;
    }
    return null;
  }

  function labelOf(v) {
    if (typeof v === "string") return v;
    if (v === null || v === undefined) return "";
    return JSON.stringify(v);
  }

  const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);
  const field = (v, k) => (isObject(v) ? v[k] : undefined);
  const str = (v) => (typeof v === "string" ? v : undefined);

  function chartFromJson(v) {
    const kind = kindOf(str(field(v, "type")) ?? str(field(v, "kind")) ?? "bar");
    const title = str(field(v, "title")) ?? null;
    const unit = str(field(v, "unit")) ?? "";
    const rawLabels = field(v, "labels");
    let labels = Array.isArray(rawLabels) ? rawLabels.map(labelOf) : [];
    const series = [];
    const list = Array.isArray(field(v, "series")) ? field(v, "series") : Array.isArray(field(v, "datasets")) ? field(v, "datasets") : null;
    if (list) {
      list.forEach((s, i) => {
        const name = str(field(s, "name")) ?? str(field(s, "label")) ?? `Series ${i + 1}`;
        const data = Array.isArray(field(s, "data")) ? field(s, "data") : Array.isArray(field(s, "values")) ? field(s, "values") : [];
        series.push({ name, values: data.map((x) => num(x) ?? 0) });
      });
    } else {
      const data = field(v, "data");
      let values;
      if (Array.isArray(data) && data.length && data.every(isObject)) {
        labels = data.map((x) => labelOf(x.label ?? x.name ?? x.x ?? null));
        values = data.map((x) => num(x.value ?? x.y ?? null) ?? 0);
      } else if (Array.isArray(data)) {
        values = data.map((x) => num(x) ?? 0);
      } else if (isObject(data)) {
        // serde_json keeps object keys sorted; so does the desktop's chart.
        const keys = Object.keys(data).sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
        labels = keys;
        values = keys.map((k) => num(data[k]) ?? 0);
      } else {
        return null;
      }
      series.push({ name: "", values });
    }
    const n = Math.max(0, ...series.map((s) => s.values.length));
    labels = labels.slice(0, n);
    while (labels.length < n) labels.push(String(labels.length + 1));
    return { kind, title, labels, series, unit };
  }

  // `type: line`, `title: ...`, `unit: ms`, then one `label: value` per line.
  function chartFromLines(text) {
    const chart = { kind: "bar", title: null, labels: [], series: [{ name: "", values: [] }], unit: "" };
    for (let line of lines(text)) {
      line = line.trim().replace(/^[-*]+/, "").trim();
      if (!line || line.startsWith("#") || line.startsWith("//")) continue;
      const colon = line.indexOf(":");
      if (colon >= 0) {
        const key = line.slice(0, colon).trim().toLowerCase();
        const value = line.slice(colon + 1);
        if (key === "type" || key === "kind") {
          chart.kind = kindOf(value);
          continue;
        }
        if (key === "title") {
          chart.title = value.trim().replace(/^"+|"+$/g, "");
          continue;
        }
        if (key === "unit") {
          chart.unit = value.trim();
          continue;
        }
      }
      let at = line.lastIndexOf(":");
      if (at < 0) at = line.lastIndexOf("=");
      if (at < 0) return null;
      const got = numberWithUnit(line.slice(at + 1));
      if (!got) return null;
      if (!chart.unit) chart.unit = got[1];
      chart.labels.push(line.slice(0, at).trim().replace(/^"+|"+$/g, ""));
      chart.series[0].values.push(got[0]);
    }
    return chart;
  }

  // Rust's f64::round: halves away from zero.
  const roundAway = (x) => Math.sign(x) * Math.round(Math.abs(x));

  function fmtNum(v) {
    const a = Math.abs(v);
    let x = v;
    let suffix = "";
    if (a >= 1e9) [x, suffix] = [v / 1e9, "B"];
    else if (a >= 1e6) [x, suffix] = [v / 1e6, "M"];
    else if (a >= 1e4) [x, suffix] = [v / 1e3, "k"];
    let s;
    if (Math.abs(x - roundAway(x)) < 1e-9) s = String(roundAway(x) + 0);
    else if (Math.abs(x) >= 100) s = x.toFixed(0);
    else if (Math.abs(x) >= 10) s = x.toFixed(1);
    else s = x.toFixed(2);
    if (s.includes(".")) s = s.replace(/0+$/, "").replace(/\.$/, "");
    return s + suffix;
  }

  function fmtValue(chart, v) {
    const n = fmtNum(v);
    const u = chart.unit;
    if (u === "") return n;
    if (u === "$" || u === "€" || u === "£") return u + n;
    return utf8Len(u) <= 2 ? n + u : `${n} ${u}`;
  }

  // The round number at or above `v` that an axis can end on.
  function niceMax(v) {
    if (v <= 0) return 1;
    const pow = Math.pow(10, Math.floor(Math.log10(v)));
    for (const m of [1, 2, 2.5, 5, 10]) if (m * pow >= v - 1e-9) return m * pow;
    return 10 * pow;
  }

  // ---------------------------------------------------------------------------
  // Mermaid

  function parseMermaid(text) {
    const st = statements(text);
    if (!st.length) return null;
    const words = st[0].split(/\s+/).filter(Boolean);
    switch (words[0]) {
      case "pie": {
        const chart = parsePie(st);
        return chart ? { type: "chart", chart } : null;
      }
      case "graph":
      case "flowchart": {
        const dir = (words[1] || "TD").toUpperCase();
        const flow = parseFlow(dir === "LR" || dir === "RL", st.slice(1));
        return flow ? { type: "flow", flow } : null;
      }
      default:
        return null;
    }
  }

  // Lines split at `;` outside brackets and quotes, comments dropped.
  function statements(text) {
    const out = [];
    for (let line of lines(text)) {
      line = line.trim();
      if (!line || line.startsWith("%%")) continue;
      let depth = 0;
      let quoted = false;
      let cur = "";
      for (const c of line) {
        if (c === '"') quoted = !quoted;
        else if (!quoted && "[({".includes(c)) depth++;
        else if (!quoted && "])}".includes(c)) depth--;
        else if (c === ";" && !quoted && depth <= 0) {
          out.push(cur);
          cur = "";
          continue;
        }
        cur += c;
      }
      out.push(cur);
    }
    return out.map((s) => s.trim()).filter(Boolean);
  }

  const FLOAT = /^[+-]?(\d+\.?\d*|\.\d+)([eE][+-]?\d+)?$/;

  function parsePie(st) {
    const chart = { kind: "pie", title: null, labels: [], series: [{ name: "", values: [] }], unit: "" };
    for (let i = 0; i < st.length; i++) {
      let s = st[i];
      if (i === 0) s = s.replace(/^(pie)+/, "").trim();
      s = s.replace(/^(showData)+/, "").trim();
      if (s.startsWith("title")) {
        chart.title = s.slice(5).trim();
        continue;
      }
      const at = s.lastIndexOf(":");
      if (at < 0) continue;
      const value = s.slice(at + 1).trim();
      if (!FLOAT.test(value)) return null;
      chart.labels.push(s.slice(0, at).trim().replace(/^"+|"+$/g, ""));
      chart.series[0].values.push(Number(value));
    }
    return chart.labels.length ? chart : null;
  }

  const SKIP = ["subgraph", "end", "classDef", "class", "style", "linkStyle", "click", "direction", "accTitle", "accDescr"];

  const isId = (c) => c !== undefined && /[\p{Alphabetic}\p{N}_]/u.test(c);
  const isWs = (c) => c !== undefined && /\s/.test(c);
  const isEdge = (c) => c !== undefined && "-=.<>~".includes(c);
  function skipWs(s, at) {
    while (at.i < s.length && isWs(s[at.i])) at.i++;
  }
  const starts = (s, i, pat) => Array.from(pat).every((c, k) => s[i + k] === c);

  function cleanLabel(s) {
    s = s.trim();
    if (s.length >= 2 && s.startsWith('"') && s.endsWith('"')) s = s.slice(1, -1);
    if (s.length >= 2 && s.startsWith("`") && s.endsWith("`")) s = s.slice(1, -1);
    return s
      .split("<br>").join("\n")
      .split("<br/>").join("\n")
      .split("<br />").join("\n")
      .split("&quot;").join('"')
      .split("&amp;").join("&")
      .trim();
  }

  const SHAPES = [
    ["(((", ")))", "circle"],
    ["((", "))", "circle"],
    ["([", "])", "stadium"],
    ["[[", "]]", "rect"],
    ["[(", ")]", "round"],
    ["[/", "/]", "rect"],
    ["[\\", "\\]", "rect"],
    ["{{", "}}", "hex"],
    ["[", "]", "rect"],
    ["(", ")", "round"],
    ["{", "}", "diamond"],
    [">", "]", "rect"],
  ];

  class FlowBuilder {
    constructor() {
      this.nodes = [];
      this.index = new Map();
      this.edges = [];
    }

    node(id, label) {
      let ix = this.index.get(id);
      if (ix === undefined) {
        this.nodes.push({ id, label: id, shape: "rect" });
        ix = this.nodes.length - 1;
        this.index.set(id, ix);
      }
      if (label) {
        this.nodes[ix].label = label[0];
        this.nodes[ix].shape = label[1];
      }
      return ix;
    }

    nodeRef(s, at) {
      const start = at.i;
      while (at.i < s.length) {
        const c = s[at.i];
        const joins = (c === "-" || c === ".") && isId(s[at.i + 1]);
        if (isId(c) || (joins && at.i > start)) at.i++;
        else break;
      }
      if (at.i === start) return null;
      const id = s.slice(start, at.i).join("");
      let label = null;
      for (const [open, close, shape] of SHAPES) {
        if (!starts(s, at.i, open)) continue;
        const from = at.i + Array.from(open).length;
        let j = from;
        let quoted = false;
        while (j < s.length && (quoted || !starts(s, j, close))) {
          if (s[j] === '"') quoted = !quoted;
          j++;
        }
        if (j >= s.length) return null;
        label = [cleanLabel(s.slice(from, j).join("")), shape];
        at.i = j + Array.from(close).length;
        break;
      }
      if (starts(s, at.i, ":::")) {
        at.i += 3;
        while (at.i < s.length && (isId(s[at.i]) || s[at.i] === "-")) at.i++;
      }
      return this.node(id, label);
    }

    // `A`, or `A & B & C`.
    group(s, at) {
      const first = this.nodeRef(s, at);
      if (first === null) return null;
      const out = [first];
      for (;;) {
        const j = { i: at.i };
        skipWs(s, j);
        if (s[j.i] !== "&") return out;
        j.i++;
        skipWs(s, j);
        at.i = j.i;
        const next = this.nodeRef(s, at);
        if (next === null) return null;
        out.push(next);
      }
    }

    statement(line) {
      const s = Array.from(line);
      const at = { i: 0 };
      let prev = null;
      for (;;) {
        skipWs(s, at);
        const group = this.group(s, at);
        if (!group) return false;
        if (prev) {
          for (const a of prev.from) {
            for (const b of group) this.edges.push({ from: a, to: b, ...prev.style });
          }
          prev = null;
        }
        skipWs(s, at);
        if (at.i >= s.length) return true;
        const style = edgeOp(s, at);
        if (!style) return false;
        prev = { from: group, style };
      }
    }
  }

  function edgeRun(s, at) {
    const start = at.i;
    while (at.i < s.length && isEdge(s[at.i])) at.i++;
    let op = s.slice(start, at.i).join("");
    // `--o` and `--x` end in a circle or cross; drawn as arrows.
    if ((s[at.i] === "o" || s[at.i] === "x") && (at.i + 1 >= s.length || isWs(s[at.i + 1])) && /[-=]$/.test(op)) {
      at.i++;
      op += ">";
    }
    return op;
  }

  function edgeOp(s, at) {
    let op = edgeRun(s, at);
    if (op.length < 2) return null;
    let label = null;
    // `-- text -->`, `== text ==>`, `-. text .->`
    if (op === "--" || op === "==" || op === "-.") {
      let j = at.i;
      while (j < s.length) {
        if (isEdge(s[j]) && isEdge(s[j + 1])) break;
        j++;
      }
      if (j >= s.length) return null;
      label = cleanLabel(s.slice(at.i, j).join(""));
      at.i = j;
      op += edgeRun(s, at);
    }
    skipWs(s, at);
    if (s[at.i] === "|") {
      const from = at.i + 1;
      let j = from;
      while (j < s.length && s[j] !== "|") j++;
      label = cleanLabel(s.slice(from, Math.min(j, s.length)).join(""));
      at.i = Math.min(j + 1, s.length);
    }
    return { label: label || null, arrow: op.includes(">"), dotted: op.includes("."), thick: op.includes("=") };
  }

  const PAD = 12;
  const NODE_LINE_H = 18;
  const EDGE_LINE_H = 14;
  // Wider than this, a flowchart is laid out in the other direction when that
  // one is narrower. The desktop uses its reply column (820); a phone column
  // is about 340, and a drawing scaled below about three quarters of its size
  // gets hard to read, so the phone turns sooner.
  const FIT_W = 440;

  const NARROW = new Set(["i", "l", "j", "I", ".", ",", ":", ";", "'", "|", "!", "(", ")"]);
  // Roughly how wide `s` is at the node font size, in pixels.
  function textW(s) {
    let w = 0;
    for (const c of s) {
      if (c === " ") w += 3.6;
      else if (NARROW.has(c)) w += 4.2;
      else if (c === "t" || c === "f" || c === "r") w += 5;
      else if (c === "m" || c === "w" || c === "M" || c === "W") w += 10.5;
      else if (/\p{Uppercase}/u.test(c)) w += 8.6;
      else if (c >= "0" && c <= "9") w += 7.4;
      else w += 7;
    }
    return w;
  }

  // Greedy word wrap to `max` pixels per line, keeping the label's own breaks.
  // A word longer than a line gets a line of its own.
  function wrapLabel(label, max) {
    const out = [];
    for (const para of lines(label)) {
      let line = "";
      for (const word of para.split(/\s+/).filter(Boolean)) {
        if (line && textW(line) + textW(" ") + textW(word) > max) {
          out.push(line);
          line = "";
        }
        if (line) line += " ";
        line += word;
      }
      out.push(line);
    }
    if (!out.length) out.push("");
    return out;
  }

  // An edge label as drawn, one string per line, and its pill's size. The text
  // is smaller than a node's, so lines wrap at a node-scale 182 px to keep the
  // pill within 180.
  function edgeLabelBox(label) {
    const ls = wrapLabel(label, 182);
    const tw = Math.max(0, ...ls.map(textW));
    return { lines: ls, w: Math.round(Math.min(tw * 0.9 + 16, 180)), h: EDGE_LINE_H * ls.length + 4 };
  }

  // A node's label as drawn, one string per line, and the box that holds it.
  function nodeBox(n) {
    const max = n.shape === "diamond" ? 120 : n.shape === "circle" ? 110 : 168;
    const ls = wrapLabel(n.label, max);
    const tw = Math.max(0, ...ls.map(textW));
    const th = NODE_LINE_H * ls.length;
    let w;
    let h;
    switch (n.shape) {
      // A rhombus holds a tw x th box when tw/w + th/h <= 1; keep some room.
      case "diamond":
        [w, h] = [Math.max(tw / 0.58, 72), Math.max(th / 0.36, 56)];
        break;
      case "circle": {
        const d = Math.max(Math.hypot(tw, th) + 16, 56);
        [w, h] = [d, d];
        break;
      }
      case "hex":
        [w, h] = [tw + 52, th + 18];
        break;
      case "stadium":
        [w, h] = [tw + th + 32, th + 18];
        break;
      default:
        [w, h] = [Math.max(tw + 28, 72), th + 18];
    }
    return { lines: ls, size: [Math.round(w), Math.round(h)] };
  }

  // Ranks by longest path over the edges that do not close a cycle, and an
  // order inside each rank from two barycenter sweeps.
  function rankNodes(n, edges) {
    const out = Array.from({ length: n }, () => []);
    for (const e of edges) if (e.from !== e.to) out[e.from].push(e.to);
    // Edges into a node still on the DFS stack close a cycle; leave them out.
    const state = new Array(n).fill(0);
    const forward = Array.from({ length: n }, () => []);
    for (let root = 0; root < n; root++) {
      if (state[root] !== 0) continue;
      const stack = [[root, 0]];
      state[root] = 1;
      while (stack.length) {
        const top = stack[stack.length - 1];
        const v = top[0];
        if (top[1] < out[v].length) {
          const w = out[v][top[1]++];
          if (state[w] === 0) {
            forward[v].push(w);
            state[w] = 1;
            stack.push([w, 0]);
          } else if (state[w] === 2) {
            forward[v].push(w);
          }
        } else {
          state[v] = 2;
          stack.pop();
        }
      }
    }
    const indeg = new Array(n).fill(0);
    for (const ws of forward) for (const w of ws) indeg[w]++;
    const rank = new Array(n).fill(0);
    const queue = [];
    for (let v = 0; v < n; v++) if (indeg[v] === 0) queue.push(v);
    for (let head = 0; head < queue.length; head++) {
      const v = queue[head];
      for (const w of forward[v]) {
        rank[w] = Math.max(rank[w], rank[v] + 1);
        if (--indeg[w] === 0) queue.push(w);
      }
    }
    const ranks = n ? Math.max(...rank) + 1 : 0;
    let layers = Array.from({ length: ranks }, () => []);
    for (let v = 0; v < n; v++) layers[rank[v]].push(v);
    const preds = Array.from({ length: n }, () => []);
    forward.forEach((ws, v) => ws.forEach((w) => preds[w].push(v)));
    const pos = new Array(n).fill(0);
    const index = () => layers.forEach((layer) => layer.forEach((v, i) => (pos[v] = i)));
    index();
    const mean = (vs, own) => (vs.length ? vs.reduce((a, u) => a + pos[u], 0) / vs.length : own);
    const sortLayer = (r, by) => {
      // Array.sort is stable, as Rust's sort_by is.
      layers[r] = layers[r].map((v) => [v, mean(by[v], pos[v])]).sort((a, b) => a[1] - b[1]).map((p) => p[0]);
      index();
    };
    for (let pass = 0; pass < 2; pass++) {
      for (let r = 1; r < ranks; r++) sortLayer(r, preds);
      for (let r = ranks - 2; r >= 0; r--) sortLayer(r, forward);
    }
    return { rank, layers };
  }

  function bezier(c, t) {
    const u = 1 - t;
    const k = [u * u * u, 3 * u * u * t, 3 * u * t * t, t * t * t];
    const f = (i) => k[0] * c.from[i] + k[1] * c.c1[i] + k[2] * c.c2[i] + k[3] * c.to[i];
    return [f(0), f(1)];
  }

  // Node boxes and edge curves for one direction.
  function place(edges, sizes, rank, layers, h) {
    const n = sizes.length;
    // Along = the direction ranks advance in; across = inside a rank.
    const along = (s) => (h ? s[0] : s[1]);
    const across = (s) => (h ? s[1] : s[0]);
    const nodeGap = h ? 24 : 32;
    // A labeled edge between neighboring ranks needs room for its pill.
    const gaps = new Array(layers.length).fill(h ? 64 : 52);
    for (const e of edges) {
      if (e.label && rank[e.to] === rank[e.from] + 1) {
        const b = edgeLabelBox(e.label);
        const need = h ? b.w + 36 : b.h + 36;
        gaps[rank[e.from]] = Math.max(gaps[rank[e.from]], need);
      }
    }
    const spans = layers.map((l) => l.reduce((a, v) => a + across(sizes[v]), 0) + nodeGap * Math.max(0, l.length - 1));
    const widest = Math.max(0, ...spans);
    const placed = new Array(n);
    let cursor = 0;
    layers.forEach((layer, r) => {
      const band = Math.max(0, ...layer.map((v) => along(sizes[v])));
      let x = (widest - spans[r]) / 2;
      for (const v of layer) {
        const s = sizes[v];
        const a = cursor + (band - along(s)) / 2;
        placed[v] = h ? { x: a, y: x, w: s[0], h: s[1] } : { x, y: a, w: s[0], h: s[1] };
        x += across(s) + nodeGap;
      }
      cursor += band + gaps[r];
    });
    const slot = (v) => {
      const layer = layers[rank[v]];
      const i = Math.max(0, layer.indexOf(v));
      return [i, layer.length - 1 - i];
    };
    const straight = (f, t) => ({ from: f, c1: f, c2: t, to: t });

    const curves = edges.map((e) => {
      const a = placed[e.from];
      const b = placed[e.to];
      if (e.from === e.to) {
        // A loop on the node's outer side.
        if (h) {
          const [x, y] = [a.x + a.w / 2, a.y + a.h];
          return { from: [x - 8, y], c1: [x - 28, y + 36], c2: [x + 28, y + 36], to: [x + 8, y] };
        }
        const [x, y] = [a.x + a.w, a.y + a.h / 2];
        return { from: [x, y - 8], c1: [x + 36, y - 28], c2: [x + 36, y + 28], to: [x, y + 8] };
      }
      if (rank[e.to] > rank[e.from]) {
        if (h) {
          const [f, t] = [[a.x + a.w, a.y + a.h / 2], [b.x, b.y + b.h / 2]];
          const d = (t[0] - f[0]) * 0.5;
          return { from: f, c1: [f[0] + d, f[1]], c2: [t[0] - d, t[1]], to: t };
        }
        const [f, t] = [[a.x + a.w / 2, a.y + a.h], [b.x + b.w / 2, b.y]];
        const d = (t[1] - f[1]) * 0.5;
        return { from: f, c1: [f[0], f[1] + d], c2: [t[0], t[1] - d], to: t };
      }
      if (rank[e.to] === rank[e.from]) {
        const l = h ? a.y < b.y : a.x < b.x;
        if (h) {
          return l
            ? straight([a.x + a.w / 2, a.y + a.h], [b.x + b.w / 2, b.y])
            : straight([a.x + a.w / 2, a.y], [b.x + b.w / 2, b.y + b.h]);
        }
        return l
          ? straight([a.x + a.w, a.y + a.h / 2], [b.x, b.y + b.h / 2])
          : straight([a.x, a.y + a.h / 2], [b.x + b.w, b.y + b.h / 2]);
      }
      // A back edge goes around the ranks it spans, on the side with fewer
      // nodes in the way at its two ends, so it does not cross the flow.
      const [lo, hi] = [rank[e.to], rank[e.from]];
      const band = [];
      for (let v = 0; v < n; v++) if (rank[v] >= lo && rank[v] <= hi) band.push(placed[v]);
      const [ab, aa] = slot(e.from);
      const [bb, ba] = slot(e.to);
      const near = ab + bb < aa + ba;
      const away = 36;
      if (h) {
        if (near) {
          const y = Math.min(...band.map((p) => p.y)) - away;
          const [f, t] = [[a.x + a.w / 2, a.y], [b.x + b.w / 2, b.y]];
          return { from: f, c1: [f[0], y], c2: [t[0], y], to: t };
        }
        const y = Math.max(...band.map((p) => p.y + p.h)) + away;
        const [f, t] = [[a.x + a.w / 2, a.y + a.h], [b.x + b.w / 2, b.y + b.h]];
        return { from: f, c1: [f[0], y], c2: [t[0], y], to: t };
      }
      if (near) {
        const x = Math.min(...band.map((p) => p.x)) - away;
        const [f, t] = [[a.x, a.y + a.h / 2], [b.x, b.y + b.h / 2]];
        return { from: f, c1: [x, f[1]], c2: [x, t[1]], to: t };
      }
      const x = Math.max(...band.map((p) => p.x + p.w)) + away;
      const [f, t] = [[a.x + a.w, a.y + a.h / 2], [b.x + b.w, b.y + b.h / 2]];
      return { from: f, c1: [x, f[1]], c2: [x, t[1]], to: t };
    });

    // Everything drawn, edge labels included, then moved inside the padding.
    let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
    const grow = (x, y) => {
      x0 = Math.min(x0, x);
      y0 = Math.min(y0, y);
      x1 = Math.max(x1, x);
      y1 = Math.max(y1, y);
    };
    for (const p of placed) {
      grow(p.x, p.y);
      grow(p.x + p.w, p.y + p.h);
    }
    edges.forEach((e, i) => {
      const c = curves[i];
      for (let k = 0; k <= 16; k++) grow(...bezier(c, k / 16));
      if (e.label) {
        const [mx, my] = bezier(c, 0.5);
        const b = edgeLabelBox(e.label);
        grow(mx - b.w / 2, my - b.h / 2);
        grow(mx + b.w / 2, my + b.h / 2);
      }
    });
    const [dx, dy] = [PAD - x0, PAD - y0];
    for (const p of placed) {
      p.x += dx;
      p.y += dy;
    }
    const shift = (q) => [q[0] + dx, q[1] + dy];
    const moved = curves.map((c) => ({ from: shift(c.from), c1: shift(c.c1), c2: shift(c.c2), to: shift(c.to) }));
    return { placed, curves: moved, width: Math.ceil(x1 - x0 + 2 * PAD), height: Math.ceil(y1 - y0 + 2 * PAD) };
  }

  function parseFlow(horizontal, st) {
    const b = new FlowBuilder();
    for (const s of st) {
      const first = s.split(/\s+/)[0] || "";
      if (SKIP.includes(first)) continue;
      if (!b.statement(s)) return null;
    }
    if (!b.nodes.length || b.nodes.length > 120) return null;
    const boxes = b.nodes.map(nodeBox);
    const sizes = boxes.map((x) => x.size);
    const { rank, layers } = rankNodes(b.nodes.length, b.edges);
    let laid = place(b.edges, sizes, rank, layers, horizontal);
    if (laid.width > FIT_W) {
      const other = place(b.edges, sizes, rank, layers, !horizontal);
      if (other.width < laid.width) {
        laid = other;
        horizontal = !horizontal;
      }
    }
    return { horizontal, nodes: b.nodes, edges: b.edges, lines: boxes.map((x) => x.lines), ...laid };
  }

  // The diagram a fenced block draws, if its language is one and it parses.
  function parse(lang, text) {
    switch (String(lang || "").toLowerCase()) {
      case "chart": {
        const chart = parseChart(String(text));
        return chart ? { type: "chart", chart } : null;
      }
      case "mermaid":
        return parseMermaid(String(text));
      default:
        return null;
    }
  }

  // ---------------------------------------------------------------------------
  // Drawing

  const r1 = (v) => Math.round(v * 10) / 10;
  const pt = (q) => `${r1(q[0])} ${r1(q[1])}`;
  // Axis and value labels are drawn at 11px; textW measures 13px text.
  const smallW = (s) => (textW(s) * 11) / 13;

  // Cut a label to `max` pixels at font size `px`, with an ellipsis, as the
  // desktop truncates.
  function fit(s, max, px = 11) {
    const w = (t) => (textW(t) * px) / 13;
    if (w(s) <= max) return s;
    const cs = Array.from(s);
    while (cs.length && w(cs.join("") + "…") > max) cs.pop();
    return cs.join("") + "…";
  }

  function textEl(x, y, s, cls, anchor) {
    return `<text x="${r1(x)}" y="${r1(y)}" class="${cls}" text-anchor="${anchor || "middle"}" dominant-baseline="central">${esc(s)}</text>`;
  }

  const svgOpen = (w, h, cls, extra) =>
    `<svg class="dgsvg ${cls}" viewBox="0 0 ${w} ${h}" width="${w}" height="${h}" preserveAspectRatio="xMidYMid meet"${extra || ""}>`;

  // Charts are drawn this wide and scaled to the card: about a phone column.
  const CHART_W = 340;
  const PLOT_H = 160;

  // A bar with rounded top corners, standing on `base`.
  function barPath(x, base, w, h, r) {
    r = Math.min(r, w / 2, h);
    const top = base - h;
    return `<path d="M${r1(x)} ${r1(base)}V${r1(top + r)}Q${r1(x)} ${r1(top)} ${r1(x + r)} ${r1(top)}H${r1(x + w - r)}Q${r1(x + w)} ${r1(top)} ${r1(x + w)} ${r1(top + r)}V${r1(base)}Z"`;
  }

  function bars(c) {
    const max = niceMax(Math.max(0, ...c.series.flatMap((s) => s.values)));
    const g = c.labels.length;
    const s = c.series.length;
    const gw = (CHART_W - 10 * (g - 1)) / g;
    const bw = Math.min(40, (gw - 4 * (s - 1)) / s);
    const values = c.series.flatMap((x) => x.values.map((v) => fmtValue(c, Math.max(0, v))));
    // Values sit over their bars; left out when there are many, or when they
    // would run into the next bar.
    const room = s === 1 ? gw + 8 : bw + 4;
    const showValues = g * s <= 16 && values.every((t) => smallW(t) <= room);
    const top = showValues ? 16 : 4;
    const axis = top + PLOT_H;
    const height = axis + 22;
    let out = svgOpen(CHART_W, height, "dgbars");
    c.labels.forEach((label, gi) => {
      const gx = gi * (gw + 10);
      const used = bw * s + 4 * (s - 1);
      c.series.forEach((series, si) => {
        const v = Math.max(0, series.values[gi] ?? 0);
        const h = Math.max(2, (v / max) * PLOT_H);
        const x = gx + (gw - used) / 2 + si * (bw + 4);
        out += `${barPath(x, axis, bw, h, 4)} fill="${color(si)}"/>`;
        if (showValues) out += textEl(x + bw / 2, axis - h - 8, fmtValue(c, v), "dgt");
      });
      out += textEl(gx + gw / 2, axis + 12, fit(label, gw), "dgt");
    });
    out += `<line x1="0" y1="${axis + 0.5}" x2="${CHART_W}" y2="${axis + 0.5}" class="dgaxis"/>`;
    return out + "</svg>";
  }

  function hbars(c) {
    const max = niceMax(Math.max(0, ...c.series.flatMap((s) => s.values)));
    const s = c.series.length;
    const labelW = Math.min(220, CHART_W * 0.28);
    const trackX = labelW + 10;
    const valueW = 56;
    const trackW = CHART_W - trackX - 8 - valueW;
    const barH = s > 1 ? 8 : 12;
    const row = 14;
    const groupH = s * row + 4 * (s - 1);
    const height = c.labels.length * groupH + 8 * (c.labels.length - 1);
    let out = svgOpen(CHART_W, height, "dghbars");
    c.labels.forEach((label, g) => {
      const gy = g * (groupH + 8);
      out += textEl(labelW, gy + groupH / 2, fit(label, labelW, 12.5), "dglabel", "end");
      c.series.forEach((series, si) => {
        const v = Math.max(0, series.values[g] ?? 0);
        const cy = gy + si * (row + 4) + row / 2;
        out += `<rect x="${r1(trackX)}" y="${r1(cy - barH / 2)}" width="${r1(trackW)}" height="${barH}" rx="4" class="dgtrack"/>`;
        out += `<rect x="${r1(trackX)}" y="${r1(cy - barH / 2)}" width="${r1(Math.max(2, (v / max) * trackW))}" height="${barH}" rx="4" fill="${color(si)}"/>`;
        out += textEl(trackX + trackW + 8, cy, fmtValue(c, v), "dgt", "start");
      });
    });
    return out + "</svg>";
  }

  const LINE_H = 170;
  const INSET = 6;

  function linesChart(c, area) {
    const all = c.series.flatMap((s) => s.values);
    let lo = Math.min(...all);
    let hi = Math.max(...all);
    lo = lo >= 0 ? 0 : -niceMax(-lo);
    hi = niceMax(Math.max(hi, lo + 1e-9));
    const n = c.labels.length;
    const x0 = 52;
    const pw = CHART_W - x0;
    const h = LINE_H - 2 * INSET;
    const yOf = (v) => INSET + h - ((v - lo) / (hi - lo)) * h;
    const xOf = (i) => x0 + (n <= 1 ? pw / 2 : 4 + (i * (pw - 8)) / (n - 1));
    const fill = area || c.series.length === 1;
    const height = LINE_H + 22;
    let out = svgOpen(CHART_W, height, "dglines");
    for (let k = 0; k < 5; k++) {
      const y = INSET + (h * k) / 4;
      out += `<line x1="${x0}" y1="${r1(y)}" x2="${CHART_W}" y2="${r1(y)}" class="${k === 4 ? "dgaxis" : "dggrid"}"/>`;
      out += textEl(44, y, fmtValue(c, lo + ((hi - lo) * (4 - k)) / 4), "dgt", "end");
    }
    c.series.forEach((s, si) => {
      const pts = s.values.map((v, i) => [xOf(i), yOf(v)]);
      const col = color(si);
      if (pts.length >= 2 && fill) {
        const base = yOf(Math.max(lo, 0));
        const d = `M${pt([pts[0][0], base])}` + pts.map((q) => `L${pt(q)}`).join("") + `L${pt([pts[pts.length - 1][0], base])}Z`;
        out += `<path d="${d}" fill="${col}" fill-opacity="${area ? 0.18 : 0.08}"/>`;
      }
      if (pts.length >= 2) {
        out += `<path d="M${pts.map(pt).join("L")}" fill="none" stroke="${col}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>`;
      }
      if (n <= 40 || pts.length === 1) {
        for (const q of pts) out += `<circle cx="${r1(q[0])}" cy="${r1(q[1])}" r="3" class="dgdot" stroke="${col}" stroke-width="1.75"/>`;
      }
    });
    // Up to eight labels, as on the desktop, when they fit the narrower
    // column; else the first, middle and last.
    let picks = [...Array(n).keys()];
    if (n > 8 || (n > 3 && c.labels.some((l) => smallW(l) > pw / n))) picks = [0, Math.floor(n / 2), n - 1];
    const each = pw / picks.length;
    picks.forEach((i, k) => {
      const anchor = picks.length === 1 ? "middle" : k === 0 ? "start" : k === picks.length - 1 ? "end" : "middle";
      const x = anchor === "start" ? x0 : anchor === "end" ? CHART_W : xOf(i);
      out += textEl(x, LINE_H + 12, fit(c.labels[i], each), "dgt", anchor);
    });
    return out + "</svg>";
  }

  function pie(c) {
    const values = c.series[0].values.map((v) => Math.max(0, v));
    const total = values.reduce((a, v) => a + v, 0);
    const D = 150;
    const [cx, cy, ro, ri] = [D / 2, D / 2, D / 2, D / 2 - 26];
    const at = (r, a) => [cx + r * Math.cos(a), cy + r * Math.sin(a)];
    let svg = svgOpen(D, D, "dgpie");
    if (total > 0) {
      let start = -Math.PI / 2;
      values.forEach((v, i) => {
        const sweep = (v / total) * Math.PI * 2;
        if (sweep <= 0) return;
        if (sweep >= Math.PI * 2 - 1e-6) {
          // A whole ring: an arc that ends where it starts draws nothing.
          svg += `<circle cx="${cx}" cy="${cy}" r="${(ro + ri) / 2}" fill="none" stroke="${color(i)}" stroke-width="${ro - ri}"/>`;
        } else {
          const gap = values.length > 1 ? 0.02 : 0;
          const [a0, a1] = [start + gap, start + sweep - gap];
          const large = a1 - a0 > Math.PI ? 1 : 0;
          svg += `<path d="M${pt(at(ro, a0))}A${ro} ${ro} 0 ${large} 1 ${pt(at(ro, a1))}L${pt(at(ri, a1))}A${ri} ${ri} 0 ${large} 0 ${pt(at(ri, a0))}Z" fill="${color(i)}"/>`;
        }
        start += sweep;
      });
    }
    svg += textEl(cx, cy - 7, fmtValue(c, total), "dgtotal") + textEl(cx, cy + 12, "total", "dgt") + "</svg>";
    const rows = c.labels
      .map((label, i) => {
        const v = values[i] ?? 0;
        const pct = total > 0 ? (v / total) * 100 : 0;
        return `<div class="dgrow"><i style="background:${color(i)}"></i><span class="dgname">${esc(label)}</span><span class="dgval">${esc(fmtValue(c, v))}</span><span class="dgpct">${pct.toFixed(0)}%</span></div>`;
      })
      .join("");
    return `<div class="dgpiewrap">${svg}<div class="dglist">${rows}</div></div>`;
  }

  function flowSvg(f) {
    let out = svgOpen(f.width, f.height, "dgflow", ` style="--dgw:${f.width}px"`);
    f.edges.forEach((e, i) => {
      const c = f.curves[i];
      // End the stroke at the arrow's base so its tip stays sharp.
      let [dx, dy] = [c.to[0] - c.c2[0], c.to[1] - c.c2[1]];
      if (Math.abs(dx) + Math.abs(dy) < 0.01) [dx, dy] = [c.to[0] - c.from[0], c.to[1] - c.from[1]];
      const len = Math.max(Math.hypot(dx, dy), 0.01);
      const [ux, uy] = [dx / len, dy / len];
      const tip = 8;
      const end = e.arrow ? [c.to[0] - ux * tip * 0.8, c.to[1] - uy * tip * 0.8] : c.to;
      const cls = `dgedge${e.thick ? " thick" : ""}${e.dotted ? " dotted" : ""}`;
      out += `<path d="M${pt(c.from)}C${pt(c.c1)} ${pt(c.c2)} ${pt(end)}" class="${cls}"/>`;
      if (e.arrow) {
        const base = [c.to[0] - ux * tip, c.to[1] - uy * tip];
        const [qx, qy] = [-uy * tip * 0.5, ux * tip * 0.5];
        out += `<path d="M${pt(c.to)}L${pt([base[0] + qx, base[1] + qy])}L${pt([base[0] - qx, base[1] - qy])}Z" class="dgarrow"/>`;
      }
    });
    f.nodes.forEach((n, i) => {
      const p = f.placed[i];
      if (n.shape === "diamond" || n.shape === "hex") {
        const [x0, y0, x1, y1] = [p.x, p.y, p.x + p.w, p.y + p.h];
        const my = (y0 + y1) / 2;
        const k = 14;
        const pts =
          n.shape === "diamond"
            ? [[(x0 + x1) / 2, y0], [x1, my], [(x0 + x1) / 2, y1], [x0, my]]
            : [[x0 + k, y0], [x1 - k, y0], [x1, my], [x1 - k, y1], [x0 + k, y1], [x0, my]];
        out += `<path d="M${pts.map(pt).join("L")}Z" class="dgnode"/>`;
      } else {
        // The design rounds plain boxes a little more than the desktop's 6.
        const r = n.shape === "rect" ? 10 : n.shape === "round" ? 14 : p.h / 2;
        out += `<rect x="${r1(p.x + 0.5)}" y="${r1(p.y + 0.5)}" width="${p.w - 1}" height="${p.h - 1}" rx="${r1(Math.min(r, p.h / 2))}" class="dgnode"/>`;
      }
      const ls = f.lines[i];
      const y0 = p.y + (p.h - NODE_LINE_H * ls.length) / 2;
      ls.forEach((l, k) => (out += textEl(p.x + p.w / 2, y0 + NODE_LINE_H * k + NODE_LINE_H / 2, l, "dgnl")));
    });
    f.edges.forEach((e, i) => {
      if (!e.label) return;
      const [mx, my] = bezier(f.curves[i], 0.5);
      const b = edgeLabelBox(e.label);
      out += `<rect x="${r1(mx - b.w / 2)}" y="${r1(my - b.h / 2)}" width="${b.w}" height="${b.h}" rx="4" class="dgpill"/>`;
      const top = my - b.h / 2 + 2;
      b.lines.forEach((l, k) => (out += textEl(mx, top + EDGE_LINE_H * k + EDGE_LINE_H / 2, l, "dgel")));
    });
    return out + "</svg>";
  }

  const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;

  const ICON_CODE =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M16 18l6-6-6-6M8 6l-6 6 6 6"/></svg>';
  const ICON_OPEN =
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7"/></svg>';

  // The drawing in its card: the picture, the source behind "Code", and a
  // footer with a caption and the two buttons. The page handles the taps.
  function card(kind, view, caption, source, title) {
    const foot =
      `<div class="dgfoot"><span class="dgcap">${esc(caption)}</span><span class="dgacts">` +
      `<button type="button" class="dgbtn dgcode" aria-pressed="false" data-back="${kind}">${ICON_CODE}<span>Code</span></button>` +
      `<button type="button" class="dgbtn dgopen" aria-label="Open the ${kind.toLowerCase()}">${ICON_OPEN}<span>Open</span></button></span></div>`;
    return `<div class="diagram" data-title="${esc(title)}"><div class="dgview dg${kind.toLowerCase()}">${view}</div><pre class="dgsrc"><code>${esc(source)}</code></pre>${foot}</div>`;
  }

  function renderChart(c, source) {
    const groups = c.labels.length;
    const longest = Math.max(0, ...c.labels.map((l) => Array.from(l).length));
    // As on the desktop, long or many labels turn bars sideways; a phone column
    // holds fewer groups, so more than eight do too.
    let kind = c.kind;
    if (kind === "bar" && (longest > 10 || groups > 8 || groups * c.series.length > 36)) kind = "hbar";
    const body = kind === "bar" ? bars(c) : kind === "hbar" ? hbars(c) : kind === "pie" ? pie(c) : linesChart(c, kind === "area");
    const head =
      c.title || c.unit
        ? `<div class="dghead"><span class="dgtitle">${esc(c.title || "")}</span><span class="dgunit">${esc(c.unit)}</span></div>`
        : "";
    const legend =
      c.series.length > 1 && kind !== "pie"
        ? `<div class="dglegend">${c.series.map((s, i) => `<span><i style="background:${color(i)}"></i>${esc(s.name)}</span>`).join("")}</div>`
        : "";
    const word = kind === "pie" ? "slice" : kind === "line" || kind === "area" ? "point" : "group";
    return card("Chart", `${head}${legend}<div class="dgbody">${body}</div>`, `Chart · ${plural(groups, word)}`, source, c.title || "Chart");
  }

  function renderFlow(f, source) {
    return card("Diagram", `<div class="dgbody">${flowSvg(f)}</div>`, `Diagram · ${plural(f.nodes.length, "step")}`, source, "Diagram");
  }

  // The card for a fenced block, as HTML, or null when it is not a drawing.
  function render(lang, text) {
    const d = parse(lang, text);
    if (!d) return null;
    return d.type === "flow" ? renderFlow(d.flow, String(text)) : renderChart(d.chart, String(text));
  }

  const api = { render, parse, fmtNum, fmtValue, niceMax, numberWithUnit, textW, wrapLabel, edgeLabelBox, FIT_W };
  if (typeof window !== "undefined") window.Diagrams = api;
  if (typeof module !== "undefined") module.exports = api;
})();
