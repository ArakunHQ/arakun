//! Clipboard bridge for the off-screen browser on Linux. CEF never holds Wayland keyboard
//! focus there (it has no window of its own), so the compositor ignores its clipboard reads and
//! writes; on X11 it can take the clipboard but doesn't answer requests for it. So the page
//! never touches the native clipboard: arakun pastes into it with script, and everything the page
//! copies comes back as a tagged console message (see `client::on_console_message`).

/// Starts a console message that carries copied text rather than a log line.
pub const COPY_TAG: &str = "\u{2063}arakun-clipboard\u{2063}";

/// Replaces the page's ways to copy (Ctrl+C/Ctrl+X, `execCommand("copy")`,
/// `navigator.clipboard`) with ones that report the text to arakun. Page `copy`/`cut` handlers
/// still run and can change what is copied. Runs on each document; the guard makes repeated
/// runs harmless.
pub fn watch_script() -> String {
    let tag = serde_json::to_string(COPY_TAG).unwrap_or_default();
    format!(
        r#"(() => {{
  if (window.__arakunClipboard) return;
  Object.defineProperty(window, "__arakunClipboard", {{ value: true }});
  const say = console.debug.bind(console), tag = {tag};
  const send = (t) => {{ if (typeof t === "string" && t) say(tag + t); }};
  const focused = () => {{
    let el = document.activeElement;
    while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
    return el;
  }};
  const field = (el) => el && (el.tagName === "TEXTAREA" || el.tagName === "INPUT") && typeof el.selectionStart === "number";
  const selected = (el) => {{
    if (field(el)) return el.type === "password" ? "" : el.value.slice(el.selectionStart, el.selectionEnd);
    if (el && el.tagName === "INPUT") return "";
    return String(getSelection() || "");
  }};
  const exec = document.execCommand.bind(document);
  const copy = (kind) => {{
    const el = focused(), text = selected(el), data = new DataTransfer();
    const e = new ClipboardEvent(kind, {{ clipboardData: data, bubbles: true, cancelable: true, composed: true }});
    (el || document.body || document.documentElement).dispatchEvent(e);
    if (e.defaultPrevented) return send(data.getData("text/plain"));
    send(text);
    const editable = field(el) ? !el.readOnly && !el.disabled : el && el.isContentEditable;
    if (kind === "cut" && text && editable) exec("delete");
  }};
  document.execCommand = function (command, ...rest) {{
    const c = String(command).toLowerCase();
    if (c === "copy" || c === "cut") {{ copy(c); return true; }}
    return exec(command, ...rest);
  }};
  addEventListener("keydown", (e) => {{
    if (e.defaultPrevented || !(e.ctrlKey || e.metaKey) || e.altKey) return;
    const k = e.key.toLowerCase();
    const kind = k === "c" || k === "insert" ? "copy" : k === "x" ? "cut" : null;
    if (!kind) return;
    e.preventDefault();
    copy(kind);
  }});
  const clip = navigator.clipboard;
  if (!clip) return;
  if (clip.writeText) clip.writeText = (t) => {{ send(String(t)); return Promise.resolve(); }};
  if (clip.write) {{
    clip.write = async (items) => {{
      for (const item of items)
        if (item.types.includes("text/plain")) send(await (await item.getType("text/plain")).text());
    }};
  }}
}})();"#
    )
}

/// Pastes `text` where the page's focus is, the way a real paste would: a `paste` event first
/// (rich editors handle it themselves), then plain insertion if nobody took it.
pub fn paste_script(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
    format!(
        r#"((t) => {{
  let el = document.activeElement;
  while (el && el.shadowRoot && el.shadowRoot.activeElement) el = el.shadowRoot.activeElement;
  const data = new DataTransfer();
  data.setData("text/plain", t);
  const e = new ClipboardEvent("paste", {{ clipboardData: data, bubbles: true, cancelable: true, composed: true }});
  if ((el || document.body || document.documentElement).dispatchEvent(e)) document.execCommand("insertText", false, t);
}})({text});"#
    )
}
