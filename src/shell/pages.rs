//! `arakun page`: agents look at and drive the browser tabs of their own environment
//! through the DevTools Protocol, the way a person at the window would.

use std::path::{Path, PathBuf};
use std::pin::pin;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use futures::channel::oneshot;
use futures::future::{Either, select};
use gpui::{AsyncWindowContext, Context, Entity, WeakEntity, Window};
use serde_json::{Value, json};

use super::Shell;
use crate::browser::{BrowserView, ConsoleLine, PageState};
use crate::control::{self, Reply, Request};
use crate::model::TabKind;

/// How long a command waits for the page to finish loading.
const LOAD_WAIT: Duration = Duration::from_secs(15);
/// How long a single DevTools command may take.
const CALL_WAIT: Duration = Duration::from_secs(15);
/// After a click or a key, for the page to react (start a navigation, log errors).
const SETTLE: Duration = Duration::from_millis(400);
const MAX_OUTPUT: usize = 40_000;
const CONSOLE_LINES: usize = 200;
/// Screenshots kept in the default folder; older ones are deleted.
const KEEP_SHOTS: usize = 30;
const CLOSED: &str = "the tab was closed";

#[derive(Debug, PartialEq)]
enum Action {
    Screenshot(Option<PathBuf>),
    Console,
    Text,
    Open(String),
    Reload,
    Eval(String),
    Click(String),
    Type(String, String),
    Press(String),
}

impl Action {
    /// Acts on the page as its user, so it is kept to pages served from this
    /// machine: the browser profile is the user's, logged in to their accounts.
    fn drives(&self) -> bool {
        matches!(
            self,
            Action::Eval(_) | Action::Click(_) | Action::Type(..) | Action::Press(_)
        )
    }
}

/// `[command, args…]` as `arakun page` sends them; `None` lists the tabs.
fn parse_action(args: &[String], cwd: &Path) -> Result<Option<Action>, String> {
    let Some((sub, rest)) = args.split_first() else {
        return Ok(None);
    };
    let action = match (sub.as_str(), rest) {
        ("" | "ls" | "list", []) => return Ok(None),
        ("screenshot" | "shot", []) => Action::Screenshot(None),
        ("screenshot" | "shot", [file]) => Action::Screenshot(Some(cwd.join(file))),
        ("console", []) => Action::Console,
        ("text", []) => Action::Text,
        ("reload", []) => Action::Reload,
        ("open", [url]) => Action::Open(control::browse_target(cwd, url)),
        // A handle is not the value it stands for; typed, it would fail or leak.
        ("eval", [_, ..]) => {
            crate::agent::secret::refuse_handle(&rest.join(" "))?;
            Action::Eval(rest.join(" "))
        }
        ("click", [_, ..]) => Action::Click(rest.join(" ")),
        ("type", [field, text @ ..]) => {
            crate::agent::secret::refuse_handle(&text.join(" "))?;
            Action::Type(field.clone(), text.join(" "))
        }
        ("press", [key]) => Action::Press(key.clone()),
        _ => return Err(format!("usage:\n{}", control::PAGE_USAGE)),
    };
    Ok(Some(action))
}

type Page = WeakEntity<BrowserView>;

impl Shell {
    /// `arakun page`: answers through `done` once the page did what was asked.
    pub(super) fn page_command(
        &mut self,
        req: Request,
        done: oneshot::Sender<Reply>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (view, action, fresh) = match self.page_target(&req, window, cx) {
            Ok(target) => target,
            Err(reply) => {
                let _ = done.send(reply);
                return;
            }
        };
        view.update(cx, |v, _| v.hold_awake(true));
        self.sync_browsers(cx);
        let page = view.downgrade();
        cx.spawn_in(window, async move |_, cx| {
            let reply = match run(&page, action, fresh, cx).await {
                Ok(out) => Reply::ok(out),
                Err(e) => Reply::err(e),
            };
            let _ = page.update(cx, |v, _| v.hold_awake(false));
            let _ = done.send(reply);
        })
        .detach();
    }

    /// The page a command acts on, whether its tab was just opened for it, and
    /// what to do. `Err` is the answer to send right away.
    fn page_target(
        &mut self,
        req: &Request,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(Entity<BrowserView>, Action, bool), Reply> {
        let env_path = self
            .command_env(req)
            .ok_or_else(|| Reply::err("no project is open in arakun"))?;
        let place = self.env_label(&env_path);
        let action = match parse_action(req.args.get(1..).unwrap_or_default(), &req.cwd) {
            Ok(Some(action)) => action,
            Ok(None) => return Err(Reply::ok(self.page_listing(&env_path, &place, cx))),
            Err(e) => return Err(Reply::err(e)),
        };
        if matches!(&action, Action::Open(target) if crate::browser::is_debug_endpoint(target)) {
            return Err(Reply::err(DEBUG_PORT_REFUSED));
        }
        let tabs = self.browser_tabs(&env_path);
        let mut fresh = false;
        let index = match req.args.first().map_or("", String::as_str) {
            "" => match (self.current_browser_tab(&env_path), &action) {
                (Some(i), _) => i,
                (None, Action::Open(url)) => {
                    let id = self
                        .push_browser_tab(&env_path, url.clone(), false, false, window, cx)
                        .ok_or_else(|| Reply::err("could not open a browser tab"))?;
                    fresh = true;
                    self.strip
                        .position(id)
                        .ok_or_else(|| Reply::err("could not open a browser tab"))?
                }
                (None, _) => {
                    return Err(Reply::err(format!(
                        "no browser tab is open in {place}; `arakun page open <url>` opens one"
                    )));
                }
            },
            n => match n
                .parse::<usize>()
                .ok()
                .and_then(|n| tabs.get(n.checked_sub(1)?))
            {
                Some(&i) => i,
                None => {
                    let listing = self.page_listing(&env_path, &place, cx);
                    return Err(Reply::err(format!(
                        "there is no browser tab {n}\n{listing}"
                    )));
                }
            },
        };
        let view = self
            .ensure_page(index, window, cx)
            .ok_or_else(|| Reply::err("could not open the page"))?;
        let url = view.read(cx).url();
        if !matches!(action, Action::Open(_)) && crate::browser::is_debug_endpoint(&url) {
            return Err(Reply::err(DEBUG_PORT_REFUSED));
        }
        if action.drives() && !is_local(&url) && !crate::page_drive::any_page() {
            let sub = req.args.get(1).map_or("", String::as_str);
            return Err(Reply::err(format!(
                "`arakun page {sub}` only works on pages served from this machine (localhost, file://); this tab shows {url}"
            )));
        }
        Ok((view, action, fresh))
    }

    /// Indices of an environment's browser tabs, in tab order.
    fn browser_tabs(&self, env_path: &Path) -> Vec<usize> {
        let tabs = &self.strip.tabs;
        (0..tabs.len())
            .filter(|&i| tabs[i].is_in(env_path) && matches!(tabs[i].kind, TabKind::Browser { .. }))
            .collect()
    }

    /// The browser tab commands act on by default: the one on screen in that
    /// environment, else the one used last.
    fn current_browser_tab(&self, env_path: &Path) -> Option<usize> {
        let strip = &self.strip;
        let is_browser = |i: &usize| {
            strip
                .tabs
                .get(*i)
                .is_some_and(|t| t.is_in(env_path) && matches!(t.kind, TabKind::Browser { .. }))
        };
        let recent = strip
            .tab_back
            .iter()
            .rev()
            .filter_map(|id| strip.position(*id));
        std::iter::once(strip.active_tab)
            .chain(strip.split_other())
            .chain(strip.front_of(env_path))
            .chain(recent)
            .find(is_browser)
            .or_else(|| (0..strip.tabs.len()).rev().find(is_browser))
    }

    fn page_listing(&self, env_path: &Path, place: &str, cx: &gpui::App) -> String {
        let tabs = self.browser_tabs(env_path);
        if tabs.is_empty() {
            return format!(
                "no browser tabs are open in {place}; `arakun page open <url>` opens one"
            );
        }
        let current = self.current_browser_tab(env_path);
        let mut out = format!("browser tabs in {place}:\n");
        for (n, &i) in tabs.iter().enumerate() {
            let tab = &self.strip.tabs[i];
            let (title, url, state) = match (&tab.browser, &tab.kind) {
                (Some(b), _) => {
                    let view = b.view.read(cx);
                    let s = view.page_state();
                    let state = if s.error.is_some() {
                        "  (did not load)"
                    } else if s.loading {
                        "  (loading)"
                    } else {
                        ""
                    };
                    (view.title(), view.url(), state)
                }
                (None, TabKind::Browser { url }) => {
                    (tab.title.clone(), url.clone(), "  (not loaded yet)")
                }
                (None, _) => continue,
            };
            let mark = if current == Some(i) { "*" } else { " " };
            let title = if title == url {
                String::new()
            } else {
                format!("{title}  ")
            };
            out.push_str(&format!("{mark} {}  {title}{url}{state}\n", n + 1));
        }
        out.push_str(
            "Commands act on the tab marked *; `arakun page -t N <command>` picks another.",
        );
        out
    }
}

async fn run(
    page: &Page,
    action: Action,
    fresh: bool,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    match action {
        Action::Open(url) => {
            if fresh {
                return loaded(page, None, cx).await;
            }
            let loads = opened(page, cx).await?;
            page.update(cx, |v, cx| v.navigate(&url, cx))
                .map_err(|_| CLOSED.to_string())?;
            loaded(page, Some(loads), cx).await
        }
        Action::Reload => {
            let loads = opened(page, cx).await?;
            page.update(cx, |v, cx| v.reload(cx))
                .map_err(|_| CLOSED.to_string())?;
            loaded(page, Some(loads), cx).await
        }
        action => {
            let finished = wait_load(page, None, cx).await?;
            // The check in `page_target` read the address before the wait, and a
            // navigation under way still shows the old one: check again here, on the
            // document the action will run in.
            let pin = if action.drives() && !crate::page_drive::any_page() {
                may_drive(finished, state(page, cx)?.dialog.as_deref())?;
                Some(pin_local(page, cx).await?)
            } else {
                None
            };
            let mut out = act(page, action, pin.as_ref(), cx).await?;
            if !finished {
                out.push_str(&format!(
                    "\n(the page was still loading after {} s)",
                    LOAD_WAIT.as_secs()
                ));
            }
            Ok(out)
        }
    }
}

/// `pin`: the local document a driving action was checked against, which its
/// scripts and input must reach; `None` when any page may be driven.
async fn act(
    page: &Page,
    action: Action,
    pin: Option<&Doc>,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    match action {
        Action::Screenshot(path) => screenshot(page, path, cx).await,
        Action::Console => Ok(console_text(&read(page, cx, |v| v.console())?)),
        Action::Text => {
            let text = evaluate(page, TEXT_JS.to_string(), None, cx).await?;
            let (title, url) = read(page, cx, |v| (v.title(), v.url()))?;
            Ok(clip(format!("{title}\n{url}\n\n{}", value_text(&text))))
        }
        Action::Eval(js) => Ok(clip(value_text(&evaluate(page, js, pin, cx).await?))),
        Action::Click(target) => click(page, &target, pin, cx).await,
        Action::Type(field, text) => type_into(page, &field, &text, pin, cx).await,
        Action::Press(key) => press(page, &key, pin, cx).await,
        Action::Open(_) | Action::Reload => Err("unexpected navigation".into()),
    }
}

fn read<R>(
    page: &Page,
    cx: &AsyncWindowContext,
    f: impl FnOnce(&BrowserView) -> R,
) -> Result<R, String> {
    page.read_with(cx, |v, _| f(v))
        .map_err(|_| CLOSED.to_string())
}

fn state(page: &Page, cx: &AsyncWindowContext) -> Result<PageState, String> {
    read(page, cx, |v| v.page_state())
}

/// Waits until the page is open and done loading; with `after`, until a load
/// past that count finished. `Ok(false)`: still loading (or held by a dialog).
async fn wait_load(
    page: &Page,
    after: Option<u64>,
    cx: &mut AsyncWindowContext,
) -> Result<bool, String> {
    let deadline = Instant::now() + LOAD_WAIT;
    loop {
        let s = state(page, cx)?;
        if !s.open
            && let Some(e) = s.error
        {
            return Err(e.to_string());
        }
        if s.open && !s.loading && after.is_none_or(|n| s.loads > n) {
            return Ok(true);
        }
        if s.dialog.is_some() || Instant::now() >= deadline {
            return Ok(false);
        }
        cx.background_executor()
            .timer(Duration::from_millis(50))
            .await;
    }
}

/// Lets a page that is still opening finish its first load before it is sent
/// elsewhere (Chromium reports that load's console messages twice otherwise);
/// returns the load count.
async fn opened(page: &Page, cx: &mut AsyncWindowContext) -> Result<u64, String> {
    let s = state(page, cx)?;
    if !s.open || s.loads == 0 {
        wait_load(page, None, cx).await?;
    }
    Ok(state(page, cx)?.loads)
}

/// Waits for the load a navigation started and says how it went.
async fn loaded(
    page: &Page,
    after: Option<u64>,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    let finished = wait_load(page, after, cx).await?;
    // Scripts that run right after the load may still log their errors.
    cx.background_executor().timer(SETTLE).await;
    let (title, url, s, console) = read(page, cx, |v| {
        (v.title(), v.url(), v.page_state(), v.console())
    })?;
    if let Some(e) = s.error {
        return Err(format!("{url} did not load: {e}"));
    }
    let verb = if finished { "loaded" } else { "still loading" };
    Ok(format!("{verb} {url} · {title}{}", problems(&console, 0)))
}

async fn call(
    page: &Page,
    method: &str,
    params: Value,
    cx: &mut AsyncWindowContext,
) -> Result<Value, String> {
    let answer = page
        .update(cx, |v, _| v.devtools(method, params))
        .map_err(|_| CLOSED.to_string())?;
    let timer = cx.background_executor().timer(CALL_WAIT);
    match select(answer, pin!(timer)).await {
        Either::Left((Ok(result), _)) => result,
        Either::Left((Err(_), _)) => Err(CLOSED.into()),
        Either::Right(_) => Err(match state(page, cx).ok().and_then(|s| s.dialog) {
            Some(message) => {
                format!("the page is waiting on a dialog (\"{message}\"); answer it in arakun")
            }
            None => format!(
                "the page did not answer {method} within {} s",
                CALL_WAIT.as_secs()
            ),
        }),
    }
}

/// Runs JavaScript in the page; the result as a DevTools `RemoteObject`. With
/// `pin`, only in that document: once the page navigated, its context is gone and
/// Chromium refuses the call.
async fn evaluate(
    page: &Page,
    expression: String,
    pin: Option<&Doc>,
    cx: &mut AsyncWindowContext,
) -> Result<Value, String> {
    let mut params = json!({
        "expression": expression,
        "returnByValue": true,
        "awaitPromise": true,
        "userGesture": true,
        "replMode": true,
    });
    if let Some(pin) = pin {
        params["uniqueContextId"] = json!(pin.context);
    }
    let mut result = call(page, "Runtime.evaluate", params, cx).await?;
    if let Some(ex) = result.get("exceptionDetails") {
        let text = ex["exception"]["description"]
            .as_str()
            .or_else(|| ex["text"].as_str())
            .unwrap_or("an exception");
        return Err(format!("the script threw: {text}"));
    }
    Ok(result
        .get_mut("result")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

/// The document a driving action is checked against.
#[derive(Clone, Debug, PartialEq)]
struct Doc {
    /// The unique id of its main-world script context. A navigation, even to the
    /// same address, gives the next document a new one.
    context: String,
    origin: String,
    url: String,
}

/// A driving action needs the page loaded: while it loads, the address it shows is
/// the one it is leaving. `dialog`: the message of a dialog the page waits on.
fn may_drive(finished: bool, dialog: Option<&str>) -> Result<(), String> {
    match (finished, dialog) {
        (true, _) => Ok(()),
        (false, Some(message)) => Err(format!(
            "the page is waiting on a dialog (\"{message}\"); answer it in arakun"
        )),
        (false, None) => Err(format!(
            "the page was still loading after {} s, so arakun did not act on it; try again once it has loaded",
            LOAD_WAIT.as_secs()
        )),
    }
}

/// Whether a driving action may reach `doc`. `loading`: a navigation is under way,
/// so the page may turn into another one at any moment. `pin`: the document the
/// action started on, which it must still be (its address may change in place).
fn check_doc(doc: &Doc, loading: bool, pin: Option<&Doc>) -> Result<(), String> {
    if let Some(pin) = pin
        && pin.context != doc.context
    {
        return Err(format!(
            "the page changed to {} during the action, so arakun stopped",
            doc.url
        ));
    }
    if !doc_is_local(&doc.origin, &doc.url) {
        return Err(format!(
            "`arakun page eval`, `click`, `type` and `press` only work on pages served from this machine (localhost, file://); this tab shows {}",
            doc.url
        ));
    }
    if loading {
        return Err(format!(
            "{} is loading another page, so arakun did not act on it; try again once it has loaded",
            doc.url
        ));
    }
    Ok(())
}

/// The address and the origin must both be local: a blank page a site opened has
/// that site's origin.
fn doc_is_local(origin: &str, url: &str) -> bool {
    is_local(url) && (matches!(origin, "null" | "file://") || is_local(origin))
}

/// The document the tab shows now, as Chromium sees it, and whether the tab is
/// loading another one.
async fn locate(page: &Page, cx: &mut AsyncWindowContext) -> Result<(Doc, bool), String> {
    let tree = call(page, "Page.getFrameTree", json!({}), cx).await?;
    let frame = tree["frameTree"]["frame"]["id"]
        .as_str()
        .ok_or("Chromium sent no frame")?
        .to_string();
    let mut context = read(page, cx, |v| v.script_context(&frame))?;
    if context.is_none() {
        // Chromium makes a page's script context on first use, and reports it then.
        call(page, "Runtime.evaluate", json!({ "expression": "0" }), cx).await?;
        context = read(page, cx, |v| v.script_context(&frame))?;
    }
    let context = context.ok_or("the page has no script context yet; try again")?;
    let doc = Doc {
        context,
        origin: String::new(),
        url: String::new(),
    };
    // Read in that context, so the address is the one of the document scripts reach.
    let place = evaluate(
        page,
        "location.origin + ' ' + location.href".into(),
        Some(&doc),
        cx,
    )
    .await?;
    let place = place["value"].as_str().unwrap_or_default();
    let (origin, url) = place.split_once(' ').unwrap_or(("", place));
    let doc = Doc {
        origin: origin.to_string(),
        url: url.to_string(),
        ..doc
    };
    Ok((doc, state(page, cx)?.loading))
}

/// Checks the page a driving action is about to reach and pins its document.
async fn pin_local(page: &Page, cx: &mut AsyncWindowContext) -> Result<Doc, String> {
    // Makes Chromium report the page's script contexts.
    call(page, "Runtime.enable", json!({}), cx).await?;
    let (doc, loading) = locate(page, cx).await?;
    check_doc(&doc, loading, None)?;
    Ok(doc)
}

/// Sends an input event. Chromium gives it to whatever document the tab shows when
/// it arrives, which no context id can pin, so with `pin` the tab is checked right
/// before: still the pinned document, and for the event that starts a gesture
/// (`starts`), not loading another one. The events that end a gesture may follow a
/// navigation the gesture itself started. A navigation that commits in the one round
/// trip between that check and the event still gets it; `delivered` reports that.
async fn input(
    page: &Page,
    method: &str,
    params: Value,
    pin: Option<&Doc>,
    starts: bool,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    if let Some(pin) = pin {
        let (doc, loading) = locate(page, cx).await?;
        check_doc(&doc, loading && starts, Some(pin))?;
    }
    call(page, method, params, cx).await.map(drop)
}

/// After a gesture's input: an error if the page turned into another document while
/// it was sent, as part of it may have reached that one.
async fn delivered(
    page: &Page,
    pin: Option<&Doc>,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    let Some(pin) = pin else {
        return Ok(());
    };
    let (doc, _) = locate(page, cx).await?;
    if doc.context != pin.context {
        return Err(format!(
            "the page changed to {} while arakun sent the input; part of it may have reached that page",
            doc.url
        ));
    }
    Ok(())
}

/// Calls one of the functions below with `args`, the helpers in scope.
fn script(function: &str, args: &[Value]) -> String {
    let args: Vec<String> = args.iter().map(Value::to_string).collect();
    format!(
        "(() => {{\n{HELPERS}\nreturn ({function})({});\n}})()",
        args.join(", ")
    )
}

/// The value a script returned, as `arakun page` prints it.
fn value_text(object: &Value) -> String {
    if let Some(v) = object.get("unserializableValue").and_then(Value::as_str) {
        return v.to_string();
    }
    match object.get("value") {
        Some(Value::String(s)) => s.clone(),
        Some(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
        None => object["description"]
            .as_str()
            .or_else(|| object["type"].as_str())
            .unwrap_or("undefined")
            .to_string(),
    }
}

async fn screenshot(
    page: &Page,
    path: Option<PathBuf>,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    let shot = call(
        page,
        "Page.captureScreenshot",
        json!({ "format": "png" }),
        cx,
    )
    .await?;
    let data = shot["data"].as_str().ok_or("Chromium sent no image")?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| e.to_string())?;
    let path = match path {
        Some(p) if p.is_dir() => p.join(shot_name()),
        Some(p) => p,
        None => shots_dir()?.join(shot_name()),
    };
    std::fs::write(&path, &png).map_err(|e| format!("{}: {e}", path.display()))?;
    let url = read(page, cx, |v| v.url())?;
    let size = png_size(&png)
        .map(|(w, h)| format!("  {w}×{h}"))
        .unwrap_or_default();
    Ok(format!("{}{size}  {url}", path.display()))
}

fn shot_name() -> String {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("page-{ms}.png")
}

/// Where screenshots go unless a file is given, pruned to the newest `KEEP_SHOTS`.
fn shots_dir() -> Result<PathBuf, String> {
    let dir = control::runtime_dir().join("shots");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut shots: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("page-"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    shots.sort();
    let excess = (shots.len() + 1).saturating_sub(KEEP_SHOTS);
    for (_, old) in shots.into_iter().take(excess) {
        let _ = std::fs::remove_file(old);
    }
    Ok(dir)
}

/// Width and height from a PNG's header.
fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let word = |at: usize| Some(u32::from_be_bytes(png.get(at..at + 4)?.try_into().ok()?));
    Some((word(16)?, word(20)?))
}

/// What to compare against after an action: the page before it.
struct Before {
    loads: u64,
    seq: u64,
    url: gpui::SharedString,
}

fn snapshot(page: &Page, cx: &AsyncWindowContext) -> Result<Before, String> {
    read(page, cx, |v| Before {
        loads: v.page_state().loads,
        seq: v.console().last().map_or(0, |l| l.seq),
        url: v.url(),
    })
}

/// Gives the page a moment to react to an action, waits for a navigation it
/// started, and reports what changed.
async fn settle(
    page: &Page,
    before: Before,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    cx.background_executor().timer(SETTLE).await;
    let mut out = String::new();
    let s = state(page, cx)?;
    if (s.loading || s.loads > before.loads)
        && !wait_load(page, None, cx).await?
        && s.dialog.is_none()
    {
        out.push_str("\nthe page is still loading");
    }
    let (url, s, console) = read(page, cx, |v| (v.url(), v.page_state(), v.console()))?;
    if url != before.url {
        out.push_str(&format!("\nnow at {url}"));
    }
    if let Some(message) = s.dialog {
        out.push_str(&format!(
            "\nthe page opened a dialog: \"{message}\" (answer it in arakun)"
        ));
    }
    out.push_str(&problems(&console, before.seq));
    Ok(out)
}

async fn click(
    page: &Page,
    target: &str,
    pin: Option<&Doc>,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    let found = evaluate(page, script(CLICK_JS, &[json!(target)]), pin, cx).await?;
    let Some(el) = found.get("value").filter(|v| v.is_object()) else {
        return Err(format!(
            "nothing on the page matches `{target}` (a CSS selector, or the text of a button or link)"
        ));
    };
    let what = el["el"].as_str().unwrap_or("the element").to_string();
    if el["visible"] != true {
        return Err(format!("{what} is not visible"));
    }
    let (x, y) = (
        el["x"].as_f64().unwrap_or(0.),
        el["y"].as_f64().unwrap_or(0.),
    );
    let before = snapshot(page, cx)?;
    for (kind, button, buttons) in [
        ("mouseMoved", "none", 0),
        ("mousePressed", "left", 1),
        ("mouseReleased", "left", 0),
    ] {
        let params = json!({ "type": kind, "x": x, "y": y, "button": button, "buttons": buttons, "clickCount": 1 });
        let starts = kind != "mouseReleased";
        input(page, "Input.dispatchMouseEvent", params, pin, starts, cx).await?;
    }
    delivered(page, pin, cx).await?;
    let mut out = format!("clicked {what}");
    if let Some(cover) = el["covered"].as_str() {
        out.push_str(&format!("\n{cover} covers it, so the click landed there"));
    }
    out.push_str(&settle(page, before, cx).await?);
    Ok(out)
}

async fn type_into(
    page: &Page,
    field: &str,
    text: &str,
    pin: Option<&Doc>,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    let found = evaluate(
        page,
        script(FOCUS_JS, &[json!(field), json!(text)]),
        pin,
        cx,
    )
    .await?;
    let Some(el) = found.get("value").filter(|v| v.is_object()) else {
        return Err(format!(
            "no field on the page matches `{field}` (a CSS selector, or a field's label, placeholder, name or id)"
        ));
    };
    let what = el["el"].as_str().unwrap_or("the field").to_string();
    let before = snapshot(page, cx)?;
    let mut out = if let Some(options) = el["options"].as_array() {
        let options: Vec<&str> = options.iter().filter_map(Value::as_str).collect();
        return Err(format!(
            "{what} has no option `{text}`; it offers: {}",
            options.join(", ")
        ));
    } else if let Some(chosen) = el["chosen"].as_str() {
        format!("picked \"{chosen}\" in {what}")
    } else {
        if el["focused"] != true {
            return Err(format!("{what} did not take the focus"));
        }
        if text.is_empty() {
            send_key(page, BACKSPACE, pin, cx).await?;
        } else {
            let params = json!({ "text": text });
            input(page, "Input.insertText", params, pin, true, cx).await?;
            delivered(page, pin, cx).await?;
        }
        let value = evaluate(page, VALUE_JS.to_string(), pin, cx).await?;
        let value = value.get("value").cloned().unwrap_or(Value::Null);
        format!(
            "typed into {what}; it now holds {}",
            clip_to(value.to_string(), 300)
        )
    };
    out.push_str(&settle(page, before, cx).await?);
    Ok(out)
}

async fn press(
    page: &Page,
    name: &str,
    pin: Option<&Doc>,
    cx: &mut AsyncWindowContext,
) -> Result<String, String> {
    let k = key(name)
        .ok_or_else(|| format!("unknown key `{name}`; arakun page press knows {KEY_NAMES}"))?;
    let before = snapshot(page, cx)?;
    send_key(page, k, pin, cx).await?;
    let mut out = format!("pressed {}", k.code);
    out.push_str(&settle(page, before, cx).await?);
    if let Ok(focus) = evaluate(page, script(FOCUSED_JS, &[]), pin, cx).await
        && let Some(focus) = focus["value"].as_str()
    {
        out.push_str(&format!("\nthe focus is on {focus}"));
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    key: &'static str,
    code: &'static str,
    vk: i32,
    text: Option<&'static str>,
}

const BACKSPACE: Key = Key {
    key: "Backspace",
    code: "Backspace",
    vk: 8,
    text: None,
};
const KEY_NAMES: &str = "Enter, Tab, Escape, Backspace, Delete, Space, ArrowUp/Down/Left/Right, Home, End, PageUp, PageDown";

fn key(name: &str) -> Option<Key> {
    let k = |key, code, vk, text| {
        Some(Key {
            key,
            code,
            vk,
            text,
        })
    };
    match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => k("Enter", "Enter", 13, Some("\r")),
        "tab" => k("Tab", "Tab", 9, None),
        "escape" | "esc" => k("Escape", "Escape", 27, None),
        "backspace" => Some(BACKSPACE),
        "delete" | "del" => k("Delete", "Delete", 46, None),
        "space" => k(" ", "Space", 32, Some(" ")),
        "arrowup" | "up" => k("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => k("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => k("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => k("ArrowRight", "ArrowRight", 39, None),
        "home" => k("Home", "Home", 36, None),
        "end" => k("End", "End", 35, None),
        "pageup" => k("PageUp", "PageUp", 33, None),
        "pagedown" => k("PageDown", "PageDown", 34, None),
        _ => None,
    }
}

async fn send_key(
    page: &Page,
    k: Key,
    pin: Option<&Doc>,
    cx: &mut AsyncWindowContext,
) -> Result<(), String> {
    // A keyDown with text also types it (Enter's "\r" submits forms); rawKeyDown does not.
    let mut down = json!({
        "type": if k.text.is_some() { "keyDown" } else { "rawKeyDown" },
        "key": k.key,
        "code": k.code,
        "windowsVirtualKeyCode": k.vk,
        "nativeVirtualKeyCode": k.vk,
    });
    if let Some(text) = k.text {
        down["text"] = json!(text);
        down["unmodifiedText"] = json!(text);
    }
    input(page, "Input.dispatchKeyEvent", down, pin, true, cx).await?;
    let up = json!({ "type": "keyUp", "key": k.key, "code": k.code, "windowsVirtualKeyCode": k.vk, "nativeVirtualKeyCode": k.vk });
    input(page, "Input.dispatchKeyEvent", up, pin, false, cx).await?;
    delivered(page, pin, cx).await
}

/// Errors and warnings logged after `since`, for the answer to an action.
fn problems(console: &[ConsoleLine], since: u64) -> String {
    let new: Vec<&ConsoleLine> = console
        .iter()
        .filter(|l| l.seq > since && matches!(l.level, "error" | "warn"))
        .collect();
    if new.is_empty() {
        return String::new();
    }
    let errors = new.iter().filter(|l| l.level == "error").count();
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let counts = match (errors, new.len() - errors) {
        (e, 0) => plural(e, "error"),
        (0, w) => plural(w, "warning"),
        (e, w) => format!("{}, {}", plural(e, "error"), plural(w, "warning")),
    };
    let mut out = format!("\n{counts} in the console:");
    for line in new.iter().take(5) {
        out.push_str(&format!("\n  {}", clip_to(console_line(line), 300)));
    }
    if new.len() > 5 {
        out.push_str("\n  … `arakun page console` shows them all");
    }
    out
}

fn console_text(lines: &[ConsoleLine]) -> String {
    if lines.is_empty() {
        return "nothing was logged since the page loaded".into();
    }
    let skip = lines.len().saturating_sub(CONSOLE_LINES);
    let mut out = String::new();
    if skip > 0 {
        out.push_str(&format!("({skip} earlier messages not shown)\n"));
    }
    for line in &lines[skip..] {
        out.push_str(&console_line(line));
        out.push('\n');
    }
    clip(out.trim_end().to_string())
}

fn console_line(line: &ConsoleLine) -> String {
    let text = line.text.trim_end();
    match line.source.as_str() {
        "" => format!("{:<5} {text}", line.level),
        source => format!("{:<5} {text}  ({source})", line.level),
    }
}

fn clip(text: String) -> String {
    clip_to(text, MAX_OUTPUT)
}

fn clip_to(mut text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let rest = text.len() - cut;
    text.truncate(cut);
    text.push_str(&format!("… ({rest} more bytes)"));
    text
}

/// A page on the browser's debugging port can drive every tab and read local files, so
/// agents may neither open it nor read or drive it, whatever the settings say.
const DEBUG_PORT_REFUSED: &str =
    "agents may not use the browser's debugging port (ARAKUN_CDP_PORT)";

/// Pages served from this machine: `file://`, `about:`, `data:`, and http(s) on
/// localhost or a loopback address.
fn is_local(url: &str) -> bool {
    if url.is_empty()
        || ["file:", "about:", "data:"]
            .iter()
            .any(|s| url.starts_with(s))
    {
        return true;
    }
    let Some(rest) = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    }
    .to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

const TEXT_JS: &str = "(document.body || document.documentElement).innerText";

const VALUE_JS: &str = "(() => { const e = document.activeElement; \
    return e && typeof e.value === 'string' ? e.value : e ? e.innerText : null; })()";

const HELPERS: &str = r#"
const arakunNorm = (s) => (s || '').replace(/\s+/g, ' ').trim().toLowerCase();
const arakunDescribe = (e) => {
  let s = e.tagName.toLowerCase();
  if (e.id) s += '#' + e.id;
  const cls = typeof e.className === 'string' ? e.className.trim().split(/\s+/).filter(Boolean).slice(0, 2) : [];
  if (cls.length) s += '.' + cls.join('.');
  const shown = e.tagName === 'SELECT' ? (e.selectedOptions[0] || {}).text : e.innerText;
  const t = (shown || e.getAttribute('aria-label') || e.getAttribute('placeholder') || e.value || '')
    .replace(/\s+/g, ' ').trim();
  return t ? s + ' "' + (t.length > 40 ? t.slice(0, 40) + '…' : t) + '"' : s;
};
const arakunFind = (query, fields) => {
  try { const e = document.querySelector(query); if (e) return e; } catch (_) {}
  const want = arakunNorm(query);
  const sel = fields
    ? 'input:not([type=hidden]),textarea,select,[contenteditable=""],[contenteditable=true]'
    : 'a,button,summary,label,input[type=submit],input[type=button],input[type=checkbox],input[type=radio],' +
      '[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=option],[onclick]';
  const all = [...document.querySelectorAll(sel)].filter((e) => e.getClientRects().length > 0);
  const names = (e) => {
    const out = [e.id, e.getAttribute('aria-label'), e.getAttribute('title'), e.getAttribute('placeholder'), e.getAttribute('name')];
    if (!fields) out.push(e.innerText, e.value);
    for (const l of e.labels || []) out.push(l.innerText);
    return out.map(arakunNorm).filter(Boolean);
  };
  return all.find((e) => names(e).includes(want)) || all.find((e) => names(e).some((n) => n.includes(want))) || null;
};"#;

const CLICK_JS: &str = r#"(query) => {
  const el = arakunFind(query, false);
  if (!el) return null;
  el.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' });
  const r = el.getBoundingClientRect();
  const visible = r.width > 0 && r.height > 0;
  const x = r.left + r.width / 2, y = r.top + r.height / 2;
  const hit = visible ? document.elementFromPoint(x, y) : null;
  const covered = hit && hit !== el && !el.contains(hit) ? arakunDescribe(hit) : null;
  return { x, y, visible, el: arakunDescribe(el), covered };
}"#;

const FOCUS_JS: &str = r#"(query, text) => {
  const el = arakunFind(query, true);
  if (!el) return null;
  el.scrollIntoView({ block: 'center', behavior: 'instant' });
  if (el.tagName === 'SELECT') {
    const want = arakunNorm(text);
    const opts = [...el.options];
    const opt = opts.find((o) => arakunNorm(o.text) === want || arakunNorm(o.value) === want)
      || opts.find((o) => arakunNorm(o.text).includes(want));
    if (!opt) return { el: arakunDescribe(el), options: opts.map((o) => o.text.trim()) };
    el.value = opt.value;
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    return { el: arakunDescribe(el), chosen: opt.text.trim() };
  }
  el.focus();
  try {
    if (typeof el.select === 'function') el.select();
    else if (el.isContentEditable) {
      const r = document.createRange();
      r.selectNodeContents(el);
      const s = getSelection();
      s.removeAllRanges();
      s.addRange(r);
    }
  } catch (_) {}
  return { el: arakunDescribe(el), focused: el === document.activeElement || el.contains(document.activeElement) };
}"#;

const FOCUSED_JS: &str = "() => { const e = document.activeElement; \
    return e && e !== document.body ? arakunDescribe(e) : null; }";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pages_on_this_machine_are_local() {
        for url in [
            "http://localhost:3000/",
            "https://app.localhost/x",
            "http://127.0.0.1:8080",
            "http://[::1]:5173/",
            "http://0.0.0.0:8000/a?b",
            "http://user@localhost/",
            "file:///home/me/index.html",
            "about:blank",
            "",
        ] {
            assert!(is_local(url), "{url}");
        }
        for url in [
            "https://github.com/",
            "http://127.evil.com/",
            "http://localhost.evil.com/",
            "chrome://version",
        ] {
            assert!(!is_local(url), "{url}");
        }
    }

    #[test]
    fn driving_needs_the_loaded_local_document_it_checked() {
        assert_eq!(may_drive(true, None), Ok(()));
        assert!(
            may_drive(false, None)
                .unwrap_err()
                .contains("still loading")
        );
        assert!(
            may_drive(false, Some("Sure?"))
                .unwrap_err()
                .contains("dialog")
        );

        let doc = |context: &str, origin: &str, url: &str| Doc {
            context: context.into(),
            origin: origin.into(),
            url: url.into(),
        };
        let local = doc("a", "http://localhost:3000", "http://localhost:3000/");
        assert_eq!(check_doc(&local, false, None), Ok(()));
        // A navigation under way: the address shown is the one the page leaves.
        assert!(
            check_doc(&local, true, None)
                .unwrap_err()
                .contains("loading")
        );
        // Checked again where the action runs, the page is elsewhere by now.
        let remote = doc("b", "https://github.com", "https://github.com/");
        assert!(
            check_doc(&remote, false, None)
                .unwrap_err()
                .contains("only work")
        );
        assert!(check_doc(&remote, false, Some(&local)).is_err());
        // Another document, even a local one, is not the one that was checked.
        let next = doc("c", "http://localhost:3000", "http://localhost:3000/");
        assert!(
            check_doc(&next, false, Some(&local))
                .unwrap_err()
                .contains("changed")
        );
        // The same document may change its address in place, and finish a gesture
        // while a navigation it started is under way.
        let moved = doc("a", "http://localhost:3000", "http://localhost:3000/#b");
        assert_eq!(check_doc(&moved, false, Some(&local)), Ok(()));

        assert!(doc_is_local("null", "about:blank"));
        assert!(doc_is_local("null", "data:text/html,hi"));
        assert!(doc_is_local("file://", "file:///home/me/index.html"));
        // A blank page a site opened has the site's origin.
        assert!(!doc_is_local("https://github.com", "about:blank"));
        assert!(!doc_is_local("http://localhost", "https://github.com/"));
    }

    #[test]
    fn page_commands_parse() {
        let cwd = Path::new("/p");
        let parse =
            |a: &[&str]| parse_action(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>(), cwd);
        assert_eq!(parse(&[]), Ok(None));
        assert_eq!(
            parse(&["shot", "a.png"]),
            Ok(Some(Action::Screenshot(Some("/p/a.png".into()))))
        );
        assert_eq!(
            parse(&["eval", "1 +", "1"]),
            Ok(Some(Action::Eval("1 + 1".into())))
        );
        assert_eq!(
            parse(&["type", "#q", "hello", "world"]),
            Ok(Some(Action::Type("#q".into(), "hello world".into())))
        );
        assert_eq!(
            parse(&["type", "#q"]),
            Ok(Some(Action::Type("#q".into(), String::new())))
        );
        // A handle is not typed into a page, nor run as script there.
        assert!(parse(&["type", "#pw", "secret-ref:ab12"]).is_err());
        assert!(parse(&["eval", "x.value='secret-ref:ab12'"]).is_err());
        assert_eq!(
            parse(&["open", "localhost:3000"]),
            Ok(Some(Action::Open("localhost:3000".into())))
        );
        assert!(parse(&["click"]).is_err());
        assert!(parse(&["console", "x"]).is_err());
        assert!(parse(&["fly"]).is_err());
        assert!(Action::Click("a".into()).drives() && !Action::Text.drives());
    }

    #[test]
    fn results_and_logs_read_well() {
        assert_eq!(
            value_text(&json!({ "type": "string", "value": "hi" })),
            "hi"
        );
        assert_eq!(value_text(&json!({ "type": "number", "value": 2 })), "2");
        assert_eq!(value_text(&json!({ "type": "undefined" })), "undefined");
        assert_eq!(
            value_text(&json!({ "type": "number", "unserializableValue": "NaN" })),
            "NaN"
        );
        assert_eq!(
            value_text(&json!({ "type": "object", "value": { "a": 1 } })),
            "{\n  \"a\": 1\n}"
        );
        let line = |seq, level, text: &str| ConsoleLine {
            seq,
            level,
            text: text.into(),
            source: String::new(),
        };
        let console = [
            line(1, "error", "old"),
            line(2, "log", "hi"),
            line(3, "error", "boom"),
            line(4, "warn", "hmm"),
        ];
        assert_eq!(
            problems(&console, 1),
            "\n1 error, 1 warning in the console:\n  error boom\n  warn  hmm"
        );
        assert_eq!(problems(&console[..2], 1), "");
        assert_eq!(clip_to("añb".into(), 2), "a… (3 more bytes)");
    }

    #[test]
    fn png_size_reads_the_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(1280u32.to_be_bytes());
        png.extend(800u32.to_be_bytes());
        assert_eq!(png_size(&png), Some((1280, 800)));
        assert_eq!(png_size(b"GIF89a"), None);
        assert_eq!(key("ENTER").map(|k| k.text), Some(Some("\r")));
        assert_eq!(key("f13"), None);
    }
}
