# Server

To run sessions on another machine, start a server there and reach it from a device.
The app's Computers tab lists this computer first, then your servers, each with its
status as a dot and a word, and what its sessions are doing. Pick one to see how it is
reached, its environments and their sessions (a session opens in its project), its
agents and their plan limits, and its settings. A server signed in to your account
(`arakun server login`) joins the list by itself: no link is needed, and the connection
goes through the relay, end to end encrypted, checked against the key your account
lists for that server. Signed out, the list says your account's servers show up once
you sign in. A server reached directly (`--reach lan` or `--reach tailscale`) is paired
with a link instead: **Add a computer** shows both ways. **Remove**, in a server's
settings or its ⋯ menu, asks once, then has the server forget the app and, for a
server on your account, takes it off the account too. Under the servers, **Cloud**
offers a cloud environment: a server Arakun runs for you, so sessions keep working with
your computer off. Access is given by hand: ask for it there, with what you would use it
for. With access, **New cloud environment** makes one (the first start takes a few
minutes), and the section shows whether it is awake, why it stopped, and the hours left
this month, with **Wake**, **Stop** and **Remove**. The phone's Computers screen has the
same.

The commands here are `arakun server …`. On a machine without a display, the same
commands are `arakun-server …` (see [Install on a machine without a display](#install-on-a-machine-without-a-display)).

```sh
arakun server login          # once: sign in with GitHub, for the relay
arakun server serve          # runs until Ctrl+C, a signal or `arakun server quit`
arakun server pair           # in another terminal: where a device signs in, or a link
arakun server machines       # the account's machines, this one marked
arakun server forget <label> # remove one of them from the account
```

`login` prints a code and `https://github.com/login/device`. Open that address on any
device, enter the code and approve, and the terminal says "Signed in as <login>." The
server keeps the sign-in in `account.json` in its folder and tells a running server,
which reaches the relay as your account from then on. Ctrl+C gives up, and so does a
code that runs out; when GitHub cannot be reached for a while, `login` says so and
keeps trying. `login` again asks the service whether the sign-in still works:
if it does, `login` says who is signed in and tells a running server, which connects
to the relay again if the relay had turned it away; if it was ended, `login` signs in
again. `login --force` signs in again in any case and ends the old sign-in. A sign-in
for another service is replaced, and `login` says so first. `logout` ends the sign-in
and removes the file; a running server stays up, leaves the relay, and devices on the
network or Tailscale stay. When the service cannot be reached, `logout` still removes
the file, but exits with code 1 and names the session, which is good until you sign it
out from another signed-in device.

`serve` through the relay needs a sign-in. Started in a terminal without one, it stops
and says to run `login`, or to serve with `--reach lan` or `--reach tailscale`, which
need no account. Started with `--service` (as `service install` starts it), or with no
terminal to print to, it stays up signed out instead, logs one line that says to run
`arakun server login`, and connects as soon as `login` tells it: a service that exited
would only be started again every few seconds. A server the relay signs out later (the
sign-in was ended from another device, say) keeps running, keeps the file, and logs a
line that says to run `login`. Once the account itself answers that the server's
sign-in is over, the server forgets every device the relay let in, with its
notifications, and asks nothing more until you run `login`: it can no longer learn
which of them were signed out. They are let in again when they connect after `login`.
A sign-in lasts at most 180 days (90 without use), so a server nobody touches is signed
out that way within half a year: run `arakun server login` on it again. `status` shows
"Signed in as <login>", "Not signed in", or that the sign-in ended.

`machines` lists the account's machines with their label, kind and whether they are
online, and `forget <label>` removes one from the account. The relay hangs up on a
removed machine that is running, and it stays out until someone acts: a server comes
back with `arakun server login` (plain `login` is enough) or a restart, and its log and
`status` say so; `logout` keeps it out. A machine that was off is listed again when it
next connects signed in. At start, after `login` or `logout`, and every 15 minutes
while a device the relay let in gets notifications, the server asks the account for its
signed-in devices and drops the ones it no longer lists, with their notifications. An
ask that gets no answer drops nothing.

A sign-in is good only at the service it came from: `https://app.arakun.dev`, or
`ARAKUN_TEAMS_URL` when set. When `--relay` names another relay, the server starts
signed out there and says so; sign in to that one with
`ARAKUN_TEAMS_URL=<relay> arakun server login`. While the relay has the server signed
out, `pair` says so instead of where to sign in.

Through the relay, `pair` prints no link: it says to open `https://app.arakun.dev` on
the device and sign in as the same GitHub account, and the device is let in. On
Tailscale and the local network, `pair` prints a link and its QR code. The link is good
for two minutes and pairs one device. It names the server's key, and the device checks
every connection against it: pairing, and everything after, goes end-to-end encrypted,
as through the relay. The app and the server must both be this version or newer to
reach each other this way. There are two ways to use it:

- **In the app.** Open the Computers tab, press **Add a computer**, pick **By link** and paste the link (a link of the old relay
  says to sign in on the server with `arakun server login` instead). From then on the app
  connects to the server when it starts, and works there as if you were at that
  machine: each folder the server offers is an environment in the sidebar, marked with
  the machine's name. A folder whose repository you also have here (the same `origin`)
  joins that project; the others make projects of their own. Its sessions open in the
  same tabs, with the same composer, as sessions here: send, stop, approve and answer
  go to the server. "New environment" makes a worktree on a server too: Alt+click the
  + and pick the machine, or right-click a server's environment. A plain + makes it on
  the open environment's machine. To spread them instead, turn on **Spread new
  environments** at the foot of the Computers tab's list: a new environment then goes to a connected machine
  that has the project, This computer included, picking the least busy one (the fewest
  sessions at work). Set each machine to Prefer, Normal, Avoid or Off in its settings there: a preferred
  machine takes new environments before the others, an avoided one only when no other
  can, and one that is off never does (you can still pick it in the dialog). When every
  machine is off, a new environment goes to the open environment's machine. The New environment dialog starts on that machine too, and a first prompt stays
  on the machine it was written on. This is kept in `servers-balance.json`. A new environment on a server goes in
  `~/arakun-workspaces/<project>/<branch>` there. Unless naming is off in Settings ›
  Branches, Arakun names its branch after the first prompt, as here. Right-click a
  server's environment to archive it there (the sessions that ask for attention
  go idle, and their agents stop; a running turn is left alone) or to delete it: the
  server removes the worktree, everything in it and its sessions, and the branch too if
  you ask. A repository's main folder is never deleted. An environment whose folder you
  remove on the server yourself leaves the list with its sessions within a few seconds,
  unless a session there is still working or waiting for an answer. A phone paired with this app sees and works in the
  servers' sessions too. A terminal in a server's environment is a shell in that folder
  on the server, and the project commands from Settings run there too. It keeps running
  while the connection is down and picks up where it was once it is back; one no app
  follows for an hour is hung up. The Changes panel of a server's environment shows
  that folder's changes and diffs, and stage, discard, commit, pull, push and the pull
  request button run there, with the server's `git` and `gh` logins
  (`ARAKUN_GITHUB=readonly` on the server keeps it from pushing or opening pull requests;
  set it before `service install`). On a server reached over plain http, the diffs,
  commit messages and pull request text cross the network unencrypted, like the rest of
  that link: reach it over https or the relay. The Explorer lists a server's environment, its files
  open in file tabs (text only, read from the server), and `@` in the composer completes
  its paths. Merging into the main checkout and GitHub's form stay with this machine's
  folders. Fork, compact, rewind and `/stage` in a server's session run on the server, with its
  agent and its git: a fork or a rewind opens as a new session there. "Continue with"
  hands a server's session to another of the agents on that server, in the same tab.
  In a server's environment, the model menu lists the models and reasoning levels that
  server offers. A new session starts with what you pick there, and picking in a running
  session changes it on the server. The permission mode and fast mode are the server's
  own. In the Marketplace, "Install on" picks a paired server: Discover then lists
  the plugins its marketplaces offer, and Install adds a plugin or MCP server for
  Claude Code there, for every project. An MCP server that wants a sign-in is signed
  in to on that machine (`claude mcp login <name>`), since that opens a browser.
  Every paired server also mirrors this computer: when it connects, and after each
  install here, it adds the plugins (and their marketplaces), the user MCP servers
  and the skills from skills.sh it lacks. What you installed in a repo
  just for you is added to the server's folders that have the same `origin`; what you
  shared with the repo goes with git. It never removes anything you installed.
  Over plain http only plugins and skills go.
  Your MCP sign-ins (Claude Code's tokens for the MCP servers you signed in to) go
  only to a server you let have them: pairing by link asks, and the answer is no
  unless you change it. **Copy MCP sign-ins** in the server's Settings in the
  Computers tab changes it later, and the server's card says when it holds a copy.
  Turning it off takes the copy off the server when it next connects, and so does
  removing the server, or unpairing this app on it. A server paired before pairing
  asked keeps getting them until you turn it off. The server keeps a sign-in of its
  own that lasts longer, and lists which device copied each sign-in in
  `sign-ins-copied.json` in arakun's folder. A server on macOS does not take
  sign-ins, which Claude keeps in the Keychain there. The skills installed from
  skills.sh are listed in `skills-installed.json` in arakun's folder, and what was
  installed in a repo in `repo-installs.json`.
  To add a folder on a server, press **Add a folder** on it in the Computers tab,
  or right-click one of its environments. Clone runs that machine's git, with its
  logins, and puts the repository in the folder that holds most of the server's
  folders. Folders right in the home folder do not count; with none elsewhere, the
  clone goes into `~/code`. Clone takes https and ssh addresses, not plain http, and
  gives up after 20 minutes. Add offers a folder the server already has, as
  `arakun server add` does. The app keeps what pairing gave it in `servers.json`
  next to its state, which only your user can read: a server's address, with the token
  pairing gave in the system keyring (one kept in the file by an older version moves
  there the next time the server is dialed), or, for one reached through the account,
  only its label on the relay (the sign-in stays in the system keyring). A server paired through the relay before accounts
  stays in the file as it was and says it was paired by link, which the relay no longer
  takes: sign in here and run `arakun server login` on the server with the same GitHub
  account, or pair it again over Tailscale. Once your account lists it, it is kept as
  one of the account's; its old ticket is never sent again.
  While the app cannot reach your account, a server of the account says "Could not reach
  your account. Trying again." and connects once the account answers. A server whose
  user removed the app from its devices is marked in the file and is not connected
  again until you remove it and run `arakun server login` on it again. What each server listed last is
  kept in `servers-inbox.json` beside it, so its environments show at start before it
  answers, or while it cannot be reached. A server over plain http that was paired
  before its socket was sealed shows them only once the Computers tab connects it: that
  first connection keeps the server's key, and every later one is checked against it. **Remove** on a server removes it
  here and has the server forget the app. A server that cannot be reached at that
  moment keeps the app in its list: remove it there with
  `arakun server unpair <device>`.
- **On a phone, or in any browser.** Open the link. It is the page the Phone tab pairs
  phones with: it lists the sessions, reads them live, and notifies you when one
  finishes or needs you.

```sh
arakun server add ~/code/app      # a folder sessions can start in; the page lists them
arakun server new --agent claude "fix the failing test"   # start one here; prints its id
arakun server ls                  # folders and sessions
arakun server log <id>            # a session's status and last entries
arakun server wait <id>           # until it stops working; prints its answer or what it asks
arakun server send <id> <text>    # a follow-up
arakun server approve <id>        # or deny: answer the permission it asks for
arakun server stop <id>           # stop its turn; follow-ups waiting behind it are dropped
arakun server devices             # paired devices; `unpair <device>` forgets one
arakun server status
arakun server service install   # keep it running; see Keep it running
```

`arakun server help` lists them all.

- **Agents.** Claude Code, Droid, OpenCode, Codex, Devin, Cursor, Antigravity and Workers AI, as in the app. Sign in to
  each one on that machine first: the server uses its logins and its permission
  settings. An agent set to allow everything there asks for nothing here either.
  Claude Code and Codex also need their ACP adapter. The server installs a missing one
  with npm the first time a session needs it, into `~/.local/share/arakun/agents`
  (`%LOCALAPPDATA%\arakun\agents` on Windows), so Node 22 or newer must be on the
  server's `PATH`; the server install script puts one there. The service keeps the `PATH` it was installed
  with: if Node comes from mise or nvm, run `arakun server service install` from a shell
  where `node` works.
- **Secrets.** When an agent there needs a login, token or password, it asks with a
  private card (`request_secret`), as in the app; the app and the phone page show it, and
  what you type stays out of the session. It goes only over an encrypted connection: the
  relay, Tailscale or https. Over `--reach lan`, which is plain http, the card offers
  only Decline. A sandboxed agent there doesn't get the card yet.
- **How devices reach it.** `serve --reach relay` (the default) dials out to the relay,
  so it works behind a firewall, and everything between the device and the server is
  end to end encrypted; `--relay <url>` names a relay of your own. It needs `login`
  (before or after it starts as a service), and lets in the devices signed in to the
  same account, with nothing to pair. `--reach tailscale`
  serves it at the machine's tailnet name over HTTPS; that takes the machine's HTTPS
  port on the tailnet, as the app's Phone tab on Tailscale does, so only one of the two
  can use it on a machine. `--reach lan` listens on every
  interface over plain HTTP (`--port`, 7474 unless told): nothing is encrypted, and
  whoever can watch that network can read what a device is let in with. Use it only on a
  network you trust, and Tailscale or the relay across networks. What `serve` is started
  with is kept for the next run.
- **What it keeps.** `~/.local/share/arakun/server/`, which only your user can read:
  `phone.json` (how it is reached, its relay identity, the paired devices, and the
  devices that came in through the relay, with no token), `account.json` (the account
  it is signed in to and the sign-in that lets it in to the relay; `logout` removes it),
  `state.json` (folders and sessions), `sessions/` (transcripts, saved as the agents
  work), `linked/` (the conversation of a session handed to another agent, as
  markdown that the new agent reads), `uploads/` (photos sent with prompts),
  `linear.json` (the Linear settings the app sent it: the switch, the mappings to its
  projects, the Linear key of each mapped workspace, and how many of its sessions
  work at once) and `linear-runs.json` (the
  issues from Linear it runs). `ARAKUN_SERVER_DIR` names another
  folder, for a second server on the machine. The app's own files are not touched: the
  app and a server can run side by side.
- **The local socket.** The commands above reach the server over `server.sock` in
  that folder, which only your user can open. It is also what keeps a second server
  off the same folder.
- **Issues delegated in Linear.** A server takes the Linear teams and projects that
  the app's Linear tab maps to its projects (see the Linear section). It runs them as
  the app does: it reads the issue with the workspace's Linear key the app gave it,
  checks the mapping, makes a worktree on the issue's branch under
  `~/arakun-workspaces/`, and starts a session there with the project's agent that
  ends in a draft pull request. When the issue has an open pull request in the
  project's repository, the worktree is on its branch and the session pushes to it
  instead; this needs `gh` signed in on the server. At most three run at once; the rest wait, and a
  reply to one that waits is kept for when it starts. Replies
  and stops from Linear reach the session (a reply that starts with `!` interrupts
  the turn), and the issue hears what it does. It needs the
  sign-in (`login`) and the relay. `linear-runs.json` in its folder holds the
  deliveries handled, each run, and the reports Linear has not taken yet (dropped
  after a day). A server that stops mid-session tells the issue when it starts again;
  a reply in Linear continues the session.
- **A turn that is running when the server stops** is cut off. Its session is still
  there after a restart, and the next message picks it up. Follow-ups that were
  waiting behind that turn run first, then the new message.

## Keep it running

`service install` runs `serve --service` as a service: systemd on Linux, launchd on
macOS. It starts the server now, again after a crash, and at boot (Linux) or login
(macOS). On Linux it keeps running while nobody is logged in, so on a VPS you can
install it over SSH and log out. `quit` stops it until the next boot or login. A
binary you built yourself goes somewhere that stays first, not in `target/`
(`install -Dm755 target/release/arakun-server ~/.local/bin/arakun-server`).

```sh
arakun server login               # through the relay: sign in, before or after install
arakun server service install     # takes serve's flags: --reach, --port, --relay
arakun server pair                # where a device signs in (or a link), then log out if you like
arakun server service status      # installed, running, lingering, and how it is reached
arakun server service logs -f     # what it prints (journalctl, or server.log on macOS)
arakun server service uninstall   # stop it and remove the service; the data stays
```

- **What it writes.** `~/.config/systemd/user/arakun-server.service` on Linux,
  `/etc/systemd/system/arakun-server.service` when you run it as root, and
  `~/Library/LaunchAgents/dev.arakun.server.plist` on macOS. `service install --print`
  shows the file and installs nothing. Running `install` again rewrites the file and
  restarts the server.
- **Not signed in.** Through the relay, a service with no sign-in does not fail: it
  runs signed out, `install` ends by saying to run `arakun server login`, and the
  running server takes the sign-in with no restart. The same holds after `logout`, and
  for a service installed before servers signed in (its file has no `--service`, and a
  server with no terminal waits just the same): after updating, run
  `arakun server login` once, and nothing else.
- **Agents.** The service gets the `PATH` of the shell you install it from, so it finds
  the same agents. If you install an agent somewhere else later, run `install` again.
  It also keeps `ARAKUN_GITHUB=readonly` when that is set in that shell.
  It also keeps `XDG_DATA_HOME` and `ARAKUN_SERVER_DIR` when they are set, so it uses
  the folder the other commands look in.
- **Logged out (Linux).** As a normal user it turns on lingering
  (`loginctl enable-linger`) before it starts the unit: systemd then starts your
  services at boot and keeps them running after you log out. If that needs root, it
  prints the `sudo` command to run. It also works after `su - user` or `sudo -u user`,
  where `systemctl --user` usually cannot find the user's systemd.
- **As root.** Many VPSs only have root. There it installs a system unit that runs as
  root and starts once the network is up. Because it runs as root at every boot, the
  binary must be owned by root in folders only root can write to (`/usr/local/bin`,
  not a home folder), and only such folders are kept on its `PATH`. To run the agents
  as another user, log in as that user and install from there.
- **Logged out (macOS).** A user's launchd agent runs from login to logout, so a Mac
  that nobody sits at needs automatic login.
- **A second server.** With `ARAKUN_SERVER_DIR` set, the service is named after that
  folder (`arakun-server-<folder>`) and keeps the variable.
- **Windows** has no `service` command yet: start `arakun server serve --service` from
  Task Scheduler at logon.

Tried on Linux and macOS.

## Install on a machine without a display

**The server alone (Linux x86_64 and arm64):** for a machine with no display, such as a VPS, install
only `arakun-server`, the same server without the window:

```sh
curl -fsSL https://arakun.dev/install-server | sh
curl -fsSL https://arakun.dev/install | sh -s -- --server    # the same
```

It is the install script with `--server`: it checks the release the same way and installs
`~/.local/bin/arakun-server` (uninstall: `rm ~/.local/bin/arakun-server`). It also
installs what the server runs agents with, if it is missing: git and gh with the system's
package manager (as root, or with `sudo` that needs no password; otherwise it prints the
command to run), and Node.js 22 from nodejs.org in `~/.local/share/arakun/node`, linked
as `node`, `npm` and `npx` in `~/.local/bin`. Without Node, a new session fails as it
starts. When no `claude` is on the `PATH`, it installs Claude Code with
[its own installer](https://docs.anthropic.com/en/docs/claude-code/setup) in
`~/.local/bin/claude`; run `claude` once to sign in. `--no-tools` leaves all four to you. It takes `--version` too. Releases after v0.4.0 carry it. It does not update itself: run the line
again for a new release, then restart the service if it runs as one:
`systemctl --user restart arakun-server`. A server in another folder
(`ARAKUN_SERVER_DIR`) is the unit `arakun-server-<folder>`, and a service installed as
root needs `systemctl` without `--user`.

## Build it from source

`arakun-server` is the server built without the window. It needs none of the
system libraries above, only a C compiler, and the binary runs on a machine that has
no desktop installed:

```sh
cargo build --release --locked -p arakun-server
./target/release/arakun-server help
```

Build it on its own, as above. Built in the same command as the app, Cargo gives both
the same GPUI, with the display code in it. `arakun server login` needs no browser on
that machine either: you enter its code on another device. On Linux x86_64 and arm64 you can also install
the release build instead: `curl -fsSL https://arakun.dev/install-server | sh` (see
[above](#install-on-a-machine-without-a-display)).
