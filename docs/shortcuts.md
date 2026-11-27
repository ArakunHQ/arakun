# Keyboard shortcuts

On macOS, `Cmd` takes the place of `Ctrl` in the shortcuts below, except for
`Ctrl+Tab` and ``Ctrl+` ``, and the menu bar lists them. `Cmd+[` / `Cmd+]` go back and
forward through tabs, `Ctrl+Cmd+F` toggles full screen, and `Cmd+H`, `Cmd+M` and
`Cmd+Q` hide, minimize and quit.

<!-- shortcuts: begin; written from src/keymap.rs -->

**Navigate**

| Keys | What they do |
| -- | -- |
| `Ctrl+K` / `Ctrl+P` | Open the palette: jump to a session, environment or command |
| `Alt+Up` / `Alt+Down` | Previous / next environment |
| `Alt+Left` / `Alt+Right` | Back / forward through tabs |
| `Ctrl+B` | Hide or show the sidebar |
| `Ctrl+Alt+B` | Hide or show the Workspace panel |
| `Ctrl+Alt+S` | Switch the Workspace panel to its Session tab and back |
| `Ctrl+F` | Search the session |
| `Ctrl+Alt+1` … `Ctrl+Alt+9` | Open pin 1–9, from anywhere |
| `Ctrl+,` | Settings |

**Projects and sessions**

| Keys | What they do |
| -- | -- |
| `Ctrl+T` | New session |
| `Ctrl+Shift+N` | New environment, at once on a random branch |
| `Ctrl+Alt+N` | New environment, asking for the branch and options first |
| `Alt+1` … `Alt+9` | Go to the sessions of project 1–9, then ↑ ↓ and Enter |
| `Ctrl+O` | Add project |

**Tabs and panes**

| Keys | What they do |
| -- | -- |
| `Ctrl+W` | Close the tab |
| `Ctrl+Alt+T` | Reopen the last closed tab where it was |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` / `Ctrl+PgDn` / `Ctrl+PgUp` | Next / previous tab, across environments |
| `Ctrl+1` … `Ctrl+9` | Go to tab 1–8, or the last tab |
| `Ctrl+Shift+PgUp` / `Ctrl+Shift+PgDn` | Move the tab left / right |
| `Alt+P` | Pin or unpin the session's tab in the title bar |
| `Ctrl+D` / `Ctrl+Shift+D` | Split right / down (again to unsplit) |
| `Ctrl+Alt+Arrows` | Move the focus to the other pane |
| `Ctrl+Alt+Shift+Arrows` | Swap the panes |

**Terminal and browser**

| Keys | What they do |
| -- | -- |
| `Ctrl+J` / `` Ctrl+` `` | Show or hide the terminal dock |
| `Ctrl+Shift+T` / `` Ctrl+Shift+` `` | New terminal |
| `Ctrl+Shift+F` | Find in the terminal (Enter older, ↓ newer) |
| `Ctrl+Shift+B` | New browser tab |

**Zoom and screen**

| Keys | What they do |
| -- | -- |
| `Ctrl+=` / `Ctrl++` / `Ctrl+-` / `Ctrl+0` | Zoom the interface in / out / back to 100% |
| `Ctrl+Shift+H` | Redacted mode: hide emails, keys and codes on screen |

**In the palette**

| Keys | What they do |
| -- | -- |
| `Right` | Open a row's actions, or go into it |
| `Left` | Go back |
| `Space` | Mark rows, after the arrows, for the bar at the bottom |
| `Alt+P` | Pin or unpin the selected row |
| `Alt+Shift+Up` / `Alt+Shift+Down` | Move the selected pin up / down |
| `Tab` | Switch the filter |
| `Ctrl+Enter` | Ask an agent |

**In the composer**

| Keys | What they do |
| -- | -- |
| `Enter` | Send, or queue while the agent works |
| `Shift+Enter` | New line |
| `Ctrl+Enter` | Stop the turn and send now |
| `Esc` | Stop the running agent turn |
| `Up` | Earlier prompts, on an empty composer |
| `Ctrl+S` | Save the draft as a ! prompt |

**In the GitHub tab**

| Keys | What they do |
| -- | -- |
| `J` / `K` / `Down` / `Up` | Next / previous item |
| `Enter` / `Esc` | Open the item's sheet / close it |
| `A` | Approve |
| `C` | Comment |
| `R` | Review with an agent |
| `U` | Catch me up |
| `S` | Summarize |
| `D` | All or fewer details |
| `O` | Open on GitHub |
| `1` / `2` / `3` / `4` | On a pilot card: send ready answer 1–4 |
| `E` | On a pilot card: Other answer… |
| `Y` | On a pilot card: mark it dealt with |

<!-- shortcuts: end -->

More about some of them:

- `Ctrl+T` makes its new session in a new worktree, made when you send the first
  prompt, unless the open one is a worktree without one.
- A middle click closes a tab, dragging it left or right moves it there (a line marks
  where it lands, and the order stays after a restart), and its right-click menu also
  opens a new session or closes the other tabs of its environment or those to its
  right. The tab on screen stays open when its environment has no other. `Alt+P` pins
  or unpins a session (see Pinned tabs). GNOME, Ubuntu and some KDE setups keep
  `Ctrl+Alt+T` to open their own terminal; there, use Reopen closed tab in the
  palette or a tab's right-click menu
- `Tab` / `Shift+Tab` move between buttons, switches and menu rows, and `Enter` or
  `Space` presses the one with the ring. Terminals, web pages, fields and the composer
  keep `Tab` for themselves
- The palette (`Ctrl+K` or `Ctrl+P`): sessions of every project, GitHub, runs, files and
  symbols, views, commands, settings and memory in one search. Start with `>` for
  commands, `/` for files, `:` for settings, `@` for people or `#` for pull requests.
  `@` followed by a model name (`@opus fix the build`) asks that model instead. With
  nothing typed, the palette ends with these and the sentences below, each with an
  example; a click types its start. Every command of the macOS menu bar is in the
  palette too, so Linux and Windows reach the same ones.
  `#` lists every open pull request of your projects' repositories, not only yours;
  `#86` finds that one even when it is closed or merged, and a first word that names a
  repository or project narrows to it (`#arakun 86`, `#frameshi`).
  In it: `→` opens a row's actions or goes into it (a project, a person, Branches), `←`
  goes back; `Space` after the arrows marks rows for the bar at the bottom; `Alt+P` pins
  a row to the top of the palette and the sidebar, `Ctrl+Alt+1...9` opens a pin (in the
  palette too), and `Alt+Shift+Up` / `Alt+Shift+Down` reorder them; `Ctrl+Enter` asks an
  agent. A key the window uses means the same in the palette: `Ctrl+1...9` still
  switches tabs and `Alt+Up` / `Alt+Down` environments. On a cloud task card (`cloud: <task>`), `Enter` sends it, `Tab` switches
  between the open pull request and a new branch (only when there is a pull request),
  and `Esc` cancels. A question offers a short answer from Claude Haiku through Claude Code, on
  your Claude account or the key or endpoint Claude Code is set up with: `Enter` on the offer asks, and nothing is sent
  before. A sentence such as "merge #64 when the checks pass and tell me" becomes an
  automation once you confirm it; one Arakun cannot read itself offers to have Claude
  read it, again only on `Enter`.
  "remind me in 20 min to review the PR", "remind me at 5pm to stretch" or
  "timer 10 min" sets a reminder that rings on the island. One that comes due while
  Arakun is closed rings, marked as missed, the next time it opens. "to-do: …" (or a
  reminder with no time) adds a to-do, and typing "to-do" lists them,
  `Enter` ticking one off. Pins are kept in `pins.json`, rules (reminders too) in
  `rules.json`, to-dos in `todos.json` and what you open in `palette-frecency.json`,
  next to the state file.
- `Ctrl+=` / `Ctrl+-` / `Ctrl+0` zoom the whole interface (in a browser tab, the page);
  Settings › Appearance › Zoom does it too
- `Ctrl+Shift+H` turns [redacted mode](privacy.md#redacted-mode) on or off
- In the composer: `Enter` sends, `Shift+Enter` new line, `Up` on an empty composer
  recalls earlier prompts (in a Team channel it edits your last message instead);
  paste or drop images to attach them. While the agent works,
  `Enter` queues the message, `Ctrl+Enter` stops the turn and sends it now, and `Esc`
  stops the turn. A queued message has a button to send it now too
- In the composer's agent and model menu: the strip of agent icons at the bottom shows
  an agent's models and reasoning, and a dot marks the agent in use. `↑` / `↓` move
  through the models and the rows below them, `←` / `→` move between agents (on the
  Reasoning row they step the level), `Enter` picks and `Esc` closes. Picking a model
  of the agent in use or a reasoning level keeps the menu open, so you can set the
  reasoning next. In a running session, only picking one of another agent's models or
  Continue with hands the session to another agent; reasoning and fast
  mode for another agent are kept for when you switch to it. Picking a different
  model for that agent clears the reasoning level kept for it, so pick the level
  again after the model
- In the GitHub tab: `J` / `K` (or `↓` / `↑`) next and previous item, `A` approve,
  `C` comment, `R` review with an agent, `U` catch me up, `S` summarize, `D` all or
  fewer details, `O` open on GitHub. `Enter` opens the selected item's sheet, and `Esc` closes it (the sheet an
  island notice opened over another tab too). While the sheet is closed, `J` / `K`
  only move through the list; while it is open, they open the next item. On a pilot
  card you clicked, `1` to `4` send its ready answers, `E` opens "Other answer…" and
  `Y` marks a fixed or failed card dealt with, in the queue or the sheet; the buttons
  show their keys, and after an answer the next question is picked (with the sheet open,
  only from that pull request's cards). Moving to another pull request lets go of the
  card. In the comment box, `Enter` sends the
  comment and `/` lists the actions
