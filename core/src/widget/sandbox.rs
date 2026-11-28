//! Running a widget's script inside what the user allowed.
//!
//! The script has no network, disk or process of its own: Rhai has none, and
//! module loading and `eval` are off. All it can reach is the host functions
//! here, and each checks the grant before it asks the [`Host`]. A call
//! outside the grant is recorded even if the script catches the error, and the
//! widget is stopped with the reason ([`Stop::Denied`]).
//!
//! `view` runs on a second engine that has no host functions at all, so a
//! redraw can't touch anything.
//!
//! An engine here is not `Send` (Rhai's `sync` feature would make the
//! Isometric scenes' engine `Sync` too), so a [`Widget`] lives on one thread;
//! the app gives each a thread of its own, where the host's blocking calls
//! are fine.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use rhai::module_resolvers::DummyModuleResolver;
use rhai::{AST, Array, CallFnOptions, Dynamic, Engine, EvalAltResult, FLOAT, INT, Map, Scope};

use super::blocks::{self, Block};
use super::manifest::{FORBIDDEN_HEADERS, Grant, Manifest, check_host, seconds};

type Res<T> = Result<T, Box<EvalAltResult>>;

/// Rhai steps `refresh` may take.
const REFRESH_OPERATIONS: u64 = 1_000_000;
/// Rhai steps `view` may take: it runs on every redraw.
const VIEW_OPERATIONS: u64 = 200_000;
const MAX_STRING: usize = 1 << 20;
const MAX_ARRAY: usize = 10_000;
const MAX_MAP: usize = 10_000;
/// Requests to one host in one of its `every` windows: a refresh may need a
/// few calls to the same API, but never a stream of them.
const FETCHES_PER_WINDOW: u32 = 5;
const NOTICE_EVERY: Duration = Duration::from_secs(60);
/// Lines of `print` and `debug` a widget's log keeps.
const LOG_LINES: usize = 200;
/// The answer a request may return.
pub const MAX_ANSWER: usize = 1 << 20;
/// What `read_file` returns at most.
pub const MAX_READ: u64 = 256 * 1024;

/// A request the sandbox checked and is ready to send: https, to a host in
/// the grant, with the token's header added by Arakun.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub url: String,
    pub host: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}

/// What `git()` returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GitInfo {
    pub branch: String,
    pub head: String,
    pub owner: String,
    pub repo: String,
}

/// One row of `sessions()`. Never messages, prompts, diffs or output.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionInfo {
    pub title: String,
    pub state: String,
    pub agent: String,
    pub branch: String,
}

/// What a token's lookup found.
pub enum Token {
    Found(String),
    /// The user hasn't added it yet: the widget waits for them.
    Missing,
}

/// The I/O behind the host functions. The sandbox has checked every call
/// against the grant before it gets here; [`super::local::Local`] is the
/// real one, tests have their own.
pub trait Host {
    fn fetch(&mut self, req: &Request) -> Result<Response, String>;
    /// `path` is one the grant names, relative to the project.
    fn read_file(&mut self, path: &str) -> Result<String, String>;
    fn git(&mut self) -> Option<GitInfo>;
    fn sessions(&mut self) -> Vec<SessionInfo>;
    /// Shown with the widget's name.
    fn notice(&mut self, title: &str, text: &str);
    fn token(&mut self, secret: &str) -> Token;
}

/// Why a widget was stopped. The text is what its row says.
#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    /// It asked for something it was not given.
    Denied(String),
    /// It keeps running out of steps or memory.
    Limit(String),
    /// A token the user hasn't added yet.
    NeedsToken(String),
}

impl Stop {
    pub fn reason(&self) -> &str {
        match self {
            Stop::Denied(s) | Stop::Limit(s) | Stop::NeedsToken(s) => s,
        }
    }
}

/// How a `refresh` ended, when it did not return new state.
#[derive(Clone, Debug, PartialEq)]
pub enum Failure {
    /// Keep the last state, show the error, try again later.
    Error(String),
    /// Ran out of steps or memory once.
    Limit(String),
    Stop(Stop),
}

struct Ctx {
    grant: Grant,
    host: Box<dyn Host>,
    /// Per host: when its window opened and the requests in it.
    windows: HashMap<String, (Instant, u32)>,
    last_notice: Option<Instant>,
    /// The first call outside the grant, kept even if the script catches
    /// the error.
    denied: Option<String>,
    missing_token: Option<String>,
    log: Vec<String>,
}

/// A compiled widget and the two engines it runs on.
pub struct Widget {
    pub manifest: Manifest,
    ast: AST,
    full: Engine,
    pure: Engine,
    ctx: Rc<RefCell<Ctx>>,
}

impl Widget {
    /// Compiles `main.rhai` for `manifest`, with `grant` as what it may do.
    /// The grant is the user's: an update waiting for approval runs the new
    /// script with the old grant only if it asks no more.
    pub fn new(
        manifest: Manifest,
        source: &str,
        grant: Grant,
        host: Box<dyn Host>,
    ) -> Result<Widget, String> {
        let ctx = Rc::new(RefCell::new(Ctx {
            grant,
            host,
            windows: HashMap::new(),
            last_notice: None,
            denied: None,
            missing_token: None,
            log: Vec::new(),
        }));
        let mut full = base(&ctx, REFRESH_OPERATIONS);
        register_host(&mut full, &ctx);
        let pure = base(&ctx, VIEW_OPERATIONS);
        let ast = full
            .compile(source)
            .map_err(|e| format!("main.rhai: {e}"))?;
        for (name, params) in [("refresh", 1), ("view", 2)] {
            if !ast
                .iter_functions()
                .any(|f| f.name == name && f.params.len() == params)
            {
                return Err(format!(
                    "main.rhai has no {name}() with {params} parameter{}",
                    if params == 1 { "" } else { "s" }
                ));
            }
        }
        Ok(Widget {
            manifest,
            ast,
            full,
            pure,
            ctx,
        })
    }

    /// Runs `refresh(state)` and returns the new state.
    pub fn refresh(&mut self, state: &Dynamic) -> Result<Dynamic, Failure> {
        {
            let mut c = self.ctx.borrow_mut();
            c.denied = None;
            c.missing_token = None;
        }
        let out = call(&self.full, &self.ast, "refresh", (state.clone(),));
        let mut c = self.ctx.borrow_mut();
        if let Some(why) = c.denied.take() {
            return Err(Failure::Stop(Stop::Denied(why)));
        }
        if let Some(label) = c.missing_token.take() {
            return Err(Failure::Stop(Stop::NeedsToken(format!(
                "Needs your {label}"
            ))));
        }
        let out = out.map_err(|e| failure(*e))?;
        data(&out, 0).map_err(Failure::Error)
    }

    /// Runs `view(state, size)` on the engine without host functions, and
    /// checks its buttons against the grant.
    pub fn view(&self, state: &Dynamic, size: &str) -> Result<Block, Failure> {
        let out = call(
            &self.pure,
            &self.ast,
            "view",
            (state.clone(), size.to_string()),
        )
        .map_err(|e| match *e {
            EvalAltResult::ErrorFunctionNotFound(ref f, _) if is_host_fn(f) => {
                Failure::Stop(Stop::Denied(format!(
                    "It called {} while drawing, which no widget may do",
                    f.split(' ').next().unwrap_or(f)
                )))
            }
            _ => failure(*e),
        })?;
        let block = Block::from_dynamic(&out).map_err(Failure::Error)?;
        let c = self.ctx.borrow();
        if let Some(a) = block
            .actions()
            .into_iter()
            .find(|a| !c.grant.needs.allows_action(a.kind()))
        {
            return Err(Failure::Stop(Stop::Denied(match a.kind() {
                "start_session" => "It asked to start sessions, which you did not allow".into(),
                _ => "It asked to open addresses, which you did not allow".into(),
            })));
        }
        Ok(block)
    }

    /// What `print` and `debug` said, newest last.
    pub fn log(&self) -> Vec<String> {
        self.ctx.borrow().log.clone()
    }
}

/// The Marketplace preview: `view(sample.json, size)` on an engine with no
/// host functions. It never runs `refresh`, so it touches nothing.
pub fn preview(
    manifest: Manifest,
    source: &str,
    sample: &str,
    size: &str,
) -> Result<Block, String> {
    let sample: serde_json::Value =
        serde_json::from_str(sample).map_err(|e| format!("sample.json: {e}"))?;
    if !sample.is_object() {
        return Err("sample.json must be an object".into());
    }
    // Its own needs, so the check on buttons passes; with no host behind
    // them, nothing else in the grant can be used.
    let grant = Grant::of(&manifest);
    let w = Widget::new(manifest, source, grant, Box::new(Nothing))?;
    w.view(&from_json(&sample), size).map_err(|e| match e {
        Failure::Error(s) | Failure::Limit(s) => s,
        Failure::Stop(s) => s.reason().to_string(),
    })
}

/// The host of a preview: nothing behind it, and nothing reaches it.
struct Nothing;

impl Host for Nothing {
    fn fetch(&mut self, _: &Request) -> Result<Response, String> {
        Err("A preview has no network".into())
    }
    fn read_file(&mut self, _: &str) -> Result<String, String> {
        Err("A preview has no files".into())
    }
    fn git(&mut self) -> Option<GitInfo> {
        None
    }
    fn sessions(&mut self) -> Vec<SessionInfo> {
        Vec::new()
    }
    fn notice(&mut self, _: &str, _: &str) {}
    fn token(&mut self, _: &str) -> Token {
        Token::Missing
    }
}

const HOST_FNS: [&str; 5] = ["fetch", "read_file", "git", "sessions", "notice"];

fn is_host_fn(signature: &str) -> bool {
    let name = signature.split([' ', '(']).next().unwrap_or("");
    HOST_FNS.contains(&name)
}

fn call(engine: &Engine, ast: &AST, name: &str, args: impl rhai::FuncArgs) -> Res<Dynamic> {
    let mut scope = Scope::new();
    // Top-level statements don't run: only the two functions do.
    let opts = CallFnOptions::new().eval_ast(false).rewind_scope(true);
    engine.call_fn_with_options(opts, &mut scope, ast, name, args)
}

fn failure(e: EvalAltResult) -> Failure {
    match e {
        EvalAltResult::ErrorTooManyOperations(_) => Failure::Limit("It took too many steps".into()),
        EvalAltResult::ErrorDataTooLarge(ref what, _) => {
            Failure::Limit(format!("{what} grew too large"))
        }
        EvalAltResult::ErrorStackOverflow(_) => {
            Failure::Limit("It called itself too deeply".into())
        }
        other => Failure::Error(other.to_string()),
    }
}

/// The engine both share: no modules, no `eval`, limits on everything, the
/// block builders and JSON.
fn base(ctx: &Rc<RefCell<Ctx>>, operations: u64) -> Engine {
    let mut e = Engine::new();
    e.set_module_resolver(DummyModuleResolver::new());
    e.disable_symbol("eval");
    e.set_max_operations(operations);
    e.set_max_string_size(MAX_STRING);
    e.set_max_array_size(MAX_ARRAY);
    e.set_max_map_size(MAX_MAP);
    e.set_max_expr_depths(64, 32);
    e.set_max_call_levels(32);
    e.set_max_modules(0);
    let log = ctx.clone();
    e.on_print(move |s| push_log(&log, s));
    let log = ctx.clone();
    e.on_debug(move |s, _, _| push_log(&log, s));
    blocks::register(&mut e);
    e.register_fn("parse_json", |s: &str| -> Res<Dynamic> {
        let v: serde_json::Value = serde_json::from_str(s).map_err(|e| e.to_string())?;
        Ok(from_json(&v))
    });
    e.register_fn("to_json", |v: Dynamic| -> Res<String> {
        Ok(to_json(&v, 0)?.to_string())
    });
    e
}

fn push_log(ctx: &Rc<RefCell<Ctx>>, s: &str) {
    // `print` inside a host function's borrow can't happen: host functions
    // don't call back into the script.
    if let Ok(mut c) = ctx.try_borrow_mut() {
        if c.log.len() >= LOG_LINES {
            c.log.remove(0);
        }
        c.log.push(s.chars().take(500).collect());
    }
}

/// Records the first call outside the grant and fails the call.
fn deny(ctx: &Rc<RefCell<Ctx>>, why: String) -> Box<EvalAltResult> {
    let mut c = ctx.borrow_mut();
    c.denied.get_or_insert(why.clone());
    why.into()
}

fn register_host(e: &mut Engine, ctx: &Rc<RefCell<Ctx>>) {
    let c = ctx.clone();
    e.register_fn("fetch", move |req: Map| fetch(&c, req));
    let c = ctx.clone();
    e.register_fn("fetch", move |url: &str| {
        let mut req = Map::new();
        req.insert("url".into(), url.into());
        fetch(&c, req)
    });
    let c = ctx.clone();
    e.register_fn("read_file", move |path: &str| -> Res<String> {
        if !c.borrow().grant.needs.file.iter().any(|f| f.path == path) {
            return Err(deny(
                &c,
                format!("It asked to read {path}, which you did not allow"),
            ));
        }
        c.borrow_mut().host.read_file(path).map_err(Into::into)
    });
    let c = ctx.clone();
    e.register_fn("git", move || -> Res<Dynamic> {
        if !c.borrow().grant.needs.git {
            return Err(deny(&c, "It asked for Git, which you did not allow".into()));
        }
        let Some(g) = c.borrow_mut().host.git() else {
            return Ok(Dynamic::UNIT);
        };
        let mut m = Map::new();
        m.insert("branch".into(), g.branch.into());
        m.insert("head".into(), g.head.into());
        m.insert("owner".into(), g.owner.into());
        m.insert("repo".into(), g.repo.into());
        Ok(m.into())
    });
    let c = ctx.clone();
    e.register_fn("sessions", move || -> Res<Array> {
        if !c.borrow().grant.needs.sessions {
            return Err(deny(
                &c,
                "It asked for your sessions, which you did not allow".into(),
            ));
        }
        let rows = c.borrow_mut().host.sessions();
        Ok(rows
            .into_iter()
            .map(|s| {
                let mut m = Map::new();
                m.insert("title".into(), s.title.into());
                m.insert("state".into(), s.state.into());
                m.insert("agent".into(), s.agent.into());
                m.insert("branch".into(), s.branch.into());
                m.into()
            })
            .collect())
    });
    let c = ctx.clone();
    e.register_fn("notice", move |title: &str, text: &str| -> Res<()> {
        if !c.borrow().grant.needs.notices {
            return Err(deny(
                &c,
                "It asked to show notices, which you did not allow".into(),
            ));
        }
        let mut cx = c.borrow_mut();
        let now = Instant::now();
        // Over the limit, a notice is dropped, not an error: the widget did
        // nothing it was not allowed to.
        if cx.last_notice.is_some_and(|t| now - t < NOTICE_EVERY) {
            return Ok(());
        }
        cx.last_notice = Some(now);
        let (title, text): (String, String) = (
            title.chars().take(80).collect(),
            text.chars().take(300).collect(),
        );
        cx.host.notice(&title, &text);
        Ok(())
    });
}

fn fetch(ctx: &Rc<RefCell<Ctx>>, req: Map) -> Res<Dynamic> {
    let get = |k: &str| req.get(k).filter(|v| !v.is_unit()).map(|v| v.to_string());
    let url = get("url").unwrap_or_default();
    let host = https_host(&url)?;
    let (every, auth) = {
        let c = ctx.borrow();
        let Some(net) = c.grant.needs.net(&host) else {
            drop(c);
            return Err(deny(
                ctx,
                format!("It asked to reach {host}, which you did not allow"),
            ));
        };
        let every = seconds(&net.every).unwrap_or(u64::MAX);
        let auth = match get("auth") {
            None => None,
            Some(name) => match c.grant.needs.secret(&name) {
                Some(s) if s.host == host => Some(s.clone()),
                Some(s) => {
                    let why = format!(
                        "It asked to send your {} to {host}; you allowed it only for {}",
                        s.label, s.host
                    );
                    drop(c);
                    return Err(deny(ctx, why));
                }
                None => {
                    drop(c);
                    return Err(deny(
                        ctx,
                        format!("It asked for a token named {name}, which you did not allow"),
                    ));
                }
            },
        };
        (every, auth)
    };
    let method = get("method")
        .unwrap_or_else(|| "GET".into())
        .to_ascii_uppercase();
    if !["GET", "POST", "HEAD"].contains(&method.as_str()) {
        return Err(format!("fetch can't use {method}: use GET, POST or HEAD").into());
    }
    let mut headers = Vec::new();
    if let Some(h) = req.get("headers") {
        let Some(h) = h.read_lock::<Map>() else {
            return Err("headers must be a map".into());
        };
        for (k, v) in h.iter() {
            let k = k.to_string();
            let lower = k.to_ascii_lowercase();
            let bad_name =
                k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
            let v = v.to_string();
            if bad_name || FORBIDDEN_HEADERS.contains(&lower.as_str()) || v.contains(['\r', '\n']) {
                return Err(format!("fetch can't set the header {k}").into());
            }
            headers.push((k, v));
        }
    }
    let body = get("body");

    let mut c = ctx.borrow_mut();
    let now = Instant::now();
    let window = c.windows.entry(host.clone()).or_insert((now, 0));
    if now.duration_since(window.0) >= Duration::from_secs(every) {
        *window = (now, 0);
    }
    if window.1 >= FETCHES_PER_WINDOW {
        return Err(format!(
            "{host} was reached {FETCHES_PER_WINDOW} times in its {every} s; wait for the next refresh"
        )
        .into());
    }
    window.1 += 1;

    if let Some(s) = auth {
        match c.host.token(&s.name) {
            Token::Found(token) => {
                // The script's own header of that name gives way to the token.
                headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&s.header));
                headers.push((s.header.clone(), s.format.replacen("{}", &token, 1)));
            }
            Token::Missing => {
                c.missing_token.get_or_insert(s.label.clone());
                return Err(format!("Needs your {}", s.label).into());
            }
        }
    }
    let request = Request {
        url,
        host,
        method,
        headers,
        body,
    };
    let response = c
        .host
        .fetch(&request)
        .map_err(|e| -> Box<EvalAltResult> { e.into() })?;
    drop(c);
    let text = String::from_utf8_lossy(&response.body).into_owned();
    let mut m = Map::new();
    m.insert("status".into(), (response.status as INT).into());
    m.insert("ok".into(), (200..300).contains(&response.status).into());
    m.insert(
        "json".into(),
        serde_json::from_str::<serde_json::Value>(&text)
            .map(|v| from_json(&v))
            .unwrap_or(Dynamic::UNIT),
    );
    m.insert("text".into(), text.into());
    Ok(m.into())
}

/// The host of an `https://` URL: no user, no port but 443, a name the
/// manifest could have named.
pub fn https_host(url: &str) -> Result<String, String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("fetch takes https addresses only, not {url}"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return Err("An address with a user name is not allowed".into());
    }
    let host = match authority.rsplit_once(':') {
        Some((h, "443")) => h,
        Some(_) => return Err("fetch can't use another port".into()),
        None => authority,
    };
    let host = host.to_ascii_lowercase();
    check_host(&host)?;
    if url.contains(char::is_whitespace) {
        return Err("An address can't have spaces".into());
    }
    Ok(host)
}

/// Keeps what `refresh` returned only if it is data: no functions or
/// handles, nothing that can carry behavior from one call to the next.
fn data(v: &Dynamic, depth: usize) -> Result<Dynamic, String> {
    if !v.is_map() {
        return Err(format!("refresh returned {}, not a map", v.type_name()));
    }
    to_json(v, depth).map(|j| from_json(&j))
}

fn to_json(v: &Dynamic, depth: usize) -> Result<serde_json::Value, String> {
    use serde_json::Value;
    if depth > 32 {
        return Err("The state nests too deep".into());
    }
    if v.is_unit() {
        return Ok(Value::Null);
    }
    if let Ok(b) = v.as_bool() {
        return Ok(b.into());
    }
    if let Ok(i) = v.as_int() {
        return Ok(i.into());
    }
    if let Ok(f) = v.as_float() {
        return Ok(serde_json::Number::from_f64(f as f64).map_or(Value::Null, Value::Number));
    }
    if v.is_string() || v.is_char() {
        return Ok(v.to_string().into());
    }
    if let Some(a) = v.read_lock::<Array>() {
        return a
            .iter()
            .map(|x| to_json(x, depth + 1))
            .collect::<Result<_, _>>()
            .map(Value::Array);
    }
    if let Some(m) = v.read_lock::<Map>() {
        return m
            .iter()
            .map(|(k, x)| Ok((k.to_string(), to_json(x, depth + 1)?)))
            .collect::<Result<_, String>>()
            .map(Value::Object);
    }
    Err(format!("{} is not data", v.type_name()))
}

pub fn from_json(v: &serde_json::Value) -> Dynamic {
    use serde_json::Value;
    match v {
        Value::Null => Dynamic::UNIT,
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => (i as INT).into(),
            None => (n.as_f64().unwrap_or(0.) as FLOAT).into(),
        },
        Value::String(s) => s.clone().into(),
        Value::Array(a) => a.iter().map(from_json).collect::<Array>().into(),
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| (k.as_str().into(), from_json(v)))
            .collect::<Map>()
            .into(),
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::super::manifest::DEPLOY_BOARD;
    use super::*;

    /// A host that answers from a table and remembers what it was asked.
    #[derive(Default)]
    pub struct Fake {
        pub asked: Rc<RefCell<Vec<Request>>>,
        pub notices: Rc<RefCell<Vec<String>>>,
        pub token: Option<String>,
        pub answer: String,
    }

    impl Host for Fake {
        fn fetch(&mut self, req: &Request) -> Result<Response, String> {
            self.asked.borrow_mut().push(req.clone());
            Ok(Response {
                status: 200,
                body: self.answer.clone().into_bytes(),
            })
        }
        fn read_file(&mut self, path: &str) -> Result<String, String> {
            Ok(format!(r#"{{"projectId":"prj_{path}"}}"#))
        }
        fn git(&mut self) -> Option<GitInfo> {
            Some(GitInfo {
                branch: "main".into(),
                ..GitInfo::default()
            })
        }
        fn sessions(&mut self) -> Vec<SessionInfo> {
            Vec::new()
        }
        fn notice(&mut self, title: &str, _: &str) {
            self.notices.borrow_mut().push(title.into());
        }
        fn token(&mut self, _: &str) -> Token {
            self.token.clone().map_or(Token::Missing, Token::Found)
        }
    }

    pub const MAIN: &str = r#"
fn refresh(state) {
    let project = parse_json(read_file("vercel.json")).projectId;
    let r = fetch(#{
        url: `https://api.vercel.com/v6/deployments?projectId=${project}&limit=5`,
        auth: "vercel",
    });
    #{ deploys: r.json.deployments.map(|d| #{
        branch: d.meta.githubCommitRef, state: d.state, at: d.created,
    }) }
}

fn view(state, size) {
    if state.deploys.is_empty() { return empty("No deploys yet"); }
    list(state.deploys.map(|d| #{
        dot: if d.state == "READY" { "success" } else if d.state == "ERROR" { "error" } else { "link" },
        title: d.branch, detail: d.state, time: d.at,
    }))
}
"#;

    const ANSWER: &str = r#"{"deployments":[
        {"meta":{"githubCommitRef":"main"},"state":"READY","created":1700000000000},
        {"meta":{"githubCommitRef":"fix"},"state":"ERROR","created":1700000001000}]}"#;

    fn widget(src: &str, fake: Fake) -> Widget {
        let m = Manifest::parse(DEPLOY_BOARD).unwrap();
        let g = Grant::of(&m);
        Widget::new(m, src, g, Box::new(fake)).unwrap()
    }

    fn denied(w: &mut Widget) -> String {
        match w.refresh(&Map::new().into()) {
            Err(Failure::Stop(Stop::Denied(why))) => why,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_spec_example_runs() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut w = widget(
            MAIN,
            Fake {
                asked: asked.clone(),
                token: Some("tok".into()),
                answer: ANSWER.into(),
                ..Fake::default()
            },
        );
        let state = w.refresh(&Map::new().into()).unwrap();
        let req = &asked.borrow()[0];
        assert_eq!(req.host, "api.vercel.com");
        assert!(req.url.contains("projectId=prj_vercel.json"));
        assert_eq!(
            req.headers,
            [("Authorization".to_string(), "Bearer tok".to_string())]
        );
        let Block::List(rows) = w.view(&state, "wide").unwrap() else {
            panic!()
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].dot, Some(blocks::Tone::Error));
        assert_eq!(rows[0].time, "1700000000000");
    }

    #[test]
    fn a_missing_token_waits_for_the_user() {
        let mut w = widget(MAIN, Fake::default());
        assert_eq!(
            w.refresh(&Map::new().into()).unwrap_err(),
            Failure::Stop(Stop::NeedsToken("Needs your Vercel token".into()))
        );
    }

    #[test]
    fn a_host_it_did_not_name_stops_it_even_when_caught() {
        let mut w = widget(
            r#"fn refresh(s) { try { fetch("https://evil.example.com/x") } catch {} ; #{} }
               fn view(s, z) { empty("") }"#,
            Fake::default(),
        );
        assert_eq!(
            denied(&mut w),
            "It asked to reach evil.example.com, which you did not allow"
        );
    }

    #[test]
    fn the_token_goes_only_to_its_host() {
        let mut w = widget(
            r#"fn refresh(s) { fetch(#{ url: "https://api.github.com/x", auth: "vercel" }); #{} }
               fn view(s, z) { empty("") }"#,
            Fake {
                token: Some("tok".into()),
                ..Fake::default()
            },
        );
        assert!(denied(&mut w).contains("you allowed it only for api.vercel.com"));
    }

    #[test]
    fn files_git_and_sessions_need_their_grant() {
        for (call, word) in [
            (r#"read_file(".env")"#, ".env"),
            (r#"read_file("../vercel.json")"#, "../vercel.json"),
            ("sessions()", "sessions"),
            (r#"notice("a", "b")"#, "notices"),
        ] {
            let mut w = widget(
                &format!("fn refresh(s) {{ {call}; #{{}} }} fn view(s, z) {{ empty(\"\") }}"),
                Fake::default(),
            );
            assert!(denied(&mut w).contains(word), "{call}");
        }
    }

    #[test]
    fn view_has_no_host_functions() {
        let w = widget(
            r#"fn refresh(s) { #{} } fn view(s, z) { git(); empty("") }"#,
            Fake::default(),
        );
        assert!(matches!(
            w.view(&Map::new().into(), "wide"),
            Err(Failure::Stop(Stop::Denied(_)))
        ));
    }

    #[test]
    fn a_button_needs_its_action() {
        let w = widget(
            r#"fn refresh(s) { #{} }
               fn view(s, z) { button(#{ label: "Open", action: "open_url", url: "https://x.dev" }) }"#,
            Fake::default(),
        );
        assert!(matches!(
            w.view(&Map::new().into(), "wide"),
            Err(Failure::Stop(Stop::Denied(_)))
        ));
    }

    #[test]
    fn no_imports_no_eval_and_a_step_limit() {
        let m = Manifest::parse(DEPLOY_BOARD).unwrap();
        let g = Grant::of(&m);
        let import = r#"import "x" as x; fn refresh(s) { #{} } fn view(s, z) { empty("") }"#;
        // Rejected when compiled (no modules) or when run (no resolver).
        if let Ok(mut w) = Widget::new(m.clone(), import, g.clone(), Box::new(Fake::default())) {
            assert!(w.refresh(&Map::new().into()).is_ok());
        }
        assert!(
            Widget::new(
                m.clone(),
                r#"fn refresh(s) { eval("1") } fn view(s, z) { empty("") }"#,
                g.clone(),
                Box::new(Fake::default())
            )
            .is_err()
        );
        let mut w = widget(
            r#"fn refresh(s) { loop {} } fn view(s, z) { empty("") }"#,
            Fake::default(),
        );
        assert!(matches!(
            w.refresh(&Map::new().into()),
            Err(Failure::Limit(_))
        ));
    }

    #[test]
    fn state_is_data_only() {
        let mut w = widget(
            r#"fn refresh(s) { #{ f: |x| x } } fn view(s, z) { empty("") }"#,
            Fake::default(),
        );
        assert!(matches!(
            w.refresh(&Map::new().into()),
            Err(Failure::Error(_))
        ));
    }

    #[test]
    fn a_host_is_reached_a_few_times_per_window() {
        let mut w = widget(
            r#"fn refresh(s) { for i in 0..6 { fetch("https://api.github.com/x") } #{} }
               fn view(s, z) { empty("") }"#,
            Fake::default(),
        );
        assert!(
            matches!(w.refresh(&Map::new().into()), Err(Failure::Error(e)) if e.contains("5 times"))
        );
    }

    #[test]
    fn urls_are_checked() {
        assert_eq!(
            https_host("https://API.github.com:443/x").unwrap(),
            "api.github.com"
        );
        for bad in [
            "http://api.github.com/",
            "https://user@api.github.com/",
            "https://api.github.com:8443/",
            "https://127.0.0.1/",
            "https://localhost/",
            "https://api.github.com/ x",
        ] {
            assert!(https_host(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_preview_reads_the_sample_and_nothing_else() {
        let m = Manifest::parse(DEPLOY_BOARD).unwrap();
        let sample = r#"{"deploys":[{"branch":"main","state":"READY","at":"2m"}]}"#;
        let Block::List(rows) = preview(m.clone(), MAIN, sample, "wide").unwrap() else {
            panic!()
        };
        assert_eq!(rows[0].title, "main");
        assert!(preview(m, MAIN, "[]", "wide").is_err());
    }
}
