# Security policy

Arakun runs coding agents on your machine, exposes a local control socket, sandboxes
headless agents, and lets paired phones control sessions, possibly through a relay on the
internet. We take reports about any of these seriously.

## Reporting a vulnerability

Please **do not open a public issue** for security problems.

Report it privately through GitHub Security Advisories:
<https://github.com/arakunhq/arakun/security/advisories/new>
(the repository's **Security** tab, then **Report a vulnerability**).

Include what you can of:

- the affected version or commit, and the platform;
- what an attacker can do, and what they need first (local user, same network, a paired
  phone, a malicious repository or web page, a prompt injection...);
- steps or a proof of concept to reproduce it.

We will acknowledge the report, keep you updated while we work on a fix, and credit you in
the advisory unless you prefer otherwise. Please give us a reasonable time to release a
fix before disclosing details publicly.

## Supported versions

Arakun is pre-1.0. Fixes go into the latest release and `main`; older releases are not
patched.

## Scope

Of particular interest:

- **Control socket.** `arakun <command>` talks to the running app over a local socket
  (`$XDG_RUNTIME_DIR/arakun/control.sock`, `%LOCALAPPDATA%\arakun\control.sock`). Another
  local user or a process outside Arakun driving the app, or a command doing more than it
  should (for example `arakun page eval` on a non-local page).
- **Headless sandbox and policy.** `arakun agent run` runs tools inside bubblewrap with a
  policy that decides what needs approval. Escapes from the sandbox, reaching credentials
  or the network without `--allow`, actions run on the host without approval, or ways to
  get around the policy.
- **Phone remote, relay and pairing.** The phone server on the LAN or Tailscale, the
  pairing codes and device tokens, the end-to-end encryption between phone and computer,
  and the relay in `cloud/src/relay/` (including the hosted relay at `arakun.dev`). Pairing without
  the code, reading or injecting traffic, impersonating a computer or phone, or one
  computer reaching another's sessions.

## What the phone's ways in protect

A phone (or the app reaching an `arakun-server`) talks to the computer one of three
ways. All three use the same end-to-end encryption (`core/src/phone/e2e.rs`,
`phone/public/e2e.js`): an ephemeral P-256 key exchange signed by the computer's
identity key, then AES-256-GCM with a counter per direction, so a frame that is
replayed, dropped or changed ends the connection.

| Way | The page comes over | The phone checks the computer by | What crosses the network in the clear |
| --- | --- | --- | --- |
| Local network | Plain HTTP | The key label in the pairing link (`k=`), or the key it met when the code was typed | The page's files |
| Tailscale | HTTPS from `tailscale serve` | The same, and Tailscale's certificate | Nothing |
| Relay | HTTPS from the account host | The key the account lists for that computer | Nothing; the relay sees sealed frames only |

On the local network and over Tailscale, the pairing code, the device token, the
sessions, notification settings and images all go inside the sealed socket at `/e2e`.
The computer answers nothing else over plain HTTP. Someone who only listens on the
same network learns nothing from it. Someone who can change traffic on the local
network can change the page itself, since it comes over plain HTTP, and so take what
the page holds. Use Tailscale or the relay where that matters.

The page keeps the device token in `localStorage`, and over HTTPS in a cookie that
script can read, for the iOS home-screen app. The page has to read the token to send it
inside the sealed channel, so an HttpOnly cookie would not work. The page's content
policy allows scripts from the computer only, so a transcript cannot run script in it.
- **Agent permissions.** Tool calls running without the permission the user chose,
  permission prompts that misrepresent what will run, or project files or web content
  that make Arakun itself (not the agent's model) run commands.

Out of scope: vulnerabilities in the agent CLIs themselves (Claude Code, Droid,
OpenCode), in CEF/Chromium, or in third-party services; report those upstream. An agent
doing something harmful that its granted permissions allow is not a vulnerability in
Arakun, although we welcome ideas to make that harder.
