//! The pane at the right: a hero for each kind of row (a pull request's
//! checks, a session's state, a file's code, a person's numbers).

use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt as _, AnyElement, Context, ElementId, IntoElement, ParentElement as _,
    Styled as _, div,
};

use super::*;
use crate::model::Status;
use crate::theme;
use crate::ui::icon;
use crate::ui::{self as kit};

fn hint(keys: &[&str], label: &str) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .text_color(theme::text_faint())
        .child(kit::keycaps(keys))
        .child(label.to_string())
        .into_any_element()
}

fn hints(parts: Vec<AnyElement>) -> AnyElement {
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap(crate::ui::sp(10.))
        .child(
            kit::props()
                .flex_row()
                .flex_wrap()
                .gap(crate::ui::sp(10.))
                .children(parts),
        )
        .into_any_element()
}

fn status_line(lead: AnyElement, word: &str, color: gpui::Hsla, rest: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(10.))
        .child(lead)
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(color)
                        .child(word.to_string()),
                )
                .when(!rest.is_empty(), |d| {
                    d.child(div().text_color(theme::text_faint()).child(rest))
                }),
        )
        .into_any_element()
}

impl Palette {
    pub(super) fn render_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let entry = self.entries.get(self.selected).cloned();
        let key = entry
            .as_ref()
            .map(|e| e.item.key.clone())
            .unwrap_or_default();
        let body: Vec<AnyElement> = match &entry {
            None => vec![kit::palette_note("Nothing selected").into_any_element()],
            Some(e) => self.preview_body(e, cx),
        };
        let id = key
            .bytes()
            .fold(7u64, |h, b| h.wrapping_mul(31).wrapping_add(b as u64));
        kit::preview_pane()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(16.))
                    .min_h_0()
                    .children(body)
                    .with_animation(
                        ElementId::NamedInteger("palette-preview".into(), id),
                        Animation::new(Duration::from_millis(120)),
                        |d, t| d.opacity(0.35 + 0.65 * t),
                    ),
            )
            .into_any_element()
    }

    fn preview_body(&mut self, e: &Entry, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if let Some(card) = &e.card {
            return self.card_preview(card, cx);
        }
        let item = &e.item;
        match &item.preview {
            Preview::Session {
                place,
                agent,
                status,
                age,
                doing,
                added,
                removed,
                files,
                cost,
                quote,
            } => {
                let (lead, word, color) = match status {
                    Status::Working => (
                        kit::lead_spinner("pv-spin"),
                        "Working",
                        theme::success().into(),
                    ),
                    Status::NeedsInput => (
                        kit::lead_dot(theme::accent(), true).into_any_element(),
                        "Needs you",
                        theme::accent(),
                    ),
                    Status::Error => (
                        kit::lead_dot(theme::error().into(), false).into_any_element(),
                        "Failed",
                        theme::error().into(),
                    ),
                    Status::Idle => (
                        kit::lead_ring().into_any_element(),
                        "Idle",
                        theme::text_muted(),
                    ),
                };
                let mut out = vec![
                    kit::preview_eyebrow("message01", place.clone()).into_any_element(),
                    status_line(lead, word, color, format!("· {age} · {agent}")),
                    kit::preview_title(item.title.clone()).into_any_element(),
                ];
                if let Some(q) = quote.clone().or_else(|| doing.clone()) {
                    out.push(kit::quote(format!("“{q}”")).into_any_element());
                }
                let mut stats = vec![(
                    format!("+{added}"),
                    "lines".to_string(),
                    Some(gpui::Hsla::from(theme::success())),
                )];
                if *removed > 0 {
                    stats.push((
                        format!("−{removed}"),
                        "removed".into(),
                        Some(theme::error().into()),
                    ));
                }
                if let Some(f) = files {
                    stats.push((f.to_string(), "files".into(), None));
                }
                if let Some(c) = cost {
                    stats.push((c.clone(), "cost".into(), None));
                }
                stats.truncate(3);
                if *added > 0 || cost.is_some() {
                    out.push(kit::stat_grid(&stats).into_any_element());
                }
                out
            }
            Preview::Pull {
                repo,
                number,
                author,
                avatar,
                waiting,
                checks,
                merge,
                changes,
                opened,
                diff,
            } => {
                let mut out = vec![
                    kit::preview_eyebrow("git-pull-request", format!("{repo} · #{number}"))
                        .into_any_element(),
                ];
                if !author.is_empty() {
                    out.push(kit::ringed_avatar(author, avatar.as_deref(), 38.).into_any_element());
                }
                out.push(kit::preview_title(item.title.clone()).into_any_element());
                out.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(10.))
                        .when_some(waiting.clone(), |d, (w, on_you)| {
                            d.child(kit::palette_pill(
                                w,
                                if on_you {
                                    theme::accent()
                                } else {
                                    theme::text_subtle()
                                },
                            ))
                        })
                        .when(!opened.is_empty(), |d| {
                            d.child(kit::row_sub(format!("opened {opened}"), true))
                        })
                        .into_any_element(),
                );
                let mut props = kit::props();
                if !author.is_empty() {
                    props = props.child(kit::prop("Author", author.clone()));
                }
                if let Some((passed, total, colors)) = checks {
                    let colors: Vec<gpui::Hsla> = colors.iter().map(|t| t.color()).collect();
                    props = props.child(kit::prop(
                        "Checks",
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .child(kit::check_bars(&colors))
                            .child(kit::mono(format!("{passed}/{total}"), theme::text())),
                    ));
                }
                if let Some((text, tint)) = merge {
                    props = props.child(kit::prop(
                        "Merge",
                        div().text_color(tint.color()).child(text.clone()),
                    ));
                }
                if let Some((a, d, f)) = changes {
                    props = props.child(kit::prop(
                        "Changes",
                        div()
                            .flex()
                            .gap(crate::ui::sp(6.))
                            .child(kit::mono(format!("+{a}"), theme::success().into()))
                            .child(kit::mono(format!("−{d}"), theme::error().into()))
                            .child(kit::mono(format!("· {f} files"), theme::text())),
                    ));
                }
                out.push(props.into_any_element());
                if !diff.is_empty() {
                    out.push(
                        kit::code_well()
                            .children(diff.iter().take(8).map(|l| {
                                let color: gpui::Hsla = if l.starts_with('+') {
                                    theme::success().into()
                                } else if l.starts_with('-') {
                                    theme::error().into()
                                } else {
                                    theme::text_subtle()
                                };
                                kit::code_line(None, l.clone(), color)
                            }))
                            .into_any_element(),
                    );
                }
                out.push(hints(vec![hint(&["→"], "actions"), hint(&["↵"], "open")]));
                out
            }
            Preview::Person {
                login,
                avatar,
                for_you,
                you_owe,
                theirs,
            } => vec![
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(kit::ringed_avatar(login, avatar.as_deref(), 68.))
                    .child(kit::preview_title(login.clone()))
                    .child(kit::row_sub(format!("@{login}"), false))
                    .into_any_element(),
                kit::stat_grid(&[
                    (for_you.to_string(), "for you".into(), Some(theme::accent())),
                    (you_owe.to_string(), "waiting on them".into(), None),
                    (theirs.to_string(), "open".into(), None),
                ])
                .into_any_element(),
                hints(vec![hint(&["→"], "their pull requests")]),
            ],
            Preview::Run {
                title,
                place,
                state,
                tint,
                ago,
                duration,
                commit,
                log,
            } => {
                let mut out = vec![
                    kit::preview_eyebrow("activity01", place.clone()).into_any_element(),
                    status_line(
                        kit::lead_dot(tint.color(), false).into_any_element(),
                        state,
                        tint.color(),
                        format!(
                            "· {ago}{}",
                            duration
                                .as_ref()
                                .map(|d| format!(" · {d}"))
                                .unwrap_or_default()
                        ),
                    ),
                    kit::preview_title(title.clone()).into_any_element(),
                ];
                if !log.is_empty() {
                    out.push(
                        kit::code_well()
                            .children(log.iter().take(10).enumerate().map(|(n, l)| {
                                kit::code_line(
                                    None,
                                    l.clone(),
                                    if n == 0 {
                                        theme::error().into()
                                    } else {
                                        theme::text_subtle()
                                    },
                                )
                            }))
                            .into_any_element(),
                    );
                }
                let mut props = kit::props();
                if !commit.is_empty() {
                    props = props.child(kit::prop(
                        "Commit",
                        kit::mono(commit.clone(), theme::text()),
                    ));
                }
                out.push(props.into_any_element());
                out.push(hints(vec![hint(&["→"], "fix, re-run, copy log")]));
                out
            }
            Preview::File { path, rel, line } => self.file_preview(path, rel, *line, cx),
            Preview::Command { blurb, keys, cli } => {
                let mut props = kit::props();
                if !keys.is_empty() {
                    props = props.child(kit::prop("Shortcut", kit::keycaps(keys)));
                }
                if let Some(c) = cli {
                    props = props.child(kit::prop("CLI", kit::mono(c.clone(), theme::text())));
                }
                vec![
                    kit::preview_eyebrow("flash", "command").into_any_element(),
                    kit::preview_title(item.title.clone()).into_any_element(),
                    kit::preview_text(blurb.clone()).into_any_element(),
                    props.into_any_element(),
                ]
            }
            Preview::View { blurb, stats } => {
                let mut out = vec![
                    kit::preview_eyebrow(
                        match item.lead {
                            Lead::Icon(i, _) => i,
                            _ => "dashboard-square01",
                        },
                        "view",
                    )
                    .into_any_element(),
                    kit::preview_title(item.title.clone()).into_any_element(),
                ];
                if !stats.is_empty() {
                    let cells: Vec<(String, String, Option<gpui::Hsla>)> = stats
                        .iter()
                        .take(3)
                        .map(|(v, l)| (v.clone(), l.clone(), None))
                        .collect();
                    out.push(kit::stat_grid(&cells).into_any_element());
                }
                out.push(kit::preview_text(blurb.clone()).into_any_element());
                out.push(hints(vec![
                    hint(&["↵"], "open"),
                    hint(&["Ctrl", "P"], "pin"),
                ]));
                out
            }
            Preview::Setting { path, blurb, on } => vec![
                kit::preview_eyebrow("settings01", path.clone()).into_any_element(),
                kit::preview_title(item.title.clone()).into_any_element(),
                kit::preview_text(blurb.clone()).into_any_element(),
                kit::props()
                    .child(kit::prop("Toggle", kit::keycap("↵")))
                    .child(kit::prop(
                        "Status",
                        div()
                            .text_color(if *on {
                                theme::success().into()
                            } else {
                                theme::text_muted()
                            })
                            .child(if *on { "On" } else { "Off" }),
                    ))
                    .into_any_element(),
            ],
            Preview::Project {
                path,
                branch,
                envs,
                sessions,
            } => vec![
                kit::preview_eyebrow("folder01", path.clone()).into_any_element(),
                kit::preview_title(item.title.clone()).into_any_element(),
                kit::props()
                    .child(kit::prop(
                        "Branch",
                        kit::mono(branch.clone(), theme::text()),
                    ))
                    .child(kit::prop("Environments", envs.to_string()))
                    .child(kit::prop("Sessions", sessions.to_string()))
                    .into_any_element(),
                hints(vec![hint(&["→"], "go in"), hint(&["↵"], "open")]),
            ],
            Preview::Branch { name, detail } => vec![
                kit::preview_eyebrow("git-branch", format!("{} · branch", self.seed.project))
                    .into_any_element(),
                kit::preview_title(name.clone()).into_any_element(),
                kit::preview_text(detail.clone()).into_any_element(),
                hints(vec![
                    hint(&["↵"], "open in Branches"),
                    hint(&["→"], "actions"),
                ]),
            ],
            Preview::Memory {
                scope,
                id,
                kind,
                body,
                files,
                age,
                hits,
            } => {
                let mut props = kit::props().child(kit::prop("Kind", kind.clone()));
                if !files.is_empty() {
                    props = props.child(kit::prop(
                        "Files",
                        kit::mono(files.join(", "), theme::text()),
                    ));
                }
                props = props.child(kit::prop("Used", format!("{hits} times")));
                vec![
                    kit::preview_eyebrow("bot", format!("memory {id} · {scope} · {age}"))
                        .into_any_element(),
                    kit::preview_title(item.title.clone()).into_any_element(),
                    kit::preview_text(body.chars().take(420).collect::<String>())
                        .line_clamp(9)
                        .into_any_element(),
                    props.into_any_element(),
                ]
            }
            Preview::Error(e) => error_preview(e),
            Preview::None => vec![
                kit::preview_title(item.title.clone()).into_any_element(),
                kit::preview_text(item.sub.clone()).into_any_element(),
            ],
        }
    }

    fn file_preview(
        &mut self,
        path: &std::path::Path,
        rel: &str,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(snip) = self.snippet(path, line, cx) else {
            return vec![
                kit::preview_eyebrow("folder01", rel.to_string()).into_any_element(),
                kit::palette_note("Reading…").into_any_element(),
            ];
        };
        let total = snip.as_ref().map(|(_, n)| *n).unwrap_or(0);
        let mut out = vec![
            kit::preview_eyebrow("folder01", format!("{rel} · {} lines", thousands(total)))
                .into_any_element(),
        ];
        match snip {
            Some((lines, _)) if !lines.is_empty() => out.push(
                kit::code_well()
                    .children(lines.into_iter().map(|(n, l)| {
                        let at = line == Some(n);
                        kit::code_line(
                            Some(n),
                            l,
                            if at {
                                theme::text()
                            } else {
                                theme::text_subtle()
                            },
                        )
                        .when(at, |d| d.text_color(theme::accent()))
                    }))
                    .into_any_element(),
            ),
            _ => out.push(kit::palette_note("No preview for this file").into_any_element()),
        }
        out.push(hints(vec![
            hint(
                &["↵"],
                if line.is_some() {
                    "open at line"
                } else {
                    "open"
                },
            ),
            hint(&["→"], "ask about it"),
        ]));
        out
    }

    fn card_preview(&mut self, card: &Card, cx: &mut Context<Self>) -> Vec<AnyElement> {
        match card {
            Card::Clip(clip::Clip::Error(e)) => error_preview(e),
            Card::Clip(clip::Clip::GitHub { link, url }) => {
                let found = self.seed.items.iter().find(|it| match (link, &it.target) {
                    (clip::GitHubLink::Pull { number, repo } | clip::GitHubLink::Issue { number, repo }, Target::Pull(r)) => {
                        r.number == *number && r.repo.eq_ignore_ascii_case(repo)
                    }
                    _ => false,
                });
                match found.cloned() {
                    Some(it) => self.preview_body(
                        &Entry {
                            item: it,
                            ranges: Vec::new(),
                            card: None,
                        },
                        cx,
                    ),
                    None => vec![
                        kit::preview_eyebrow("github", link.repo().to_string()).into_any_element(),
                        kit::preview_title(url.clone()).into_any_element(),
                        kit::preview_text("Not in your GitHub queue yet: Enter opens it in the GitHub tab.").into_any_element(),
                    ],
                }
            }
            Card::Ask => {
                let model = self.model_chip().map(|(_, l)| l).unwrap_or_else(|| self.seed.agent.clone());
                let branch = self.branch_chip().unwrap_or_else(|| self.seed.branch.clone());
                let mut out = vec![
                    kit::preview_eyebrow("message01", format!("{} · {branch}", self.seed.project)).into_any_element(),
                    kit::preview_title(format!("New session with {model}")).into_any_element(),
                    kit::preview_text("Starts a session with what you typed as the first message. Nothing is sent until you press Enter.")
                        .into_any_element(),
                ];
                if let Some(f) = self.failure().cloned()
                    && self.branch_chip().is_some()
                {
                    out.push(kit::preview_eyebrow("activity01", f.what.clone()).into_any_element());
                    out.push(
                        kit::code_well()
                            .children(f.log.iter().take(8).enumerate().map(|(n, l)| {
                                kit::code_line(None, l.clone(), if n == 0 { theme::error().into() } else { theme::text_subtle() })
                            }))
                            .into_any_element(),
                    );
                }
                out
            }
            Card::Remember(m) => self.preview_body(
                &Entry {
                    item: self
                        .memory_items(&m.memory.title)
                        .into_iter()
                        .find(|i| i.key == format!("memory:{}", m.id()))
                        .unwrap_or_else(|| Item::new("", Kind::Memory, m.memory.title.clone(), Target::Memory)),
                    ranges: Vec::new(),
                    card: None,
                },
                cx,
            ),
            Card::Answer => {
                let f = self.answer.as_ref().and_then(|a| a.failure.clone());
                match f.and_then(|what| self.seed.failures.iter().find(|x| x.what == what).cloned()) {
                    Some(f) => vec![
                        kit::preview_eyebrow("activity01", format!("run #{} · {}", f.id, f.branch)).into_any_element(),
                        status_line(
                            kit::lead_dot(theme::error().into(), false).into_any_element(),
                            "Failed",
                            theme::error().into(),
                            String::new(),
                        ),
                        kit::preview_title(f.what.clone()).into_any_element(),
                        kit::code_well()
                            .children(f.log.iter().take(10).enumerate().map(|(n, l)| {
                                kit::code_line(None, l.clone(), if n == 0 { theme::error().into() } else { theme::text_subtle() })
                            }))
                            .into_any_element(),
                    ],
                    None => vec![
                        kit::preview_eyebrow("sparkles", format!("quick answer · {}", answer::MODEL_LABEL)).into_any_element(),
                        kit::preview_text(
                            "A short answer from a small model, with what Arakun already knows. Tab keeps chatting in a real session.",
                        )
                        .into_any_element(),
                    ],
                }
            }
            Card::OfferAnswer => vec![
                kit::preview_eyebrow("sparkles", format!("quick answer · {}", answer::MODEL_LABEL)).into_any_element(),
                kit::preview_title("Ask Claude for a short answer").into_any_element(),
                kit::preview_text(
                    "Enter sends the question, the memories that match it and the failed CI runs of this branch to Anthropic through Claude Code, on your Claude account. Nothing is sent before.",
                )
                .into_any_element(),
            ],
            Card::OfferRule => vec![
                kit::preview_eyebrow("flash", format!("automation · {}", answer::MODEL_LABEL)).into_any_element(),
                kit::preview_title("Read it as an automation").into_any_element(),
                kit::preview_text(
                    "Enter sends the sentence and your session titles to Anthropic through Claude Code, which reads it as a rule. You still confirm the rule before it waits. Nothing is sent before.",
                )
                .into_any_element(),
            ],
            Card::Rule(d) => {
                let pull = match &d.trigger {
                    rules::Trigger::PrChecksPass { number, repo } | rules::Trigger::PrMerged { number, repo } => self
                        .seed
                        .items
                        .iter()
                        .find(|it| matches!(&it.target, Target::Pull(r) if r.number == *number && r.repo.eq_ignore_ascii_case(repo)))
                        .cloned(),
                    _ => None,
                };
                let mut props = kit::props();
                if let Some(Preview::Pull { checks, waiting, .. }) = pull.as_ref().map(|p| &p.preview) {
                    if let Some((passed, total, _)) = checks {
                        props = props.child(kit::prop("Watching", format!("{} checks · {passed}/{total} passed", total)));
                    }
                    if let Some((w, _)) = waiting
                        && d.steps.iter().any(|s| matches!(s, rules::Step::Merge { .. }))
                    {
                        props = props.child(kit::prop("Needs", div().text_color(theme::accent()).child(w.clone())));
                    }
                }
                if let Some(rules::Step::Merge { method }) =
                    d.steps.iter().find(|s| matches!(s, rules::Step::Merge { .. }))
                {
                    props = props.child(kit::prop("Merge method", format!("{method} (repo default)")));
                }
                props = props.child(kit::prop("Expires", format!("in {} h", d.hours)));
                vec![
                    kit::preview_eyebrow("flash", "automation · draft").into_any_element(),
                    kit::preview_title(d.title.clone()).into_any_element(),
                    props.into_any_element(),
                    kit::preview_text("It shows in Automations, and you can cancel it there or from here.")
                        .into_any_element(),
                ]
            }
            Card::Confirm(item, title) => vec![
                kit::preview_eyebrow("git-merge", format!("{} · #{}", item.repo, item.number)).into_any_element(),
                kit::preview_title(title.clone()).into_any_element(),
                kit::preview_text("Merging writes to GitHub. The Merge button checks it can be merged first and says why when it can't.")
                    .into_any_element(),
            ],
        }
    }
}

fn error_preview(e: &clip::ErrorText) -> Vec<AnyElement> {
    let lines: Vec<&str> = e.text.lines().collect();
    vec![
        kit::preview_eyebrow("alert-circle", format!("clipboard · {} lines", lines.len()))
            .into_any_element(),
        kit::code_well()
            .children(lines.iter().take(14).enumerate().map(|(n, l)| {
                kit::code_line(
                    None,
                    l.to_string(),
                    if n == 0 || l.trim_start().starts_with('|') && l.contains("here") {
                        theme::error().into()
                    } else {
                        theme::text_subtle()
                    },
                )
            }))
            .into_any_element(),
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .child(icon("sparkles", 12.).text_color(theme::accent()))
            .child(kit::row_sub(
                "Enter starts a session with the error and its file as context",
                false,
            ))
            .into_any_element(),
    ]
}
