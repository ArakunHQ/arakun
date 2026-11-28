# Community widgets: format and permissions

Issue: ARAKUN-269 (GitHub #1152). Project: Marketplace redesign and community
widgets. Design: Marketplace v5 (ARAKUN-264), frame "5 · Community widget"
(https://doop.arakun.dev/c/sEA9hKJMVj?frame=rZZmrDNC94) and the widgets note
(https://doop.arakun.dev/c/sEA9hKJMVj?frame=Rip8BodRwI).

Status: the loader (ARAKUN-270) follows this spec. See "The loader" below for
the choices it made where the spec left room.

## Why

Nobody outside the team can add a tab or panel to Arakun today. The tabs are
compiled in (`View::EXTENSIONS` in `src/sidebar.rs`). Marketplace v5 lets people
publish widgets, so we have to decide what a widget is before we write a loader:
what it can draw, what it can read and do, and how it is packaged, updated, signed
and taken down.

## Goals

1. A widget can fill what frame 5 shows. A listing shows the name, author, version
   and its date, installs, license, the source repository and commit, Verified or
   Not verified, and a preview drawn from sample data.
2. The user approves a short list of needs on install, each in plain words with its
   reason. The widget gets those and nothing else.
3. Arakun enforces the limits, not the widget. A widget that wants more than it
   asked for has no way to get it.
4. Tokens stay in the keychain and go only to the host they belong to.
5. An install is pinned, so a changed repository can't change what runs.
6. A bad widget can be pulled from every computer that runs it.

## Non-goals

- Edit layout and saved layouts (placing a widget anywhere). See "Later".
- Widgets shared with a project, sign-ins with OAuth, web widgets, widgets on a
  paired computer. See "Later".
- The loader, the Marketplace UI and the index routes. Those are ARAKUN-270 and
  the issues after it.

## Anti-goals (failure even if it works)

- A widget runs a command, writes a file or reads a session's messages.
- A widget sees a token, or sends one to a host the user didn't approve it for.
- A widget reaches the local network or an address it didn't name.
- An update gets more access without the user approving it.
- The preview in the Marketplace touches the network or the disk.

## 1. What a widget is

A widget is a Git repository with three files:

- `widget.toml`: the manifest.
- `main.rhai`: the script.
- `sample.json`: sample data for the preview.

It may also have `icon.svg`, `README.md` and `LICENSE`, and nothing else. There are
no binaries, and the files may take 512 KB at most.

The script has two functions:

- `refresh(state)` fetches data with the host functions below and returns the new
  state (a map). Arakun calls it every `every` seconds, and never more often than
  every 30 s.
- `view(state, size)` returns blocks and nothing else. It can't call any host
  function, so it is cheap and safe to run on every redraw.

The Marketplace preview is `view(sample.json, size)`. It never runs `refresh`, so a
preview needs no permission and touches nothing.

### Why Rhai and blocks, not a web page

- Rhai is already in the app (the Isometric scenes, `src/iso/script.rs`), so there
  is no new runtime.
- The browser is optional, and CI builds without it.
- Blocks follow the theme and the design tokens by themselves.
- Most important: the script has no network, disk or process of its own. Rhai has
  none, and Arakun registers only the functions below. Each one checks the manifest
  before it does anything.

What we give up is free HTML. A web widget in its own tab, behind the browser
feature, is later work if people ask for it.

### The engine

The widget engine is stricter than the Isometric one, which uses `Engine::new()`:

- No module loading: set a module resolver that refuses every `import`. The
  default file resolver could read `.rhai` files from disk.
- `eval` is disabled.
- `print` and `debug` go to the widget's log in Arakun, not to stdout.
- `set_max_operations`, as the Isometric scenes do, plus caps on expression depth
  and on the size of strings, arrays and maps.
- Only the host functions for the needs the user approved are registered. `view`
  runs on an engine with no host functions at all.

## 2. What it can draw

### Blocks, version 1

The manifest says `blocks = 1`. Version 1 has:

| Block | What it is |
| -- | -- |
| `stack`, `row` | Layout: children top to bottom, or side by side. |
| `text` | Plain text. |
| `stat` | A number with a label and a change. |
| `dot` | A status. |
| `badge` | A short label. |
| `list` | Rows with a dot, a title, a detail and a time. |
| `table` | Rows and columns of text. |
| `sparkline`, `bars`, `meter` | Small charts. |
| `button` | Runs an action (see "What it can read and do"). |
| `link` | Opens an address, after asking. |
| `empty` | A message when there is no data. |

- Colors are tones only: success, error, attention, link, muted and category 1–8.
  Never hex values.
- Text is plain. Never Markdown or HTML.
- A widget that asks for a newer block version than the app has is not offered for
  install.
- A block the app doesn't know is drawn as "This widget needs a newer Arakun".

### Places in v5

A widget shows in one of the places that exist today:

- the Widgets segment of the Workspace panel (sizes `small` and `wide`),
- its own tab (`tab`),
- a bar (`bar`, one line).

The manifest lists the sizes it can draw. The user picks the place on install and
can change it from the ⋯ menu.

## 3. What it can read and do

The user approves the `[needs]` list on install, shown as the plain lines of the
Needs step. Each line has a `why`, shown as written.

| Need | What the script gets | Limits Arakun enforces |
| -- | -- | -- |
| `net` (a host) | `fetch(#{ url, method, headers, body, auth })` | HTTPS only. Exact host names: no wildcards, no IP addresses. A host that resolves to a private or loopback address is refused. Redirects only to the same host. No cookies. 10 s timeout, 1 MB answer. At most once per the `every` the line names. |
| `file` (a path) | `read_file(path)` | Read only. Exact paths in the project the widget is placed in. No `..` and no links out of it. A file that `core/src/secrets.rs` sees as a secret file (`secret_path`, such as `.env`) is refused even if it is named. 256 KB. |
| `git` | `git()`: branch, head commit, remote owner and repository | Nothing else from Git: no diffs, no log, no other branches. |
| `sessions` | `sessions()`: title, state, agent and branch of each session in this project | Never messages, prompts, diffs or output. |
| `notices` | `notice(title, text)` | Shown with the widget's name. At most one a minute. The user can mute a widget's notices. |
| `secret` (a token for a host) | Nothing. `fetch(#{ auth: "vercel" })` makes Arakun add the header. | The script never sees the token. Arakun adds it only to a request to the host the line names. Kept in the keychain (`core/src/platform.rs`, `keyring`) or as a password manager reference. |
| `action` | `button(#{ action: "start_session", prompt })`, `open_url` | Asks every time, and shows exactly what will happen (the prompt, the address). |

**No widget can**, verified or not: run commands, write or delete files, read
session contents, read other secrets or environment variables, read the clipboard,
or reach an address it did not name. There is no function for any of these.

### Limits per script

- Each call stops after a set number of Rhai operations.
- Strings, arrays and maps have size caps.
- When `refresh` fails, the widget keeps its last state and shows when that was and
  the error. It waits longer after each failure, up to 15 minutes.
- A widget that keeps hitting the limits is paused, with a notice.

## 4. Packaging and versions

- The id is `<author>/<name>`, such as `mrvn/deploy-board`.
- `version` is semver, and the repository has a matching tag (`v0.4.1`).
- An install pins the repository, the commit and a SHA-256 of the files. Arakun
  downloads the files at that commit and refuses them if the hash differs from the
  index, so a force-push can't swap them.
- Installed files go in the data folder under `widgets/<author>/<name>/<version>/`.
  The install settings (place, size, paused, the needs approved, secret references)
  go in `widgets.toml` there. Both go in the list in `core/src/paths.rs` with the
  loader.
- A widget is installed for one person on one computer ("Just me"). It runs on the
  computer the app runs on.

## 5. Index, Verified and signing

- The index is served by the Worker in `cloud/`. Each entry has the listing (name,
  author, about, license, repository, installs) and its versions: version, commit,
  date, file hash, the needs, Verified or not, and Pulled or not.
- **Publish a widget** (frame 5) sends a repository and a tag. The Worker fetches
  it, checks the manifest, the files and the size, and adds the version as Not
  verified.
- **Verified** means Arakun reviewed that exact commit and confirmed the author's
  GitHub account. Arakun then signs the version entry (id, version, commit, file
  hash, needs) with a widget key, using minisign as the releases do
  (`minisign-verify` is already a dependency, see `src/update.rs`). The app shows
  Verified only when the signature checks out.
- Verified belongs to a version, not a widget. A new version is Not verified until
  it is reviewed.
- Not verified widgets install too, with the warning, a link to the source at that
  commit, and Report.

## 6. Updates

The app reads the index once a day and when the Marketplace opens.

- A new version with the same needs or fewer installs by itself.
- A new version that needs more goes through the Needs step again: a new host,
  file, secret, action or need, or a shorter `every`. Until the user allows it, the
  old version keeps running.

## 7. Taking a widget down

- Report sends the widget, the version and a reason to the Worker. A person at
  Arakun decides.
- The index can mark a version, or the whole widget, Pulled, with a reason. When
  the app next reads the index, it stops a pulled widget, removes its files and
  shows a notice with the reason. It offers to forget the tokens the widget used.
- An author can pull their own version. The app then moves to the newest version
  that is not pulled and needs no more than the user already approved.

## Example: the Deploy board in frame 5

`widget.toml`:

```toml
[widget]
id = "mrvn/deploy-board"
name = "Deploy board"
about = "Vercel deploys and their checks, live"
version = "0.4.1"
license = "MIT"
blocks = 1
sizes = ["wide", "tab"]
every = "30s"

[needs]
git = true
sessions = false
notices = false

[[needs.net]]
host = "api.vercel.com"
why = "Deploys for your project"
every = "30s"

[[needs.net]]
host = "api.github.com"
why = "Checks on this repository. Nothing else."
every = "60s"

[[needs.file]]
path = "vercel.json"
why = "In this project only, to find the project id"

[[needs.secret]]
name = "vercel"
label = "Vercel token"
host = "api.vercel.com"
header = "Authorization"
format = "Bearer {}"
get_one = "https://vercel.com/account/tokens"

[[needs.action]]
kind = "start_session"
why = "Start a session with the build log"
```

`main.rhai`:

```rhai
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
```

## The loader (ARAKUN-270)

The runtime is `core/src/widget/`. Where this spec left room, it decides:

- **The file hash** is SHA-256 over the files sorted by name, each as its
  name, a zero byte, its length (8 bytes, little endian) and its bytes, in
  lowercase hex (`manifest::hash`). The index must compute the same. Arakun
  hashes the files again each time it loads them, so a file changed on disk
  doesn't run.
- **"At most once per `every`"** for a host means one window per `every`, with
  up to 5 requests in it, so a refresh can make a few calls to one API.
- **A call outside the grant** stops the widget even when the script catches
  the error. The row says what it asked for, such as "It asked to reach
  evil.example.com, which you did not allow".
- **A token not added yet** is the Waiting state ("Needs your Vercel token"),
  not Broken. Tokens are kept in the keyring as `widget:<id>:<secret name>`.
- **Requests** resolve the host and refuse it when any address is private,
  loopback, link-local or reserved. The connection uses the addresses that
  were checked. There is no proxy and no cookie jar, a new client is made for
  each request, and the methods are GET, POST and HEAD.
- **Hitting the limits** three refreshes in a row stops the widget (Broken).
- **An update** that asks for more, or that no longer draws in its place,
  waits as Update available. Its files are kept, and the old version keeps
  running until the user allows it.

## Code this builds on

- `src/iso/script.rs`: the Rhai engine and `set_max_operations`.
- `src/update.rs`: minisign checks.
- `core/src/platform.rs`: the keyring.
- `core/src/secrets.rs`: `secret_path`, the secret file check.
- `src/http.rs`: the HTTP clients.
- `src/notices.rs`: notices.
- `src/sidebar.rs`: `View::EXTENSIONS`, the compiled-in tabs.

## Later (not in v5)

- Edit layout and saved layouts: placing a widget anywhere, and layout code that
  carries places, not widgets.
- Widgets shared with a project ("Everyone in arakun", in `.arakun/widgets.toml`),
  where each person approves the needs.
- "Sign in with Vercel" and other sign-ins. Each needs an OAuth app per service. v5
  has Paste a token and password manager references.
- Web widgets in their own tab, behind the browser feature.
- Widgets on a paired computer.

## Open questions for the review

1. Is 30 s the right shortest refresh, or should a verified widget be allowed 10 s?
2. Should MCP servers, plugins and skills share this index later, or stay on their
   own sources?
3. Is `git` (branch, commit, remote) enough, or do widgets need CI checks from
   Arakun too, rather than calling `api.github.com` with their own token?
4. Who reviews for Verified, and how fast do we promise to answer a report?
