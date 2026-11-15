---
name: ui-change
description: Make a visual change to Arakun's GPUI interface the way the project expects - use the theme and ui design tokens so design_lint passes, see the result in a headless test Arakun with `arakun dev`, take before and after screenshots, and update the README for changed shortcuts or settings. Use when editing a view, panel, dialog, tab or component in src/, or when the user asks to restyle, lay out or fix how something looks.
---

# Change the UI

## Use the design system

Colors, grays, text sizes, radii and fonts come from `src/theme.rs` and
`src/ui.rs`. Never write a value by hand where the system has a name.

| Need | Use |
| --- | --- |
| Text color | `theme::text()`, `text_muted()`, `text_subtle()`, `text_faint()` |
| Border | `theme::border_subtle()`, `border()`, `border_strong()` |
| Background | `theme::fill_raised()`, `fill_hover()`, `fill_selected()`, `surface(..)` |
| Status | `theme::success()`, `error()`, `attention()`, `link()`, `accent()`, `danger()` |
| Text size | `ui::TEXT_XS` .. `TEXT_LG`, `TEXT_TITLE`, or `.ty(Type::…)` |
| Radius, heights | `ui::RADIUS_*`, `BUTTON_H`, `INPUT_H`, `CONTROL_H`, `TOOLBAR_H`, `PAGE_PAD` |
| Font | `theme::FONT_SANS`, `theme::FONT_MONO` |
| Building blocks | `ui::button`, `icon_button`, `card`, `field`, `segmented`, `modal_panel`, `page_title`, and `src/ui/kit.rs` |

Read the code around you and reuse its components before writing new ones. The
Design system tab (Settings → Design system → Open) shows every token and
component. A new reusable component goes in `ui`, and shows up in
`src/gallery_view.rs`.

`src/design_lint.rs` fails `cargo test` on hand-written text colors, faint
borders and fills from `theme::mix(..)`, literal `.text_size(px(N))` and font
names. Run it alone while you work:

```sh
cargo test --locked --no-default-features design_lint
```

Fix what it reports with the token it names. Don't add a file to its `EXEMPT`
list.

## See it

On Linux, `arakun dev` runs a second, fresh Arakun from your build on a virtual
display. It never touches the user's screen or their own Arakun. It needs `Xvfb`,
and `tesseract` for `dev text` and clicking by text
(`tools/setup-dev.sh --yes --display` installs both).

```sh
cargo build --no-default-features
./target/debug/arakun dev start            # this checkout as its project
./target/debug/arakun dev click "Settings" # by visible text, or x y
./target/debug/arakun dev key ctrl-f       # also: type, scroll, move, wait, text
./target/debug/arakun dev shot "$TMPDIR/after.png"
./target/debug/arakun dev stop
```

`arakun dev` with no arguments lists every subcommand. Look at the screenshot
yourself (open the PNG) before you call the change done. Check light and dark
if the change touches color.

For the "before" shot, build `main` in another worktree, or take it before your
first edit.

## Hand it over

- Keep screenshots outside the repository (`$TMPDIR`), not in `docs/screenshots/`,
  which holds the README's images.
- The pull request needs before and after screenshots (a short GIF for an
  interaction). GitHub has no API to upload images, so give the user the paths
  and say they need to be dragged into the pull request.
- Changed a shortcut, a setting or a command? Update the README, and
  `docs/windows.md` if Windows differs.
- Code for one platform goes behind `cfg(...)`. Say in the pull request which
  platforms you saw it on: `arakun dev` is Linux only.
