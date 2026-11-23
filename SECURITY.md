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
- **Agent permissions.** Tool calls running without the permission the user chose,
  permission prompts that misrepresent what will run, or project files or web content
  that make Arakun itself (not the agent's model) run commands.

Out of scope: vulnerabilities in the agent CLIs themselves (Claude Code, Droid,
OpenCode), in CEF/Chromium, or in third-party services; report those upstream. An agent
doing something harmful that its granted permissions allow is not a vulnerability in
Arakun, although we welcome ideas to make that harder.
