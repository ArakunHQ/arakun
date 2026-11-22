//! Tool calls in the transcript: their labels, rows and the output they show when opened.

use super::*;
use std::borrow::Cow;

pub(super) struct ToolLabel {
    pub(super) action: &'static str,
    pub(super) target: String,
    pub(super) file: bool,
}

/// A tool call read as action and target: `Read src/main.rs`, `Ran cargo test`.
pub(super) fn tool_label(t: &ToolCall, cwd: &Path, running: bool, asking: bool) -> ToolLabel {
    let path = t.locations.first().cloned().or_else(|| {
        t.content.iter().find_map(|c| match c {
            ToolContent::Diff { path, .. } => Some(path.clone()),
            _ => None,
        })
    });
    let mut title = t.title.trim().replace('`', "");
    // Droid appends the risk level it assessed: "`rm x` (medium)".
    for risk in [" (low)", " (medium)", " (high)"] {
        if let Some(stripped) = title.strip_suffix(risk) {
            title = stripped.to_string();
        }
    }
    let strip = |title: &str, words: &[&str]| -> String {
        for w in words {
            if let Some(rest) = title.strip_prefix(w)
                && rest.starts_with(' ')
            {
                return rest.trim().to_string();
            }
        }
        title.to_string()
    };
    let file_target = |fallback: String| match &path {
        Some(p) => (rel_path(p, cwd), true),
        None => (fallback, false),
    };
    let input = |key: &str| {
        t.raw_input
            .as_ref()
            .and_then(|i| i.get(key))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    if t.is_subagent() && t.tool_name.is_some() {
        return ToolLabel {
            action: if running { "Running agent" } else { "Agent" },
            target: title,
            file: false,
        };
    }
    match t.kind.as_str() {
        "read" => {
            let (target, file) = file_target(strip(&title, &["Read", "read"]));
            ToolLabel {
                action: "Read",
                target,
                file,
            }
        }
        "edit" => {
            let creating = t.content.iter().any(|c| {
                matches!(c, ToolContent::Diff { old_text, .. } if old_text.as_deref().is_none_or(str::is_empty))
            }) || title.to_lowercase().starts_with("write");
            let (target, file) = file_target(strip(&title, &["Edit", "Write", "edit", "write"]));
            ToolLabel {
                action: if creating { "Write" } else { "Edit" },
                target,
                file,
            }
        }
        "delete" => {
            let (target, file) = file_target(strip(&title, &["Delete", "delete"]));
            ToolLabel {
                action: "Delete",
                target,
                file,
            }
        }
        "move" => ToolLabel {
            action: "Move",
            target: strip(&title, &["Move", "move"]),
            file: false,
        },
        "search" => ToolLabel {
            action: "Search",
            target: strip(
                &title,
                &["Search", "search", "Grep", "grep", "Find", "find"],
            ),
            file: false,
        },
        "execute" => ToolLabel {
            action: if asking {
                "Run"
            } else if running {
                "Running"
            } else {
                "Ran"
            },
            target: input("command").unwrap_or_else(|| strip(&title, &["Run", "Bash"])),
            file: false,
        },
        "fetch" => ToolLabel {
            action: "Fetch",
            target: input("url").unwrap_or(title),
            file: false,
        },
        "think" => ToolLabel {
            action: "Think",
            target: title,
            file: false,
        },
        "switch_mode" => ToolLabel {
            action: "Mode",
            target: title,
            file: false,
        },
        _ => ToolLabel {
            action: "",
            target: if title.is_empty() {
                "Tool".into()
            } else {
                title
            },
            file: false,
        },
    }
}

/// How long a command may run: a plain `sleep` waits exactly that long,
/// and Claude's Bash takes a `timeout` in milliseconds.
pub(super) fn wait_limit(t: &ToolCall) -> Option<Wait> {
    if t.kind != "execute" {
        return None;
    }
    let input = t.raw_input.as_ref()?;
    if let Some(secs) = input
        .get("command")
        .and_then(|c| c.as_str())
        .and_then(sleep_secs)
        .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
    {
        return Some(Wait::Sleep(secs));
    }
    let ms = input.get("timeout").and_then(|v| v.as_u64())?;
    (ms > 0).then(|| Wait::Timeout(Duration::from_millis(ms)))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Wait {
    /// The command only waits: it ends when the time is up.
    Sleep(Duration),
    /// The command is stopped if it runs this long.
    Timeout(Duration),
}

/// The seconds `sleep 30`, `sleep 1.5` or `sleep 2m` waits; None for anything
/// that does more than sleep.
fn sleep_secs(cmd: &str) -> Option<f64> {
    let mut words = cmd.split_whitespace();
    if words.next()? != "sleep" {
        return None;
    }
    let mut total = 0.;
    let mut any = false;
    for w in words {
        let (num, unit) = match w.char_indices().last()? {
            (i, c @ ('s' | 'm' | 'h' | 'd')) => (&w[..i], c),
            _ => (w, 's'),
        };
        let n: f64 = num
            .parse()
            .ok()
            .filter(|n: &f64| n.is_finite() && *n >= 0.)?;
        total += n * match unit {
            'm' => 60.,
            'h' => 3600.,
            'd' => 86400.,
            _ => 1.,
        };
        any = true;
    }
    (any && total > 0.).then_some(total)
}

/// The timer at the end of a running call: the time so far, the time left of
/// a sleep, or the time against the command's timeout with a bar that fills.
fn run_timer(ix: usize, elapsed: Duration, wait: Option<Wait>) -> AnyElement {
    let ms = elapsed.as_millis() as u64;
    let (text, fraction) = match wait {
        Some(Wait::Sleep(total)) => {
            let left = total.saturating_sub(elapsed);
            let text = if left.is_zero() {
                "Done waiting".to_string()
            } else {
                format!("{} left", fmt_elapsed(left.as_millis() as u64))
            };
            (text, Some(elapsed.as_secs_f32() / total.as_secs_f32()))
        }
        Some(Wait::Timeout(limit)) => (
            format!(
                "{} of {}",
                fmt_elapsed(ms),
                fmt_elapsed(limit.as_millis() as u64)
            ),
            Some(elapsed.as_secs_f32() / limit.as_secs_f32()),
        ),
        None => (fmt_elapsed(ms), None),
    };
    // Close to the timeout reads as a warning: the command may be cut off.
    let late = matches!(wait, Some(Wait::Timeout(_))) && fraction.is_some_and(|f| f >= 0.8);
    let fill = if late { amber() } else { theme::working() };
    div()
        .id(("tool-timer", ix))
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .text_size(px(crate::ui::TEXT_SM))
        .text_color(if late { amber() } else { theme::text_faint() })
        .when_some(fraction, |d, f| {
            d.child(
                div()
                    .w(px(36.))
                    .h(px(3.))
                    .rounded(crate::ui::rad(2.))
                    .bg(theme::fill_hover())
                    .child(
                        div()
                            .h_full()
                            .rounded(crate::ui::rad(2.))
                            .bg(fill)
                            .w(px(36. * f.clamp(0., 1.))),
                    ),
            )
        })
        .child(text)
        .into_any_element()
}

impl ThreadView {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn activity_row(
        &mut self,
        ix: usize,
        entry: &Entry,
        cwd: &Path,
        live: bool,
        permission: Option<&PermissionRequest>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.open.contains(&ix);
        let row = match entry {
            Entry::Thought { text } | Entry::Agent { text } => {
                let thought = matches!(entry, Entry::Thought { .. });
                let summary = markdown::summary(text);
                let summary = if summary.is_empty() {
                    "Thinking".to_string()
                } else {
                    summary
                };
                let color = if thought {
                    theme::mix(0.5)
                } else {
                    theme::mix(0.7)
                };
                let streaming = thought && live && ix + 1 == self.thread.read(cx).entries().len();
                let group: SharedString = format!("row-{ix}").into();
                let label = div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(crate::ui::TEXT_LG))
                    .text_color(color)
                    .group_hover(group.clone(), |s| s.text_color(theme::text_muted()))
                    .child(if open && thought || streaming && summary == "Thinking" {
                        "Thinking…".to_string()
                    } else {
                        summary
                    });
                let mut row = div().flex().flex_col().min_w_0().child(
                    div()
                        .id(("row", ix))
                        .group(group)
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .py(crate::ui::sp(4.))
                        .cursor_pointer()
                        .child(if streaming {
                            pulse(self.active, label)
                        } else {
                            label.into_any_element()
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_open(ix, cx))),
                );
                if open {
                    let st = if thought {
                        MdStyle::muted()
                    } else {
                        MdStyle::reply()
                    };
                    let body = div()
                        .min_w_0()
                        .pb(crate::ui::sp(8.))
                        .child(self.surface(ix, text, st, cx))
                        .into_any_element();
                    row = row.child(self.unfold(Fold::Row, ix, body, cx));
                }
                row.into_any_element()
            }
            Entry::Tool(tool) => {
                let asking = permission.filter(|p| p.tool_call_id == tool.id);
                let mut col = div().flex().flex_col().min_w_0().child(self.tool_row(
                    ix,
                    tool,
                    cwd,
                    live,
                    asking.is_some(),
                    cx,
                ));
                if let Some(p) = asking {
                    col = col.child(self.permission_card(p, Some(tool), cwd, window, cx));
                } else if open {
                    let details = tool_details(tool, cwd);
                    col = col.child(self.unfold(Fold::Row, ix, details, cx));
                }
                col.into_any_element()
            }
            Entry::Plan { entries } => div()
                .py(crate::ui::sp(4.))
                .child(plan_card(entries))
                .into_any_element(),
            Entry::Notice {
                text,
                error,
                places,
                ..
            } => div()
                .py(crate::ui::sp(4.))
                .text_size(px(crate::ui::TEXT_LG))
                .text_color(if *error {
                    red().opacity(0.85)
                } else {
                    theme::mix(0.5)
                })
                .child(text.clone())
                .children(self.notice_places(ix, places, cx))
                .into_any_element(),
            Entry::Compacted {
                removed,
                before,
                after,
            } => compaction_divider(*removed, *before, *after),
            Entry::User { .. } => div().into_any_element(),
        };
        self.arrive(ix, row, cx)
    }

    pub(super) fn tool_row(
        &self,
        ix: usize,
        tool: &ToolCall,
        cwd: &Path,
        live: bool,
        asking: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        // A subagent runs on in the background after the turn that started it.
        let subagent = tool
            .subagent_id()
            .and_then(|id| self.thread.read(cx).subagent(id))
            .map(|s| (s.id.clone(), s.state));
        let background = subagent.as_ref().is_some_and(|(_, s)| s.is_running());
        let live = live || background;
        let running = live && matches!(tool.status, ToolStatus::Pending | ToolStatus::InProgress);
        // A subagent keeps its own clock; it outlives the turn's.
        let timer = if !running || asking {
            None
        } else if background {
            tool.subagent_id()
                .and_then(|id| self.thread.read(cx).subagent(id))
                .map(|s| Duration::from_secs(s.elapsed_secs()))
        } else {
            self.thread.read(cx).tool_elapsed(&tool.id)
        }
        .filter(|d| d.as_secs() >= 1)
        .map(|d| run_timer(ix, d, wait_limit(tool)));
        let failed = tool.status == ToolStatus::Failed;
        let label = tool_label(tool, cwd, running, asking);
        let has_output = tool.content.iter().any(|c| match c {
            ToolContent::Text { text } => !text.trim().is_empty(),
            ToolContent::Diff { .. } => true,
            ToolContent::Terminal { .. } => false,
        }) || tool
            .raw_output
            .as_ref()
            .is_some_and(|o| !o.trim().is_empty())
            || !tool.terminal_output.trim().is_empty();
        let compact = matches!(tool.kind.as_str(), "read" | "search") && !failed;
        let expandable = !asking && !compact && (has_output || tool.kind == "execute");
        let open = self.open.contains(&ix);
        let target_color = if failed { red() } else { theme::mix(0.85) };
        let group: SharedString = format!("tool-{ix}").into();
        // The file a read or edit touched, to open from its name.
        let file = label
            .file
            .then(|| {
                tool.locations.first().or_else(|| {
                    tool.content.iter().find_map(|c| match c {
                        ToolContent::Diff { path, .. } => Some(path),
                        _ => None,
                    })
                })
            })
            .flatten()
            .map(|p| {
                if p.is_relative() {
                    cwd.join(p)
                } else {
                    p.clone()
                }
            })
            .filter(|p| p.is_file());

        let status_icon = if failed {
            Some(icon("cancel01", 14.).text_color(red()).into_any_element())
        } else if matches!(tool.status, ToolStatus::Pending | ToolStatus::InProgress)
            && (live || asking)
        {
            Some(if asking {
                icon("circle-dashed", 14.)
                    .text_color(theme::text_faint())
                    .into_any_element()
            } else {
                spinner(self.active, 14., theme::mix(0.4))
            })
        } else {
            None
        };

        div()
            .id(("tool", ix))
            .group(group.clone())
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .py(crate::ui::sp(4.))
            .min_w_0()
            .when(expandable, |d| d.cursor_pointer())
            .when_some(status_icon, |d, i| d.child(i))
            .when(!label.action.is_empty(), |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(crate::ui::TEXT_LG))
                        .text_color(if failed { red() } else { theme::mix(0.5) })
                        .child(label.action),
                )
            })
            .child(
                div()
                    .id(("tool-file", ix))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(4.))
                    .min_w_0()
                    .px(crate::ui::sp(4.))
                    .rounded(crate::ui::rad(4.))
                    .when(label.file, |d| d.bg(theme::fill_hover()))
                    .when_some(file, |d, file| {
                        d.cursor_pointer()
                            .hover(|s| {
                                s.bg(theme::fill_selected())
                                    .text_color(markdown::link_color())
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(ThreadViewEvent::OpenFile(file.clone(), None));
                            }))
                    })
                    .font_family(theme::FONT_MONO)
                    .text_size(px(crate::ui::TEXT_MD))
                    .text_color(target_color)
                    .when(label.file, |d| {
                        d.child(icon("file01", 12.).text_color(theme::text_subtle()))
                    })
                    .child(
                        div().min_w_0().truncate().child(
                            crate::redact::text(&label.target.replace('\n', " ")).into_owned(),
                        ),
                    ),
            )
            .when(tool.added > 0 || tool.removed > 0, |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .gap(crate::ui::sp(4.))
                        .text_size(px(crate::ui::TEXT_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .when(tool.added > 0, |d| {
                            d.child(
                                div()
                                    .text_color(theme::diff_add())
                                    .child(format!("+{}", tool.added)),
                            )
                        })
                        .when(tool.removed > 0, |d| {
                            d.child(
                                div()
                                    .text_color(theme::diff_del())
                                    .child(format!("-{}", tool.removed)),
                            )
                        }),
                )
            })
            .child(div().flex_1())
            .when_some(timer, |d, timer| d.child(timer))
            .when_some(subagent, |d, (id, _)| {
                d.child(
                    div()
                        .id(("tool-subagent", ix))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .h(px(20.))
                        .px(crate::ui::sp(6.))
                        .rounded(crate::ui::rad(4.))
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                        .child(icon("sidebar-right01", 12.).text_color(theme::text_subtle()))
                        .child("Open")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(ThreadViewEvent::OpenSubagent(Some(id.clone())));
                        })),
                )
            })
            .when(expandable, |d| {
                d.child(chevron(
                    ("tool-chevron", ix),
                    open,
                    icon("arrow-right01", 14.)
                        .text_color(theme::text_faint())
                        .group_hover(group, |s| s.text_color(theme::text_muted())),
                ))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_open(ix, cx)))
            })
            .into_any_element()
    }
}

pub(super) fn tool_details(tool: &ToolCall, cwd: &Path) -> AnyElement {
    let mut col = div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(6.))
        .pt(crate::ui::sp(2.))
        .pb(crate::ui::sp(6.))
        .min_w_0();
    if tool.kind == "execute" {
        return col.child(terminal_card(tool)).into_any_element();
    }
    let mut any_text = false;
    for c in &tool.content {
        match c {
            ToolContent::Diff {
                path,
                old_text,
                new_text,
            } => col = col.child(diff_card(path, old_text.as_deref(), new_text, cwd)),
            ToolContent::Text { text } if !text.trim().is_empty() => {
                any_text = true;
                col = col.child(mono_block(text, theme::mix(0.55)));
            }
            _ => {}
        }
    }
    if !any_text
        && let Some(out) = tool.raw_output.as_ref().filter(|o| !o.trim().is_empty())
        && !tool
            .content
            .iter()
            .any(|c| matches!(c, ToolContent::Diff { .. }))
    {
        col = col.child(mono_block(out, theme::mix(0.55)));
    }
    col.into_any_element()
}

/// A command and what it printed, like a terminal: from the `_meta.terminal_output`
/// extension when the agent sends it, else the text it put in the tool's content.
pub(super) fn terminal_card(tool: &ToolCall) -> AnyElement {
    let cmd = tool
        .raw_input
        .as_ref()
        .and_then(|i| i.get("command"))
        .and_then(|c| c.as_str())
        .map(str::to_string);
    let (output, dropped) = if !tool.terminal_output.trim().is_empty() {
        (
            Cow::Borrowed(tool.terminal_output.as_str()),
            tool.terminal_dropped_lines,
        )
    } else {
        let texts: Vec<&str> = tool
            .content
            .iter()
            .filter_map(|c| match c {
                ToolContent::Text { text } if !text.trim().is_empty() => Some(unfence(text)),
                _ => None,
            })
            .collect();
        let text = if texts.is_empty() {
            Cow::Borrowed(tool.raw_output.as_deref().unwrap_or_default())
        } else {
            Cow::Owned(texts.join("\n"))
        };
        (text, 0)
    };
    // The end of a command's output says the most: how it finished. Only that
    // end is cleaned up, as this runs on every frame while the command prints.
    let (hidden, shown) = output_tail(&output, OUTPUT_LINES);
    let hidden = dropped + hidden;
    let failed = tool.exit_code.is_some_and(|c| c != 0) || tool.status == ToolStatus::Failed;
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
        .bg(theme::sunk())
        .overflow_hidden()
        .font_family(theme::FONT_MONO)
        .text_size(px(crate::ui::TEXT_SM))
        .line_height(px(20.))
        .when_some(cmd, |d, cmd| {
            d.child(
                div()
                    .flex()
                    .items_start()
                    .gap(crate::ui::sp(8.))
                    .px(crate::ui::sp(10.))
                    .py(crate::ui::sp(6.))
                    .child(div().flex_none().text_color(theme::text_faint()).child("$"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme::text_muted())
                            .child(crate::redact::text(&cmd).into_owned()),
                    ),
            )
        })
        .when(hidden > 0, |d| {
            d.child(
                div()
                    .px(crate::ui::sp(10.))
                    .pt(crate::ui::sp(6.))
                    .text_color(theme::text_faint())
                    .child(format!("… {hidden} earlier lines")),
            )
        })
        .when(!shown.is_empty(), |d| {
            d.child(
                div()
                    .px(crate::ui::sp(10.))
                    .py(crate::ui::sp(6.))
                    .text_color(theme::text_subtle())
                    .child(crate::redact::text(&shown).into_owned()),
            )
        })
        .when(failed, |d| {
            d.child(
                div()
                    .px(crate::ui::sp(10.))
                    .py(crate::ui::sp(4.))
                    .text_color(red().opacity(0.85))
                    .child(match tool.exit_code {
                        Some(code) => format!("Exited with code {code}"),
                        None => "Failed".to_string(),
                    }),
            )
        })
        .into_any_element()
}

/// The last `n` lines of terminal output without escape sequences, and how
/// many lines come before them. Long lines are cut to their end.
fn output_tail(output: &str, n: usize) -> (u64, String) {
    const SHOWN_MAX: usize = 16 * 1024;
    let output = output.trim_end();
    let mut start = 0;
    let mut seen = 0;
    for (i, _) in output.rmatch_indices('\n') {
        seen += 1;
        if seen == n {
            start = i + 1;
            break;
        }
    }
    let mut from = start.max(output.len().saturating_sub(SHOWN_MAX));
    while !output.is_char_boundary(from) {
        from += 1;
    }
    // Lines the size cut removes whole are hidden too; a line cut through
    // still shows its end.
    let hidden = output[..from].bytes().filter(|&b| b == b'\n').count() as u64;
    (hidden, strip_ansi(&output[from..]).trim_end().to_string())
}

/// The inside of a fenced block, as Claude sends command output without the
/// terminal extension.
pub(super) fn unfence(text: &str) -> &str {
    let t = text.trim();
    match t.strip_prefix("```") {
        Some(rest) => {
            let body = rest.split_once('\n').map_or("", |(_, b)| b);
            body.strip_suffix("```").unwrap_or(body).trim_end()
        }
        None => text,
    }
}

/// Terminal output without its escape sequences (colors, cursor moves) and
/// with carriage-return redraws collapsed to what the line ended as.
pub(super) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\x07' || (n == '\x1b' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\r' if chars.peek() == Some(&'\n') => {}
            '\r' => {
                let line_start = out.rfind('\n').map_or(0, |i| i + 1);
                out.truncate(line_start);
            }
            c => out.push(c),
        }
    }
    out
}

pub(super) fn mono_block(text: &str, color: Hsla) -> AnyElement {
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let more = lines.len().saturating_sub(OUTPUT_LINES);
    let mut shown = lines[..lines.len().min(OUTPUT_LINES)].join("\n");
    if more > 0 {
        shown.push_str(&format!("\n… {more} more lines"));
    }
    div()
        .min_w_0()
        .px(crate::ui::sp(10.))
        .font_family(theme::FONT_MONO)
        .text_size(px(crate::ui::TEXT_SM))
        .line_height(px(20.))
        .text_color(color)
        .child(crate::redact::text(&shown).into_owned())
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_tail_counts_the_lines_above_it() {
        let output: String = (1..=50)
            .map(|i| format!("\x1b[1mline {i}\x1b[0m\n"))
            .collect();
        let (hidden, shown) = output_tail(&output, 40);
        assert_eq!(hidden, 10);
        assert!(shown.starts_with("line 11\n"));
        assert!(shown.ends_with("line 50"));
        assert_eq!(output_tail("a\nb\n\n", 40), (0, "a\nb".to_string()));
        assert_eq!(output_tail("", 40), (0, String::new()));
    }

    #[test]
    fn a_long_line_is_cut_to_its_end() {
        let line = "é".repeat(20_000);
        let (hidden, shown) = output_tail(&line, 40);
        assert_eq!(hidden, 0);
        assert!(shown.len() <= 16 * 1024 && shown.chars().all(|c| c == 'é'));
        // Forty long lines: the ones cut away by size count as earlier lines.
        let output: String = (0..40).map(|_| format!("{}\n", "y".repeat(1024))).collect();
        let (hidden, shown) = output_tail(&output, 40);
        assert_eq!(hidden as usize + shown.lines().count(), 40);
        assert!(hidden >= 24);
    }

    fn command(cmd: &str, timeout: Option<u64>) -> ToolCall {
        let mut input = serde_json::json!({ "command": cmd });
        if let Some(ms) = timeout {
            input["timeout"] = ms.into();
        }
        serde_json::from_value(serde_json::json!({
            "id": "t", "title": cmd, "kind": "execute", "status": "in_progress",
            "raw_input": input,
        }))
        .unwrap()
    }

    #[test]
    fn sleep_reads_its_seconds() {
        assert_eq!(sleep_secs("sleep 30"), Some(30.));
        assert_eq!(sleep_secs("sleep 1.5"), Some(1.5));
        assert_eq!(sleep_secs("sleep 2m 10"), Some(130.));
        assert_eq!(sleep_secs("sleep 1h"), Some(3600.));
        assert_eq!(sleep_secs("sleep"), None);
        assert_eq!(sleep_secs("sleep 0"), None);
        assert_eq!(sleep_secs("sleep 5 && cargo test"), None);
        assert_eq!(sleep_secs("sleepy 5"), None);
    }

    #[test]
    fn a_command_waits_for_its_sleep_or_its_timeout() {
        assert_eq!(
            wait_limit(&command("sleep 30", Some(60_000))),
            Some(Wait::Sleep(Duration::from_secs(30)))
        );
        assert_eq!(
            wait_limit(&command("cargo test", Some(600_000))),
            Some(Wait::Timeout(Duration::from_secs(600)))
        );
        assert_eq!(wait_limit(&command("cargo test", None)), None);
        // Too long for a Duration: falls back to the timeout instead of panicking.
        assert_eq!(
            wait_limit(&command("sleep 1e308d", Some(60_000))),
            Some(Wait::Timeout(Duration::from_secs(60)))
        );
        assert_eq!(wait_limit(&command("sleep 1e308d", None)), None);
        let mut read = command("sleep 30", None);
        read.kind = "read".into();
        assert_eq!(wait_limit(&read), None);
    }
}
