//! Diagrams drawn natively from fenced blocks in agent replies:
//! ```` ```chart ```` (JSON, or `label: value` lines) and ```` ```mermaid ````
//! (flowcharts and pies). Anything else stays a code block.

use crate::ui::Typography as _;
use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, BorderStyle, Bounds, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, PathBuilder, Pixels, Point, StatefulInteractiveElement as _, Styled as _,
    Window, canvas, div, point, px, quad, relative, size,
};
use serde_json::Value;

use super::markdown::copy_button;
use crate::theme;
use crate::ui::icon;

fn color(i: usize) -> Hsla {
    // Blue, violet, green, amber, rose, cyan, coral, sand: neighbors far apart.
    const ORDER: [usize; 8] = [0, 3, 2, 1, 6, 5, 4, 7];
    gpui::rgb(theme::CATEGORY[ORDER[i % ORDER.len()]]).into()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// Vertical bars, or horizontal ones when the labels are long or many.
    Bar,
    HBar,
    Line,
    Area,
    Pie,
}

#[derive(Debug, Clone)]
pub struct Series {
    pub name: String,
    pub values: Vec<f64>,
}

#[derive(Debug, Clone)]
pub struct Chart {
    pub kind: Kind,
    pub title: Option<String>,
    pub labels: Vec<String>,
    pub series: Vec<Series>,
    pub unit: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Rect,
    Round,
    Stadium,
    Circle,
    Diamond,
    Hex,
}

#[derive(Debug, Clone)]
pub struct FlowNode {
    pub id: String,
    pub label: String,
    pub shape: Shape,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlowEdge {
    pub from: usize,
    pub to: usize,
    pub label: Option<String>,
    pub arrow: bool,
    pub dotted: bool,
    pub thick: bool,
}

/// A node's box, top-left origin, in diagram coordinates.
#[derive(Debug, Clone, Copy)]
struct Placed {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// An edge as a cubic curve.
#[derive(Debug, Clone, Copy)]
struct Curve {
    from: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    to: (f32, f32),
}

impl Curve {
    fn at(&self, t: f32) -> (f32, f32) {
        let u = 1. - t;
        let (k0, k1, k2, k3) = (u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t);
        let f = |a: f32, b: f32, c: f32, d: f32| k0 * a + k1 * b + k2 * c + k3 * d;
        (
            f(self.from.0, self.c1.0, self.c2.0, self.to.0),
            f(self.from.1, self.c1.1, self.c2.1, self.to.1),
        )
    }

    fn mid(&self) -> (f32, f32) {
        self.at(0.5)
    }
}

#[derive(Debug, Clone)]
pub struct Flow {
    pub horizontal: bool,
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
    /// Each node's label wrapped to the lines its box was sized for.
    lines: Vec<Vec<String>>,
    placed: Vec<Placed>,
    curves: Vec<Curve>,
    width: f32,
    height: f32,
}

pub enum Diagram {
    Chart(Chart),
    Flow(Flow),
}

/// The diagram a fenced block draws, if its language is one and it parses.
pub fn parse(lang: &str, text: &str) -> Option<Diagram> {
    match lang.to_ascii_lowercase().as_str() {
        "chart" => parse_chart(text).map(Diagram::Chart),
        "mermaid" => parse_mermaid(text),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Charts

fn parse_chart(text: &str) -> Option<Chart> {
    let chart = match serde_json::from_str::<Value>(text.trim()) {
        Ok(v) => chart_from_json(&v)?,
        Err(_) => chart_from_lines(text)?,
    };
    let n = chart.series.iter().map(|s| s.values.len()).max().unwrap_or(0);
    (n > 0 && chart.values().all(f64::is_finite)).then_some(chart)
}

impl Chart {
    fn values(&self) -> impl Iterator<Item = f64> + '_ {
        self.series.iter().flat_map(|s| s.values.iter().copied())
    }

    fn fmt(&self, v: f64) -> String {
        let n = fmt_num(v);
        match self.unit.as_str() {
            "" => n,
            "$" | "€" | "£" => format!("{}{n}", self.unit),
            u if u.len() <= 2 => format!("{n}{u}"),
            u => format!("{n} {u}"),
        }
    }
}

fn kind_of(s: &str) -> Kind {
    match s.trim().to_ascii_lowercase().as_str() {
        "line" => Kind::Line,
        "area" => Kind::Area,
        "pie" | "donut" | "doughnut" => Kind::Pie,
        "hbar" | "barh" | "horizontal" | "row" => Kind::HBar,
        _ => Kind::Bar,
    }
}

/// A number and whatever unit trails it: `12.5ms` -> (12.5, "ms").
fn number_with_unit(s: &str) -> Option<(f64, String)> {
    let s = s.trim().trim_matches('"');
    let (prefix, s) = match s.strip_prefix('$') {
        Some(rest) => ("$", rest),
        None => ("", s),
    };
    let end = s
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || c == ',' || (i == 0 && c == '-')))
        .map_or(s.len(), |(i, _)| i);
    let n: f64 = s[..end].replace(',', "").parse().ok()?;
    let unit = if prefix.is_empty() { s[end..].trim().to_string() } else { prefix.to_string() };
    Some((n, unit))
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().or_else(|| number_with_unit(v.as_str()?).map(|(n, _)| n))
}

fn label_of(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        x => x.to_string(),
    }
}

fn chart_from_json(v: &Value) -> Option<Chart> {
    let kind = kind_of(v["type"].as_str().or(v["kind"].as_str()).unwrap_or("bar"));
    let title = v["title"].as_str().map(str::to_string);
    let unit = v["unit"].as_str().unwrap_or("").to_string();
    let mut labels: Vec<String> = v["labels"]
        .as_array()
        .map(|a| a.iter().map(label_of).collect())
        .unwrap_or_default();
    let mut series = Vec::new();
    if let Some(arr) = v["series"].as_array().or(v["datasets"].as_array()) {
        for (i, s) in arr.iter().enumerate() {
            let name = s["name"]
                .as_str()
                .or(s["label"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("Series {}", i + 1));
            let values = s["data"]
                .as_array()
                .or(s["values"].as_array())
                .map(|a| a.iter().map(|x| num(x).unwrap_or(0.)).collect())
                .unwrap_or_default();
            series.push(Series { name, values });
        }
    } else {
        let values = match &v["data"] {
            Value::Array(a) if !a.is_empty() && a.iter().all(Value::is_object) => {
                labels = a
                    .iter()
                    .map(|x| label_of(x.get("label").or(x.get("name")).or(x.get("x")).unwrap_or(&Value::Null)))
                    .collect();
                a.iter()
                    .map(|x| x.get("value").or(x.get("y")).and_then(num).unwrap_or(0.))
                    .collect()
            }
            Value::Array(a) => a.iter().map(|x| num(x).unwrap_or(0.)).collect(),
            Value::Object(m) => {
                labels = m.keys().cloned().collect();
                m.values().map(|x| num(x).unwrap_or(0.)).collect()
            }
            _ => return None,
        };
        series.push(Series {
            name: String::new(),
            values,
        });
    }
    let n = series.iter().map(|s| s.values.len()).max().unwrap_or(0);
    labels.truncate(n);
    while labels.len() < n {
        labels.push((labels.len() + 1).to_string());
    }
    Some(Chart {
        kind,
        title,
        labels,
        series,
        unit,
    })
}

/// `type: line`, `title: ...`, `unit: ms`, then one `label: value` per line.
fn chart_from_lines(text: &str) -> Option<Chart> {
    let mut chart = Chart {
        kind: Kind::Bar,
        title: None,
        labels: Vec::new(),
        series: vec![Series {
            name: String::new(),
            values: Vec::new(),
        }],
        unit: String::new(),
    };
    for line in text.lines() {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            match key.trim().to_ascii_lowercase().as_str() {
                "type" | "kind" => {
                    chart.kind = kind_of(value);
                    continue;
                }
                "title" => {
                    chart.title = Some(value.trim().trim_matches('"').to_string());
                    continue;
                }
                "unit" => {
                    chart.unit = value.trim().to_string();
                    continue;
                }
                _ => {}
            }
        }
        let (label, value) = line.rsplit_once(':').or_else(|| line.rsplit_once('='))?;
        let (n, unit) = number_with_unit(value)?;
        if chart.unit.is_empty() {
            chart.unit = unit;
        }
        chart.labels.push(label.trim().trim_matches('"').to_string());
        chart.series[0].values.push(n);
    }
    Some(chart)
}

fn fmt_num(v: f64) -> String {
    let a = v.abs();
    let (x, suffix) = if a >= 1e9 {
        (v / 1e9, "B")
    } else if a >= 1e6 {
        (v / 1e6, "M")
    } else if a >= 1e4 {
        (v / 1e3, "k")
    } else {
        (v, "")
    };
    let s = if (x - x.round()).abs() < 1e-9 {
        format!("{}", x.round() as i64)
    } else if x.abs() >= 100. {
        format!("{x:.0}")
    } else if x.abs() >= 10. {
        format!("{x:.1}")
    } else {
        format!("{x:.2}")
    };
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    s + suffix
}

/// The round number at or above `v` that an axis can end on.
fn nice_max(v: f64) -> f64 {
    if v <= 0. {
        return 1.;
    }
    let pow = 10f64.powf(v.log10().floor());
    [1., 2., 2.5, 5., 10.]
        .iter()
        .map(|m| m * pow)
        .find(|&m| m >= v - 1e-9)
        .unwrap_or(10. * pow)
}

// ---------------------------------------------------------------------------
// Mermaid

fn parse_mermaid(text: &str) -> Option<Diagram> {
    let statements = statements(text);
    let head = statements.first()?;
    let mut words = head.split_whitespace();
    match words.next()? {
        "pie" => parse_pie(&statements).map(Diagram::Chart),
        "graph" | "flowchart" => {
            let dir = words.next().unwrap_or("TD").to_ascii_uppercase();
            let flow = parse_flow(matches!(dir.as_str(), "LR" | "RL"), &statements[1..])?;
            Some(Diagram::Flow(flow))
        }
        _ => None,
    }
}

/// Lines split at `;` outside brackets and quotes, comments dropped.
fn statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        let (mut depth, mut quoted, mut cur) = (0i32, false, String::new());
        for c in line.chars() {
            match c {
                '"' => quoted = !quoted,
                '[' | '(' | '{' if !quoted => depth += 1,
                ']' | ')' | '}' if !quoted => depth -= 1,
                ';' if !quoted && depth <= 0 => {
                    out.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            }
            cur.push(c);
        }
        out.push(cur);
    }
    out.into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_pie(statements: &[String]) -> Option<Chart> {
    let mut chart = Chart {
        kind: Kind::Pie,
        title: None,
        labels: Vec::new(),
        series: vec![Series {
            name: String::new(),
            values: Vec::new(),
        }],
        unit: String::new(),
    };
    for (i, s) in statements.iter().enumerate() {
        let s = if i == 0 { s.trim_start_matches("pie").trim() } else { s.as_str() };
        let s = s.trim_start_matches("showData").trim();
        if let Some(t) = s.strip_prefix("title") {
            chart.title = Some(t.trim().to_string());
        } else if let Some((label, value)) = s.rsplit_once(':') {
            chart.labels.push(label.trim().trim_matches('"').to_string());
            chart.series[0].values.push(value.trim().parse().ok()?);
        }
    }
    (!chart.labels.is_empty()).then_some(chart)
}

const SKIP: &[&str] = &[
    "subgraph", "end", "classDef", "class", "style", "linkStyle", "click", "direction",
    "accTitle", "accDescr",
];

#[derive(Default)]
struct FlowBuilder {
    nodes: Vec<FlowNode>,
    index: HashMap<String, usize>,
    edges: Vec<FlowEdge>,
}

struct EdgeStyle {
    label: Option<String>,
    arrow: bool,
    dotted: bool,
    thick: bool,
}

fn is_id(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn skip_ws(s: &[char], i: &mut usize) {
    while *i < s.len() && s[*i].is_whitespace() {
        *i += 1;
    }
}

fn starts(s: &[char], i: usize, pat: &str) -> bool {
    pat.chars().enumerate().all(|(k, c)| s.get(i + k) == Some(&c))
}

fn clean_label(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(s);
    let s = s.strip_prefix('`').and_then(|s| s.strip_suffix('`')).unwrap_or(s);
    s.replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

impl FlowBuilder {
    fn node(&mut self, id: &str, label: Option<(String, Shape)>) -> usize {
        let ix = *self.index.entry(id.to_string()).or_insert_with(|| {
            self.nodes.push(FlowNode {
                id: id.to_string(),
                label: id.to_string(),
                shape: Shape::Rect,
            });
            self.nodes.len() - 1
        });
        if let Some((label, shape)) = label {
            self.nodes[ix].label = label;
            self.nodes[ix].shape = shape;
        }
        ix
    }

    fn node_ref(&mut self, s: &[char], i: &mut usize) -> Option<usize> {
        let start = *i;
        while *i < s.len() {
            let c = s[*i];
            let joins = (c == '-' || c == '.') && s.get(*i + 1).is_some_and(|&n| is_id(n));
            if is_id(c) || (joins && *i > start) {
                *i += 1;
            } else {
                break;
            }
        }
        if *i == start {
            return None;
        }
        let id: String = s[start..*i].iter().collect();
        const SHAPES: &[(&str, &str, Shape)] = &[
            ("(((", ")))", Shape::Circle),
            ("((", "))", Shape::Circle),
            ("([", "])", Shape::Stadium),
            ("[[", "]]", Shape::Rect),
            ("[(", ")]", Shape::Round),
            ("[/", "/]", Shape::Rect),
            ("[\\", "\\]", Shape::Rect),
            ("{{", "}}", Shape::Hex),
            ("[", "]", Shape::Rect),
            ("(", ")", Shape::Round),
            ("{", "}", Shape::Diamond),
            (">", "]", Shape::Rect),
        ];
        let mut label = None;
        for (open, close, shape) in SHAPES {
            if !starts(s, *i, open) {
                continue;
            }
            let from = *i + open.chars().count();
            let mut j = from;
            let mut quoted = false;
            while j < s.len() && (quoted || !starts(s, j, close)) {
                if s[j] == '"' {
                    quoted = !quoted;
                }
                j += 1;
            }
            if j >= s.len() {
                return None;
            }
            label = Some((clean_label(&s[from..j].iter().collect::<String>()), *shape));
            *i = j + close.chars().count();
            break;
        }
        if starts(s, *i, ":::") {
            *i += 3;
            while *i < s.len() && (is_id(s[*i]) || s[*i] == '-') {
                *i += 1;
            }
        }
        Some(self.node(&id, label))
    }

    /// `A`, or `A & B & C`.
    fn group(&mut self, s: &[char], i: &mut usize) -> Option<Vec<usize>> {
        let mut out = vec![self.node_ref(s, i)?];
        loop {
            let mut j = *i;
            skip_ws(s, &mut j);
            if s.get(j) != Some(&'&') {
                return Some(out);
            }
            j += 1;
            skip_ws(s, &mut j);
            *i = j;
            out.push(self.node_ref(s, i)?);
        }
    }

    fn statement(&mut self, line: &str) -> Option<()> {
        let s: Vec<char> = line.chars().collect();
        let mut i = 0;
        let mut prev: Option<(Vec<usize>, EdgeStyle)> = None;
        loop {
            skip_ws(&s, &mut i);
            let group = self.group(&s, &mut i)?;
            if let Some((from, e)) = prev.take() {
                for &a in &from {
                    for &b in &group {
                        self.edges.push(FlowEdge {
                            from: a,
                            to: b,
                            label: e.label.clone(),
                            arrow: e.arrow,
                            dotted: e.dotted,
                            thick: e.thick,
                        });
                    }
                }
            }
            skip_ws(&s, &mut i);
            if i >= s.len() {
                return Some(());
            }
            prev = Some((group, edge_op(&s, &mut i)?));
        }
    }
}

fn is_edge(c: char) -> bool {
    "-=.<>~".contains(c)
}

fn edge_run(s: &[char], i: &mut usize) -> String {
    let start = *i;
    while *i < s.len() && is_edge(s[*i]) {
        *i += 1;
    }
    let mut op: String = s[start..*i].iter().collect();
    // `--o` and `--x` end in a circle or cross; drawn as arrows.
    if matches!(s.get(*i), Some('o' | 'x'))
        && s.get(*i + 1).is_none_or(|c| c.is_whitespace())
        && op.ends_with(['-', '='])
    {
        *i += 1;
        op.push('>');
    }
    op
}

fn edge_op(s: &[char], i: &mut usize) -> Option<EdgeStyle> {
    let mut op = edge_run(s, i);
    if op.len() < 2 {
        return None;
    }
    let mut label = None;
    // `-- text -->`, `== text ==>`, `-. text .->`
    if matches!(op.as_str(), "--" | "==" | "-.") {
        let mut j = *i;
        while j < s.len() {
            if is_edge(s[j]) && s.get(j + 1).is_some_and(|&c| is_edge(c)) {
                break;
            }
            j += 1;
        }
        if j >= s.len() {
            return None;
        }
        label = Some(clean_label(&s[*i..j].iter().collect::<String>()));
        *i = j;
        op.push_str(&edge_run(s, i));
    }
    skip_ws(s, i);
    if s.get(*i) == Some(&'|') {
        let from = *i + 1;
        let mut j = from;
        while j < s.len() && s[j] != '|' {
            j += 1;
        }
        label = Some(clean_label(&s[from..j.min(s.len())].iter().collect::<String>()));
        *i = (j + 1).min(s.len());
    }
    Some(EdgeStyle {
        label: label.filter(|l| !l.is_empty()),
        arrow: op.contains('>'),
        dotted: op.contains('.'),
        thick: op.contains('='),
    })
}

fn parse_flow(horizontal: bool, statements: &[String]) -> Option<Flow> {
    let mut b = FlowBuilder::default();
    for s in statements {
        let first = s.split_whitespace().next().unwrap_or("");
        if SKIP.contains(&first) {
            continue;
        }
        b.statement(s)?;
    }
    if b.nodes.is_empty() || b.nodes.len() > 120 {
        return None;
    }
    let (lines, sizes): (Vec<_>, Vec<_>) = b.nodes.iter().map(node_box).unzip();
    let (rank, layers) = rank_nodes(b.nodes.len(), &b.edges);
    let mut horizontal = horizontal;
    let mut laid = place(&b.edges, &sizes, &rank, &layers, horizontal);
    if laid.width > FIT_W {
        let other = place(&b.edges, &sizes, &rank, &layers, !horizontal);
        if other.width < laid.width {
            laid = other;
            horizontal = !horizontal;
        }
    }
    Some(Flow {
        horizontal,
        nodes: b.nodes,
        edges: b.edges,
        lines,
        placed: laid.placed,
        curves: laid.curves,
        width: laid.width,
        height: laid.height,
    })
}
const PAD: f32 = 12.;
const NODE_LINE_H: f32 = 18.;
const EDGE_LINE_H: f32 = 14.;
/// Wider than this, a flowchart is laid out in the other direction when that
/// one is narrower: a reply column holds about this much.
const FIT_W: f32 = 820.;

/// Roughly how wide `s` is at the node font size, in pixels.
fn text_w(s: &str) -> f32 {
    s.chars()
        .map(|c| match c {
            ' ' => 3.6,
            'i' | 'l' | 'j' | 'I' | '.' | ',' | ':' | ';' | '\'' | '|' | '!' | '(' | ')' => 4.2,
            't' | 'f' | 'r' => 5.,
            'm' | 'w' | 'M' | 'W' => 10.5,
            c if c.is_uppercase() => 8.6,
            c if c.is_ascii_digit() => 7.4,
            _ => 7.,
        })
        .sum()
}

/// An edge label as drawn, one string per line, and its pill's size. The
/// text is smaller than a node's, so lines wrap at a node-scale 182 px to
/// keep the pill within 180.
fn edge_label_box(label: &str) -> (Vec<String>, (f32, f32)) {
    let lines = wrap_label(label, 182.);
    let tw = lines.iter().map(|l| text_w(l)).fold(0., f32::max);
    let w = (tw * 0.9 + 16.).min(180.).round();
    let h = EDGE_LINE_H * lines.len() as f32 + 4.;
    (lines, (w, h))
}

/// Greedy word wrap to `max` pixels per line, keeping the label's own breaks.
/// A word longer than a line gets a line of its own.
fn wrap_label(label: &str, max: f32) -> Vec<String> {
    let mut out = Vec::new();
    for para in label.lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && text_w(&line) + text_w(" ") + text_w(word) > max {
                out.push(std::mem::take(&mut line));
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// A node's label as drawn, one string per line, and the box that holds it.
fn node_box(n: &FlowNode) -> (Vec<String>, (f32, f32)) {
    let max = match n.shape {
        Shape::Diamond => 120.,
        Shape::Circle => 110.,
        _ => 168.,
    };
    let lines = wrap_label(&n.label, max);
    let tw = lines.iter().map(|l| text_w(l)).fold(0., f32::max);
    let th = NODE_LINE_H * lines.len() as f32;
    let (w, h) = match n.shape {
        // A rhombus holds a tw × th box when tw/w + th/h ≤ 1; keep some room.
        Shape::Diamond => ((tw / 0.58).max(72.), (th / 0.36).max(56.)),
        Shape::Circle => {
            let d = (tw.hypot(th) + 16.).max(56.);
            (d, d)
        }
        Shape::Hex => (tw + 52., th + 18.),
        Shape::Stadium => (tw + th + 32., th + 18.),
        _ => ((tw + 28.).max(72.), th + 18.),
    };
    (lines, (w.round(), h.round()))
}

/// Ranks by longest path over the edges that do not close a cycle, and an
/// order inside each rank from two barycenter sweeps.
fn rank_nodes(n: usize, edges: &[FlowEdge]) -> (Vec<usize>, Vec<Vec<usize>>) {
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        if e.from != e.to {
            out[e.from].push(e.to);
        }
    }
    // Edges into a node still on the DFS stack close a cycle; leave them out.
    let mut state = vec![0u8; n];
    let mut forward: Vec<Vec<usize>> = vec![Vec::new(); n];
    for root in 0..n {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state[root] = 1;
        while let Some(&mut (v, ref mut k)) = stack.last_mut() {
            if let Some(&w) = out[v].get(*k) {
                *k += 1;
                match state[w] {
                    0 => {
                        forward[v].push(w);
                        state[w] = 1;
                        stack.push((w, 0));
                    }
                    2 => forward[v].push(w),
                    _ => {}
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    let mut indeg = vec![0usize; n];
    for &w in forward.iter().flatten() {
        indeg[w] += 1;
    }
    let mut rank = vec![0usize; n];
    let mut queue: Vec<usize> = (0..n).filter(|&v| indeg[v] == 0).collect();
    let mut head = 0;
    while head < queue.len() {
        let v = queue[head];
        head += 1;
        for &w in &forward[v] {
            rank[w] = rank[w].max(rank[v] + 1);
            indeg[w] -= 1;
            if indeg[w] == 0 {
                queue.push(w);
            }
        }
    }
    let ranks = rank.iter().max().map_or(0, |m| m + 1);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); ranks];
    for v in 0..n {
        layers[rank[v]].push(v);
    }
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (v, out) in forward.iter().enumerate() {
        for &w in out {
            preds[w].push(v);
        }
    }
    let mut pos = vec![0f32; n];
    let index = |layers: &Vec<Vec<usize>>, pos: &mut Vec<f32>| {
        for layer in layers {
            for (i, &v) in layer.iter().enumerate() {
                pos[v] = i as f32;
            }
        }
    };
    index(&layers, &mut pos);
    let mean = |vs: &[usize], pos: &[f32], own: f32| {
        if vs.is_empty() { own } else { vs.iter().map(|&u| pos[u]).sum::<f32>() / vs.len() as f32 }
    };
    for _ in 0..2 {
        for r in 1..ranks {
            let mut sorted: Vec<(usize, f32)> = layers[r].iter().map(|&v| (v, mean(&preds[v], &pos, pos[v]))).collect();
            sorted.sort_by(|a, b| a.1.total_cmp(&b.1));
            layers[r] = sorted.into_iter().map(|(v, _)| v).collect();
            index(&layers, &mut pos);
        }
        for r in (0..ranks.saturating_sub(1)).rev() {
            let mut sorted: Vec<(usize, f32)> = layers[r].iter().map(|&v| (v, mean(&forward[v], &pos, pos[v]))).collect();
            sorted.sort_by(|a, b| a.1.total_cmp(&b.1));
            layers[r] = sorted.into_iter().map(|(v, _)| v).collect();
            index(&layers, &mut pos);
        }
    }
    (rank, layers)
}

/// Node boxes and edge curves for one direction.
struct Laid {
    placed: Vec<Placed>,
    curves: Vec<Curve>,
    width: f32,
    height: f32,
}

fn place(edges: &[FlowEdge], sizes: &[(f32, f32)], rank: &[usize], layers: &[Vec<usize>], h: bool) -> Laid {
    let n = sizes.len();
    // Along = the direction ranks advance in; across = inside a rank.
    let along = |s: (f32, f32)| if h { s.0 } else { s.1 };
    let across = |s: (f32, f32)| if h { s.1 } else { s.0 };
    let node_gap = if h { 24. } else { 32. };
    // A labeled edge between neighboring ranks needs room for its pill.
    let mut gaps: Vec<f32> = vec![if h { 64. } else { 52. }; layers.len()];
    for e in edges {
        if let Some(l) = &e.label
            && rank[e.to] == rank[e.from] + 1
        {
            let (_, (w, lh)) = edge_label_box(l);
            let need = if h { w + 36. } else { lh + 36. };
            gaps[rank[e.from]] = gaps[rank[e.from]].max(need);
        }
    }
    let spans: Vec<f32> = layers
        .iter()
        .map(|l| l.iter().map(|&v| across(sizes[v])).sum::<f32>() + node_gap * l.len().saturating_sub(1) as f32)
        .collect();
    let widest = spans.iter().cloned().fold(0., f32::max);
    let mut placed = vec![Placed { x: 0., y: 0., w: 0., h: 0. }; n];
    let mut cursor = 0.;
    for (r, layer) in layers.iter().enumerate() {
        let band = layer.iter().map(|&v| along(sizes[v])).fold(0., f32::max);
        let mut x = (widest - spans[r]) / 2.;
        for &v in layer {
            let s = sizes[v];
            let a = cursor + (band - along(s)) / 2.;
            placed[v] = if h {
                Placed { x: a, y: x, w: s.0, h: s.1 }
            } else {
                Placed { x, y: a, w: s.0, h: s.1 }
            };
            x += across(s) + node_gap;
        }
        cursor += band + gaps[r];
    }
    let slot = |v: usize| {
        let layer = &layers[rank[v]];
        let i = layer.iter().position(|&u| u == v).unwrap_or(0);
        (i, layer.len() - 1 - i)
    };

    let mut curves = Vec::with_capacity(edges.len());
    for e in edges {
        let (a, b) = (placed[e.from], placed[e.to]);
        let curve = if e.from == e.to {
            // A loop on the node's outer side.
            if h {
                let (x, y) = (a.x + a.w / 2., a.y + a.h);
                Curve { from: (x - 8., y), c1: (x - 28., y + 36.), c2: (x + 28., y + 36.), to: (x + 8., y) }
            } else {
                let (x, y) = (a.x + a.w, a.y + a.h / 2.);
                Curve { from: (x, y - 8.), c1: (x + 36., y - 28.), c2: (x + 36., y + 28.), to: (x, y + 8.) }
            }
        } else if rank[e.to] > rank[e.from] {
            if h {
                let (f, t) = ((a.x + a.w, a.y + a.h / 2.), (b.x, b.y + b.h / 2.));
                let d = (t.0 - f.0) * 0.5;
                Curve { from: f, c1: (f.0 + d, f.1), c2: (t.0 - d, t.1), to: t }
            } else {
                let (f, t) = ((a.x + a.w / 2., a.y + a.h), (b.x + b.w / 2., b.y));
                let d = (t.1 - f.1) * 0.5;
                Curve { from: f, c1: (f.0, f.1 + d), c2: (t.0, t.1 - d), to: t }
            }
        } else if rank[e.to] == rank[e.from] {
            let l = if h { a.y < b.y } else { a.x < b.x };
            if h {
                let (f, t) = if l { ((a.x + a.w / 2., a.y + a.h), (b.x + b.w / 2., b.y)) } else { ((a.x + a.w / 2., a.y), (b.x + b.w / 2., b.y + b.h)) };
                Curve { from: f, c1: f, c2: t, to: t }
            } else {
                let (f, t) = if l { ((a.x + a.w, a.y + a.h / 2.), (b.x, b.y + b.h / 2.)) } else { ((a.x, a.y + a.h / 2.), (b.x + b.w, b.y + b.h / 2.)) };
                Curve { from: f, c1: f, c2: t, to: t }
            }
        } else {
            // A back edge goes around the ranks it spans, on the side with fewer
            // nodes in the way at its two ends, so it does not cross the flow.
            let (lo, hi) = (rank[e.to], rank[e.from]);
            let band: Vec<Placed> = (0..n).filter(|&v| (lo..=hi).contains(&rank[v])).map(|v| placed[v]).collect();
            let ((ab, aa), (bb, ba)) = (slot(e.from), slot(e.to));
            let near = ab + bb < aa + ba;
            let out = 36.;
            if h {
                if near {
                    let y = band.iter().map(|p| p.y).fold(f32::MAX, f32::min) - out;
                    let (f, t) = ((a.x + a.w / 2., a.y), (b.x + b.w / 2., b.y));
                    Curve { from: f, c1: (f.0, y), c2: (t.0, y), to: t }
                } else {
                    let y = band.iter().map(|p| p.y + p.h).fold(f32::MIN, f32::max) + out;
                    let (f, t) = ((a.x + a.w / 2., a.y + a.h), (b.x + b.w / 2., b.y + b.h));
                    Curve { from: f, c1: (f.0, y), c2: (t.0, y), to: t }
                }
            } else if near {
                let x = band.iter().map(|p| p.x).fold(f32::MAX, f32::min) - out;
                let (f, t) = ((a.x, a.y + a.h / 2.), (b.x, b.y + b.h / 2.));
                Curve { from: f, c1: (x, f.1), c2: (x, t.1), to: t }
            } else {
                let x = band.iter().map(|p| p.x + p.w).fold(f32::MIN, f32::max) + out;
                let (f, t) = ((a.x + a.w, a.y + a.h / 2.), (b.x + b.w, b.y + b.h / 2.));
                Curve { from: f, c1: (x, f.1), c2: (x, t.1), to: t }
            }
        };
        curves.push(curve);
    }

    // Everything drawn, edge labels included, then moved inside the padding.
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut grow = |x: f32, y: f32| {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    };
    for p in &placed {
        grow(p.x, p.y);
        grow(p.x + p.w, p.y + p.h);
    }
    for (e, c) in edges.iter().zip(&curves) {
        for k in 0..=16 {
            let (x, y) = c.at(k as f32 / 16.);
            grow(x, y);
        }
        if let Some(l) = &e.label {
            let ((mx, my), (_, (w, lh))) = (c.mid(), edge_label_box(l));
            grow(mx - w / 2., my - lh / 2.);
            grow(mx + w / 2., my + lh / 2.);
        }
    }
    let (dx, dy) = (PAD - x0, PAD - y0);
    for p in &mut placed {
        p.x += dx;
        p.y += dy;
    }
    let shift = |(x, y): (f32, f32)| (x + dx, y + dy);
    for c in &mut curves {
        *c = Curve { from: shift(c.from), c1: shift(c.c1), c2: shift(c.c2), to: shift(c.to) };
    }
    Laid {
        placed,
        curves,
        width: (x1 - x0 + 2. * PAD).ceil(),
        height: (y1 - y0 + 2. * PAD).ceil(),
    }
}
// ---------------------------------------------------------------------------
// Rendering

/// The code block's shell, with the diagram where the code would be.
fn card(ix: usize, icon_name: &str, title: String, source: &str, body: AnyElement) -> AnyElement {
    div()
        .id(("diagram", ix))
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(px(crate::ui::RADIUS_LG))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(36.))
                .pl(px(12.))
                .pr(px(6.))
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(icon(icon_name, 14.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(crate::ui::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_muted())
                        .child(title),
                )
                .child(copy_button(("diagram-copy", ix), source.to_string())),
        )
        .child(body)
        .into_any_element()
}

pub fn render(ix: usize, diagram: &Diagram, source: &str) -> AnyElement {
    match diagram {
        Diagram::Chart(c) => {
            let title = c.title.clone().unwrap_or_else(|| "Chart".into());
            card(ix, "chart-breakout-square", title, source, render_chart(ix, c))
        }
        Diagram::Flow(f) => card(ix, "git-branch", "Diagram".into(), source, render_flow(ix, f)),
    }
}

fn legend(series: &[Series]) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .gap_x(px(16.))
        .gap_y(px(4.))
        .text_size(px(crate::ui::TEXT_XS))
        .text_color(theme::text_subtle())
        .children(series.iter().enumerate().map(|(i, s)| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(div().size(px(8.)).rounded(px(4.)).bg(color(i)))
                .child(s.name.clone())
        }))
        .into_any_element()
}

fn render_chart(ix: usize, c: &Chart) -> AnyElement {
    let groups = c.labels.len();
    let longest = c.labels.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let kind = match c.kind {
        Kind::Bar if longest > 10 || groups > 14 || groups * c.series.len() > 36 => Kind::HBar,
        k => k,
    };
    let body = match kind {
        Kind::Bar => bars(c),
        Kind::HBar => hbars(c),
        Kind::Line | Kind::Area => lines(ix, c, kind == Kind::Area),
        Kind::Pie => pie(c),
    };
    div()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .min_w_0()
        .child(body)
        .when(c.series.len() > 1 && kind != Kind::Pie, |d| d.child(legend(&c.series)))
        .into_any_element()
}

const PLOT_H: f32 = 160.;

fn bars(c: &Chart) -> AnyElement {
    let max = nice_max(c.values().fold(0., f64::max));
    let show_values = c.labels.len() * c.series.len() <= 16;
    div()
        .flex()
        .items_end()
        .gap(px(10.))
        .min_w_0()
        .children(c.labels.iter().enumerate().map(|(g, label)| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .w_full()
                        .h(px(PLOT_H + 16.))
                        .flex()
                        .items_end()
                        .justify_center()
                        .gap(px(4.))
                        .border_b_1()
                        .border_color(theme::border_strong())
                        .children(c.series.iter().enumerate().map(|(s, series)| {
                            let v = series.values.get(g).copied().unwrap_or(0.).max(0.);
                            let h = (v / max) as f32 * PLOT_H;
                            div()
                                .flex_1()
                                .max_w(px(40.))
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_end()
                                .gap(px(2.))
                                .when(show_values, |d| {
                                    d.child(
                                        div()
                                            .text_size(px(crate::ui::TEXT_XS))
                                            .line_height(px(12.))
                                            .text_color(theme::text_subtle())
                                            .whitespace_nowrap()
                                            .child(c.fmt(v)),
                                    )
                                })
                                .child(div().w_full().h(px(h.max(2.))).rounded_t(px(4.)).bg(color(s)))
                        })),
                )
                .child(
                    div()
                        .w_full()
                        .truncate()
                        .text_center()
                        .text_size(px(crate::ui::TEXT_XS))
                        .line_height(px(14.))
                        .text_color(theme::text_subtle())
                        .child(label.clone()),
                )
        }))
        .into_any_element()
}

fn hbars(c: &Chart) -> AnyElement {
    let max = nice_max(c.values().fold(0., f64::max));
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .min_w_0()
        .children(c.labels.iter().enumerate().map(|(g, label)| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .min_w_0()
                .child(
                    div()
                        .w(relative(0.28))
                        .max_w(px(220.))
                        .flex_none()
                        .truncate()
                        .text_right()
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .child(label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(4.))
                        .children(c.series.iter().enumerate().map(|(s, series)| {
                            let v = series.values.get(g).copied().unwrap_or(0.).max(0.);
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .h(px(if c.series.len() > 1 { 8. } else { 12. }))
                                        .rounded(px(4.))
                                        .bg(theme::fill_hover())
                                        .child(
                                            div()
                                                .h_full()
                                                .w(relative((v / max) as f32))
                                                .min_w(px(2.))
                                                .rounded(px(4.))
                                                .bg(color(s)),
                                        ),
                                )
                                .child(
                                    div()
                                        .w(px(56.))
                                        .flex_none()
                                        .text_size(px(crate::ui::TEXT_XS))
                                        .line_height(px(12.))
                                        .text_color(theme::text_subtle())
                                        .child(c.fmt(v)),
                                )
                        })),
                )
        }))
        .into_any_element()
}

const LINE_H: f32 = 170.;
const INSET: f32 = 6.;

fn lines(ix: usize, c: &Chart, area: bool) -> AnyElement {
    let (lo, hi) = c.values().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(v), b.max(v)));
    let lo = if lo >= 0. { 0. } else { -nice_max(-lo) };
    let hi = nice_max(hi.max(lo + 1e-9));
    let ticks: Vec<String> = (0..5).rev().map(|k| c.fmt(lo + (hi - lo) * k as f64 / 4.)).collect();
    let series: Vec<(Vec<f64>, Hsla)> = c
        .series
        .iter()
        .enumerate()
        .map(|(i, s)| (s.values.clone(), color(i)))
        .collect();
    let n = c.labels.len();
    let dots = n <= 40;
    let fill = area || c.series.len() == 1;
    let plot = canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
            let o = bounds.origin;
            let (w, h) = (bounds.size.width.as_f32(), bounds.size.height.as_f32() - 2. * INSET);
            let y_of = |v: f64| o.y + px(INSET + h - ((v - lo) / (hi - lo)) as f32 * h);
            let x_of = |i: usize| o.x + px(if n <= 1 { w / 2. } else { 4. + i as f32 * (w - 8.) / (n - 1) as f32 });
            for k in 0..5 {
                let y = o.y + px(INSET + h * k as f32 / 4.);
                let mut p = PathBuilder::stroke(px(1.));
                p.move_to(point(o.x, y));
                p.line_to(point(o.x + px(w), y));
                if let Ok(path) = p.build() {
                    window.paint_path(path, theme::mix(if k == 4 { 0.14 } else { 0.06 }));
                }
            }
            for (values, col) in &series {
                let pts: Vec<Point<Pixels>> = values.iter().enumerate().map(|(i, &v)| point(x_of(i), y_of(v))).collect();
                if pts.len() >= 2 && fill {
                    let mut p = PathBuilder::fill();
                    p.move_to(point(pts[0].x, y_of(lo.max(0.))));
                    for &q in &pts {
                        p.line_to(q);
                    }
                    p.line_to(point(pts[pts.len() - 1].x, y_of(lo.max(0.))));
                    p.close();
                    if let Ok(path) = p.build() {
                        window.paint_path(path, col.opacity(if area { 0.18 } else { 0.08 }));
                    }
                }
                if pts.len() >= 2 {
                    let mut p = PathBuilder::stroke(px(2.));
                    p.move_to(pts[0]);
                    for &q in &pts[1..] {
                        p.line_to(q);
                    }
                    if let Ok(path) = p.build() {
                        window.paint_path(path, *col);
                    }
                }
                if dots || pts.len() == 1 {
                    for &q in &pts {
                        let r = 3.;
                        window.paint_quad(quad(
                            Bounds::new(point(q.x - px(r), q.y - px(r)), size(px(r * 2.), px(r * 2.))),
                            px(r),
                            theme::background(),
                            px(1.75),
                            *col,
                            BorderStyle::Solid,
                        ));
                    }
                }
            }
        },
    )
    .flex_1()
    .min_w_0()
    .h(px(LINE_H));
    let x_labels: Vec<String> = match n {
        0 => Vec::new(),
        1..=8 => c.labels.clone(),
        _ => vec![c.labels[0].clone(), c.labels[n / 2].clone(), c.labels[n - 1].clone()],
    };
    let label = |t: String| {
        div()
            .text_size(px(crate::ui::TEXT_XS))
            .line_height(px(12.))
            .text_color(theme::text_subtle())
            .whitespace_nowrap()
            .child(t)
    };
    div()
        .id(("line-chart", ix))
        .flex()
        .flex_col()
        .gap(px(6.))
        .min_w_0()
        .child(
            div()
                .flex()
                .gap(px(8.))
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .w(px(44.))
                        .h(px(LINE_H))
                        .flex()
                        .flex_col()
                        .justify_between()
                        .items_end()
                        .children(ticks.into_iter().map(label)),
                )
                .child(plot),
        )
        .child(
            div()
                .flex()
                .justify_between()
                .ml(px(52.))
                .min_w_0()
                .children(x_labels.into_iter().map(label)),
        )
        .into_any_element()
}

fn pie(c: &Chart) -> AnyElement {
    let values: Vec<f64> = c.series[0].values.iter().map(|v| v.max(0.)).collect();
    let total: f64 = values.iter().sum();
    let paint_values = values.clone();
    const D: f32 = 150.;
    let donut = canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
            let total: f64 = paint_values.iter().sum();
            if total <= 0. {
                return;
            }
            let o = bounds.origin;
            let (cx, cy) = (o.x.as_f32() + D / 2., o.y.as_f32() + D / 2.);
            let (r_out, r_in) = (D / 2., D / 2. - 26.);
            let at = |r: f32, a: f32| point(px(cx + r * a.cos()), px(cy + r * a.sin()));
            let mut start = -std::f32::consts::FRAC_PI_2;
            for (i, v) in paint_values.iter().enumerate() {
                let sweep = (*v / total) as f32 * std::f32::consts::TAU;
                if sweep <= 0. {
                    continue;
                }
                let gap = if paint_values.len() > 1 { 0.02 } else { 0. };
                let (a0, a1) = (start + gap, start + sweep - gap);
                let steps = ((a1 - a0).abs() / 0.05).ceil().max(2.) as usize;
                let mut pts = Vec::with_capacity(steps * 2 + 2);
                for k in 0..=steps {
                    pts.push(at(r_out, a0 + (a1 - a0) * k as f32 / steps as f32));
                }
                for k in (0..=steps).rev() {
                    pts.push(at(r_in, a0 + (a1 - a0) * k as f32 / steps as f32));
                }
                let mut p = PathBuilder::fill();
                p.add_polygon(&pts, true);
                if let Ok(path) = p.build() {
                    window.paint_path(path, color(i));
                }
                start += sweep;
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size(px(D));
    div()
        .flex()
        .items_center()
        .gap(px(24.))
        .min_w_0()
        .child(
            div()
                .relative()
                .flex_none()
                .size(px(D))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .child(donut)
                .child(
                    div()
                        .ty(crate::ui::Type::Subtitle)
                        .text_color(theme::text())
                        .child(c.fmt(total)),
                )
                .child(div().text_size(px(crate::ui::TEXT_XS)).text_color(theme::text_subtle()).child("total")),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .text_size(px(crate::ui::TEXT_SM))
                .children(c.labels.iter().enumerate().map(|(i, label)| {
                    let v = values.get(i).copied().unwrap_or(0.);
                    let pct = if total > 0. { v / total * 100. } else { 0. };
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .min_w_0()
                        .child(div().flex_none().size(px(8.)).rounded(px(4.)).bg(color(i)))
                        .child(div().flex_1().min_w_0().truncate().text_color(theme::text_muted()).child(label.clone()))
                        .child(div().flex_none().text_color(theme::text_subtle()).child(c.fmt(v)))
                        .child(
                            div()
                                .flex_none()
                                .w(px(44.))
                                .text_right()
                                .text_color(theme::text_faint())
                                .child(format!("{pct:.0}%")),
                        )
                })),
        )
        .into_any_element()
}

fn render_flow(ix: usize, f: &Flow) -> AnyElement {
    let shapes: Vec<(Shape, Placed)> = f.nodes.iter().map(|n| n.shape).zip(f.placed.iter().copied()).collect();
    let edges: Vec<(Curve, bool, bool, bool)> = f
        .edges
        .iter()
        .zip(&f.curves)
        .map(|(e, c)| (*c, e.arrow, e.dotted, e.thick))
        .collect();
    let paint = canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| paint_flow(bounds, &shapes, &edges, window),
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(f.width))
    .h(px(f.height));
    let mut area = div()
        .relative()
        .flex_none()
        .mx_auto()
        .w(px(f.width))
        .h(px(f.height))
        .child(paint);
    // Lines are laid out by hand so the text matches the box it was sized for.
    for (lines, p) in f.lines.iter().zip(&f.placed) {
        area = area.child(
            div()
                .absolute()
                .left(px(p.x))
                .top(px(p.y))
                .w(px(p.w))
                .h(px(p.h))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .text_size(px(crate::ui::TEXT_SM))
                .line_height(px(NODE_LINE_H))
                .text_color(theme::content())
                .children(lines.iter().map(|l| div().whitespace_nowrap().child(l.clone()))),
        );
    }
    for (e, c) in f.edges.iter().zip(&f.curves) {
        let Some(label) = &e.label else { continue };
        let (mx, my) = c.mid();
        let (lines, (w, lh)) = edge_label_box(label);
        area = area.child(
            div()
                .absolute()
                .left(px(mx - w / 2.))
                .top(px(my - lh / 2.))
                .w(px(w))
                .h(px(lh))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .rounded(px(4.))
                .bg(theme::background())
                .text_size(px(crate::ui::TEXT_XS))
                .line_height(px(EDGE_LINE_H))
                .text_color(theme::text_subtle())
                .children(lines.into_iter().map(|l| div().whitespace_nowrap().child(l))),
        );
    }
    div()
        .id(("flow-scroll", ix))
        .flex()
        .min_w_0()
        .py(px(10.))
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .child(area)
        .into_any_element()
}

fn paint_flow(bounds: Bounds<Pixels>, shapes: &[(Shape, Placed)], edges: &[(Curve, bool, bool, bool)], window: &mut Window) {
    let o = bounds.origin;
    let pt = |(x, y): (f32, f32)| point(o.x + px(x), o.y + px(y));
    let line = theme::mix(0.38);
    for &(c, arrow, dotted, thick) in edges {
        // End the stroke at the arrow's base so its tip stays sharp.
        let (dx, dy) = (c.to.0 - c.c2.0, c.to.1 - c.c2.1);
        let (dx, dy) = if dx.abs() + dy.abs() < 0.01 { (c.to.0 - c.from.0, c.to.1 - c.from.1) } else { (dx, dy) };
        let len = (dx * dx + dy * dy).sqrt().max(0.01);
        let (ux, uy) = (dx / len, dy / len);
        let tip = 8.;
        let end = if arrow { (c.to.0 - ux * tip * 0.8, c.to.1 - uy * tip * 0.8) } else { c.to };
        let mut p = PathBuilder::stroke(px(if thick { 2.5 } else { 1.5 }));
        if dotted {
            p = p.dash_array(&[px(4.), px(4.)]);
        }
        p.move_to(pt(c.from));
        p.cubic_bezier_to(pt(end), pt(c.c1), pt(c.c2));
        if let Ok(path) = p.build() {
            window.paint_path(path, line);
        }
        if arrow {
            let base = (c.to.0 - ux * tip, c.to.1 - uy * tip);
            let (px_, py_) = (-uy * tip * 0.5, ux * tip * 0.5);
            let mut a = PathBuilder::fill();
            a.add_polygon(
                &[pt(c.to), pt((base.0 + px_, base.1 + py_)), pt((base.0 - px_, base.1 - py_))],
                true,
            );
            if let Ok(path) = a.build() {
                window.paint_path(path, theme::mix(0.55));
            }
        }
    }
    let fill = theme::background().blend(theme::mix(0.06));
    let border = theme::mix(0.28);
    for &(shape, p) in shapes {
        let b = Bounds::new(pt((p.x, p.y)), size(px(p.w), px(p.h)));
        match shape {
            Shape::Rect | Shape::Round | Shape::Stadium | Shape::Circle => {
                let r = match shape {
                    Shape::Rect => 6.,
                    Shape::Round => 12.,
                    _ => p.h / 2.,
                };
                window.paint_quad(quad(b, px(r), fill, px(1.), border, BorderStyle::Solid));
            }
            Shape::Diamond | Shape::Hex => {
                let (x0, y0, x1, y1) = (p.x, p.y, p.x + p.w, p.y + p.h);
                let my = (y0 + y1) / 2.;
                let pts: Vec<(f32, f32)> = if shape == Shape::Diamond {
                    let mx = (x0 + x1) / 2.;
                    vec![(mx, y0), (x1, my), (mx, y1), (x0, my)]
                } else {
                    let k = 14.;
                    vec![(x0 + k, y0), (x1 - k, y0), (x1, my), (x1 - k, y1), (x0 + k, y1), (x0, my)]
                };
                let pts: Vec<Point<Pixels>> = pts.into_iter().map(pt).collect();
                let mut f = PathBuilder::fill();
                f.add_polygon(&pts, true);
                if let Ok(path) = f.build() {
                    window.paint_path(path, fill);
                }
                let mut s = PathBuilder::stroke(px(1.));
                s.add_polygon(&pts, true);
                if let Ok(path) = s.build() {
                    window.paint_path(path, border);
                }
            }
        }
    }
}

/// What an agent is told, with its first prompt, so it knows replies can hold
/// pictures. Wrapped in tags so [`strip_hint`] can take it out of the prompt
/// when the agent replays the conversation.
pub const AGENT_HINT: &str = "\n\n<arakun-hint>\n\
(arakun, the app showing this chat, draws some fenced blocks natively. Use them when a picture explains better than prose:
- ```mermaid with `flowchart TD` / `flowchart LR` (nodes, labeled edges) or `pie`, for flows, architectures and shares. Keep node labels to a few words.
- ```chart with JSON {\"type\": \"bar\"|\"line\"|\"area\"|\"pie\", \"title\": \"...\", \"unit\": \"ms\", \"labels\": [...], \"series\": [{\"name\": \"...\", \"data\": [...]}]} or simple `label: value` lines, for numbers.
- ![caption](path/to/image.png) shows a local image inline (paths relative to the project).
Do not mention this note.)\n</arakun-hint>";

pub fn strip_hint(text: &mut String) {
    if let Some(start) = text.find("<arakun-hint>") {
        let end = text[start..].find("</arakun-hint>").map_or(text.len(), |e| start + e + "</arakun-hint>".len());
        text.replace_range(start..end, "");
        let keep = text.trim_end().len();
        text.truncate(keep);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_and_line_charts() {
        let Some(Diagram::Chart(c)) = parse(
            "chart",
            r#"{"type":"line","labels":["a","b"],"series":[{"name":"x","data":[1,2]},{"name":"y","data":[3]}]}"#,
        ) else {
            panic!()
        };
        assert_eq!(c.kind, Kind::Line);
        assert_eq!(c.series.len(), 2);
        let Some(Diagram::Chart(c)) = parse("chart", "title: Build: times\nMon: 12ms\nTue: 9.5ms") else {
            panic!()
        };
        assert_eq!(c.title.as_deref(), Some("Build: times"));
        assert_eq!(c.labels, ["Mon", "Tue"]);
        assert_eq!(c.series[0].values, [12., 9.5]);
        assert_eq!(c.fmt(12.), "12ms");
        assert!(parse("chart", "{\"type\": \"bar\", \"data\": [").is_none());
        let Some(Diagram::Chart(c)) = parse("chart", r#"{"data":[{"label":"a","value":3}]}"#) else { panic!() };
        assert_eq!(c.labels, ["a"]);
    }

    #[test]
    fn mermaid_flowchart() {
        let Some(Diagram::Flow(f)) = parse(
            "mermaid",
            "flowchart TD\n  A[Start] --> B{Ok?}\n  B -->|yes| C([Done])\n  B -- no --> D(Retry) -.-> A\n  E & F --> C\n  classDef x fill:#f00",
        ) else {
            panic!()
        };
        let ids: Vec<&str> = f.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["A", "B", "C", "D", "E", "F"]);
        assert_eq!(f.nodes[1].shape, Shape::Diamond);
        assert_eq!(f.nodes[2].label, "Done");
        assert_eq!(f.edges.len(), 6);
        assert_eq!(f.edges[1].label.as_deref(), Some("yes"));
        assert_eq!(f.edges[2].label.as_deref(), Some("no"));
        assert!(f.edges[3].dotted && f.edges[3].arrow);
        assert_eq!(f.curves.len(), 6);
        assert!(f.width > 0. && f.height > 0.);
        // A starts above B, B above C.
        assert!(f.placed[0].y < f.placed[1].y && f.placed[1].y < f.placed[2].y);
        assert!(parse("mermaid", "graph LR; a-->b; b-->c").is_some());
        assert!(parse("mermaid", "sequenceDiagram\nA->>B: hi").is_none());
    }

    const HARNESS_LOOP: &str = "flowchart LR
  A[Agente termina turno] --> B{arakun corre el check del proyecto}
  B -- falla --> C[arakun reinyecta la salida, máx. N veces]
  C --> A
  B -- pasa --> D[Navegador y logs: el agente comprueba la app]
  D --> E[Listo y aviso al usuario]
  E --> F[Métricas: keep rate, errores, bucles]
  F --> G[Automation semanal propone ajustes al harness]";

    fn inside(p: &Placed, (x, y): (f32, f32)) -> bool {
        x > p.x + 1. && x < p.x + p.w - 1. && y > p.y + 1. && y < p.y + p.h - 1.
    }

    #[test]
    fn long_flowcharts_fit_the_column() {
        let Some(Diagram::Flow(f)) = parse("mermaid", HARNESS_LOOP) else { panic!() };
        // Seven nodes in a row are far wider than a reply; it turns top-down.
        assert!(!f.horizontal);
        assert!(f.width <= FIT_W, "width {}", f.width);
        for (lines, p) in f.lines.iter().zip(&f.placed) {
            assert!(p.x >= 0. && p.y >= 0. && p.x + p.w <= f.width && p.y + p.h <= f.height);
            for l in lines {
                assert!(text_w(l) <= p.w, "{l:?} wider than its box");
            }
        }
        assert!(f.lines[2].len() > 1, "long labels wrap: {:?}", f.lines[2]);
        for (i, a) in f.placed.iter().enumerate() {
            for b in &f.placed[i + 1..] {
                let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                assert!(apart, "nodes overlap");
            }
        }
        // No edge runs through a node it does not touch.
        for (e, c) in f.edges.iter().zip(&f.curves) {
            for (v, p) in f.placed.iter().enumerate() {
                if v == e.from || v == e.to {
                    continue;
                }
                assert!((0..=32).all(|k| !inside(p, c.at(k as f32 / 32.))), "edge {e:?} crosses node {v}");
            }
        }
    }

    #[test]
    fn short_flowcharts_keep_their_direction() {
        let Some(Diagram::Flow(f)) = parse("mermaid", "flowchart LR\n  A[Plan] --> B[Code] --> C[Ship]\n  C -. bug .-> A") else {
            panic!()
        };
        assert!(f.horizontal);
        assert!(f.placed[0].x < f.placed[1].x && f.placed[1].x < f.placed[2].x);
        assert_eq!(wrap_label("uno dos\ntres", 400.), ["uno dos", "tres"]);
    }

    #[test]
    fn edge_label_line_breaks() {
        let Some(Diagram::Flow(f)) = parse("mermaid", "flowchart TD\n  A -- \"x 62<br/>capas\" --> B\n  B -- uno --> C") else {
            panic!()
        };
        let (two, (_, h2)) = edge_label_box(f.edges[0].label.as_deref().unwrap());
        let (one, (_, h1)) = edge_label_box(f.edges[1].label.as_deref().unwrap());
        assert_eq!(two, vec!["x 62".to_string(), "capas".to_string()]);
        assert_eq!(one.len(), 1);
        assert!(h2 > h1);
    }

    #[test]
    fn mermaid_pie() {
        let Some(Diagram::Chart(c)) = parse("mermaid", "pie title Pets\n  \"Dogs\" : 386\n  \"Cats\" : 85") else {
            panic!()
        };
        assert_eq!(c.kind, Kind::Pie);
        assert_eq!(c.title.as_deref(), Some("Pets"));
        assert_eq!(c.series[0].values, [386., 85.]);
    }

    #[test]
    fn number_formats() {
        assert_eq!(fmt_num(12000.), "12k");
        assert_eq!(fmt_num(3.5), "3.5");
        assert_eq!(fmt_num(2.0), "2");
        assert_eq!(nice_max(37.), 50.);
        assert_eq!(nice_max(100.), 100.);
    }
}
