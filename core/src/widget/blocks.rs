//! Blocks, version 1: what `view` returns. Plain data with tones, never
//! colors, HTML or Markdown, so the app draws every widget with its own
//! theme. The script builds them with the functions in [`register`] or as
//! maps with a `block` key; [`Block::from_dynamic`] reads either.

use rhai::{Array, Dynamic, Engine, FLOAT, INT, Map};

/// Longest text a block keeps; the rest is cut.
const MAX_TEXT: usize = 400;
/// Most children, rows or points a block keeps.
const MAX_ITEMS: usize = 200;
/// How deep stacks and rows nest.
const MAX_DEPTH: usize = 8;

/// A color by meaning. The app maps each to its theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Success,
    Error,
    Attention,
    Link,
    Muted,
    /// Category 1 to 8, for charts and labels that only need to differ.
    Category(u8),
}

impl Tone {
    pub fn parse(s: &str) -> Option<Tone> {
        Some(match s {
            "" | "plain" => Tone::Plain,
            "success" => Tone::Success,
            "error" => Tone::Error,
            "attention" => Tone::Attention,
            "link" => Tone::Link,
            "muted" => Tone::Muted,
            _ => {
                let n: u8 = s
                    .strip_prefix("category-")
                    .or_else(|| s.strip_prefix("category"))?
                    .parse()
                    .ok()?;
                return (1..=8).contains(&n).then_some(Tone::Category(n));
            }
        })
    }
}

/// What a button does, after Arakun asks.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    StartSession { prompt: String },
    OpenUrl { url: String },
}

impl Action {
    /// The need that allows it.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::StartSession { .. } => "start_session",
            Action::OpenUrl { .. } => "open_url",
        }
    }

    /// Exactly what will happen, for the question Arakun asks each time.
    pub fn ask(&self) -> String {
        match self {
            Action::StartSession { prompt } => format!("Start a session with: {prompt}"),
            Action::OpenUrl { url } => format!("Open {url}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListRow {
    pub dot: Option<Tone>,
    pub title: String,
    pub detail: String,
    pub time: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    Stack(Vec<Block>),
    Row(Vec<Block>),
    Text {
        text: String,
        tone: Tone,
    },
    Stat {
        label: String,
        value: String,
        change: String,
        tone: Tone,
    },
    Dot {
        tone: Tone,
        label: String,
    },
    Badge {
        text: String,
        tone: Tone,
    },
    List(Vec<ListRow>),
    Table {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Sparkline {
        values: Vec<f32>,
        tone: Tone,
    },
    Bars {
        values: Vec<f32>,
        labels: Vec<String>,
        tone: Tone,
    },
    /// A share, 0 to 1.
    Meter {
        value: f32,
        label: String,
        tone: Tone,
    },
    Button {
        label: String,
        action: Action,
    },
    /// Opens `url` after asking. Always https.
    Link {
        label: String,
        url: String,
    },
    Empty(String),
    /// A block this Arakun doesn't know: drawn as [`Block::NEWER`].
    Unknown(String),
}

impl Block {
    /// What an unknown block shows.
    pub const NEWER: &'static str = "This widget needs a newer Arakun";

    /// Reads what `view` returned. An array is a stack.
    pub fn from_dynamic(v: &Dynamic) -> Result<Block, String> {
        read(v, 0)
    }

    /// The actions the buttons in this block ask for, for the check against
    /// the needs.
    pub fn actions(&self) -> Vec<&Action> {
        let mut out = Vec::new();
        self.walk(&mut |b| {
            if let Block::Button { action, .. } = b {
                out.push(action);
            }
        });
        out
    }

    fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Block)) {
        f(self);
        if let Block::Stack(c) | Block::Row(c) = self {
            for b in c {
                b.walk(f);
            }
        }
    }
}

fn read(v: &Dynamic, depth: usize) -> Result<Block, String> {
    if depth > MAX_DEPTH {
        return Err(format!("Blocks nest more than {MAX_DEPTH} deep"));
    }
    if let Some(a) = v.read_lock::<Array>() {
        return Ok(Block::Stack(children(&a, depth)?));
    }
    if v.is_string() {
        return Ok(Block::Text {
            text: clean(&v.to_string()),
            tone: Tone::Plain,
        });
    }
    let Some(m) = v.read_lock::<Map>() else {
        return Err(format!("view returned {}, not blocks", v.type_name()));
    };
    let kind = text(&m, "block");
    let tone = tone(&m, "tone")?;
    Ok(match kind.as_str() {
        "stack" | "row" => {
            let c = m
                .get("children")
                .and_then(|c| c.read_lock::<Array>().map(|a| children(&a, depth)))
                .transpose()?
                .unwrap_or_default();
            if kind == "stack" {
                Block::Stack(c)
            } else {
                Block::Row(c)
            }
        }
        "text" => Block::Text {
            text: text(&m, "text"),
            tone,
        },
        "stat" => Block::Stat {
            label: text(&m, "label"),
            value: text(&m, "value"),
            change: text(&m, "change"),
            tone,
        },
        "dot" => Block::Dot {
            tone,
            label: text(&m, "label"),
        },
        "badge" => Block::Badge {
            text: text(&m, "text"),
            tone,
        },
        "list" => Block::List(
            items(&m, "rows")
                .iter()
                .map(|r| {
                    let r = r.read_lock::<Map>().ok_or("A list row must be a map")?;
                    Ok(ListRow {
                        dot: match r.get("dot") {
                            Some(_) => Some(tone_of(&r, "dot")?),
                            None => None,
                        },
                        title: text(&r, "title"),
                        detail: text(&r, "detail"),
                        time: text(&r, "time"),
                    })
                })
                .collect::<Result<_, String>>()?,
        ),
        "table" => Block::Table {
            columns: items(&m, "columns").iter().map(plain).collect(),
            rows: items(&m, "rows")
                .iter()
                .map(|r| {
                    r.read_lock::<Array>()
                        .map(|a| a.iter().take(MAX_ITEMS).map(plain).collect())
                        .unwrap_or_default()
                })
                .collect(),
        },
        "sparkline" => Block::Sparkline {
            values: numbers(&m, "values"),
            tone,
        },
        "bars" => Block::Bars {
            values: numbers(&m, "values"),
            labels: items(&m, "labels").iter().map(plain).collect(),
            tone,
        },
        "meter" => Block::Meter {
            value: m.get("value").and_then(number).unwrap_or(0.).clamp(0., 1.),
            label: text(&m, "label"),
            tone,
        },
        "button" => Block::Button {
            label: text(&m, "label"),
            action: match text(&m, "action").as_str() {
                "start_session" => Action::StartSession {
                    prompt: text(&m, "prompt"),
                },
                "open_url" => Action::OpenUrl {
                    url: https(&text(&m, "url"))?,
                },
                other => return Err(format!("A button can't `{other}`")),
            },
        },
        "link" => Block::Link {
            label: text(&m, "label"),
            url: https(&text(&m, "url"))?,
        },
        "empty" => Block::Empty(text(&m, "text")),
        other => Block::Unknown(clean(other)),
    })
}

fn children(a: &Array, depth: usize) -> Result<Vec<Block>, String> {
    a.iter()
        .take(MAX_ITEMS)
        .filter(|v| !v.is_unit())
        .map(|v| read(v, depth + 1))
        .collect()
}

fn items(m: &Map, key: &str) -> Vec<Dynamic> {
    m.get(key)
        .and_then(|v| {
            v.read_lock::<Array>()
                .map(|a| a.iter().take(MAX_ITEMS).cloned().collect())
        })
        .unwrap_or_default()
}

fn numbers(m: &Map, key: &str) -> Vec<f32> {
    items(m, key).iter().filter_map(number).collect()
}

fn number(v: &Dynamic) -> Option<f32> {
    v.as_float()
        .ok()
        .or_else(|| v.as_int().ok().map(|i| i as FLOAT))
        .filter(|f| f.is_finite())
}

fn text(m: &Map, key: &str) -> String {
    m.get(key).map(plain).unwrap_or_default()
}

fn plain(v: &Dynamic) -> String {
    if v.is_unit() {
        String::new()
    } else {
        clean(&v.to_string())
    }
}

/// Plain text, one paragraph, short: no control characters a terminal or a
/// text layout would act on.
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TEXT)
        .collect()
}

fn tone(m: &Map, key: &str) -> Result<Tone, String> {
    match m.get(key) {
        None => Ok(Tone::Plain),
        Some(_) => tone_of(m, key),
    }
}

fn tone_of(m: &Map, key: &str) -> Result<Tone, String> {
    let t = text(m, key);
    Tone::parse(&t).ok_or_else(|| {
        format!("`{t}` is not a tone: use success, error, attention, link, muted or category1 to 8")
    })
}

fn https(url: &str) -> Result<String, String> {
    if url.starts_with("https://") && !url.contains(char::is_whitespace) {
        Ok(url.to_string())
    } else {
        Err(format!("`{url}` is not an https address"))
    }
}

/// The functions that build blocks. They only make maps, so both engines,
/// the one for `refresh` and the one for `view`, have them.
pub fn register(engine: &mut Engine) {
    fn block(kind: &str, mut m: Map) -> Map {
        m.insert("block".into(), kind.into());
        m
    }
    fn with(kind: &str, key: &str, v: Dynamic) -> Map {
        let mut m = Map::new();
        m.insert(key.into(), v);
        block(kind, m)
    }
    engine
        .register_fn("stack", |c: Array| with("stack", "children", c.into()))
        .register_fn("row", |c: Array| with("row", "children", c.into()))
        .register_fn("text", |s: &str| with("text", "text", s.into()))
        .register_fn("text", |s: &str, tone: &str| {
            let mut m = with("text", "text", s.into());
            m.insert("tone".into(), tone.into());
            m
        })
        .register_fn("stat", |m: Map| block("stat", m))
        .register_fn("dot", |tone: &str, label: &str| {
            let mut m = with("dot", "tone", tone.into());
            m.insert("label".into(), label.into());
            m
        })
        .register_fn("badge", |s: &str| with("badge", "text", s.into()))
        .register_fn("badge", |s: &str, tone: &str| {
            let mut m = with("badge", "text", s.into());
            m.insert("tone".into(), tone.into());
            m
        })
        .register_fn("list", |rows: Array| with("list", "rows", rows.into()))
        .register_fn("table", |m: Map| block("table", m))
        .register_fn("sparkline", |v: Array| {
            with("sparkline", "values", v.into())
        })
        .register_fn("bars", |m: Map| block("bars", m))
        .register_fn("meter", |v: FLOAT, label: &str| {
            let mut m = with("meter", "value", v.into());
            m.insert("label".into(), label.into());
            m
        })
        .register_fn("meter", |v: INT, label: &str| {
            let mut m = with("meter", "value", (v as FLOAT).into());
            m.insert("label".into(), label.into());
            m
        })
        .register_fn("button", |m: Map| block("button", m))
        .register_fn("link", |label: &str, url: &str| {
            let mut m = with("link", "label", label.into());
            m.insert("url".into(), url.into());
            m
        })
        .register_fn("empty", |s: &str| with("empty", "text", s.into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> Result<Block, String> {
        let mut e = Engine::new();
        register(&mut e);
        Block::from_dynamic(&e.eval::<Dynamic>(src).map_err(|e| e.to_string())?)
    }

    #[test]
    fn builders_make_blocks() {
        let b = eval(
            r#"[
                list([#{ dot: "success", title: "main", detail: "READY", time: "2m" }]),
                stat(#{ label: "Deploys", value: 12, change: "+3", tone: "category3" }),
                row([badge("beta", "attention"), meter(0.5, "Used")]),
                empty("No deploys yet"),
            ]"#,
        )
        .unwrap();
        let Block::Stack(c) = b else { panic!() };
        assert_eq!(c.len(), 4);
        assert_eq!(
            c[0],
            Block::List(vec![ListRow {
                dot: Some(Tone::Success),
                title: "main".into(),
                detail: "READY".into(),
                time: "2m".into(),
            }])
        );
        assert!(matches!(
            c[1],
            Block::Stat {
                tone: Tone::Category(3),
                ..
            }
        ));
        assert_eq!(c[3], Block::Empty("No deploys yet".into()));
    }

    #[test]
    fn colors_are_tones_only() {
        assert!(eval(r##"text("hi", "#ff0000")"##).is_err());
        assert!(eval(r#"text("hi", "category9")"#).is_err());
    }

    #[test]
    fn an_unknown_block_asks_for_a_newer_arakun() {
        assert_eq!(
            eval(r#"#{ block: "map3d" }"#).unwrap(),
            Block::Unknown("map3d".into())
        );
    }

    #[test]
    fn buttons_name_their_action_and_links_are_https() {
        let b = eval(r#"button(#{ label: "Fix", action: "start_session", prompt: "fix it" })"#)
            .unwrap();
        assert_eq!(b.actions()[0].kind(), "start_session");
        assert!(eval(r#"button(#{ label: "x", action: "run", prompt: "" })"#).is_err());
        assert!(eval(r#"link("x", "http://example.com")"#).is_err());
        assert!(eval(r#"link("x", "javascript:alert(1)")"#).is_err());
    }

    #[test]
    fn text_is_plain() {
        let Block::Text { text, .. } = eval(r#"text("a\nb\u001b[31m")"#).unwrap() else {
            panic!()
        };
        assert_eq!(text, "a b [31m");
    }
}
