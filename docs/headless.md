# Headless and sandboxed agents

How Arakun runs agents without its window, and what it keeps from agents that run
in it.

## Headless agents

`arakun agent start --headless` (or `arakun agent run`, the same) starts an agent
in its own process that keeps going with the app closed. Its tools run inside a sandbox (workspace writable, the
rest read-only, no credentials, no network unless `--allow net`), and a policy
(`--policy readonly|workspace|full`) decides which tool calls need your approval
(`arakun agent approve|deny`). The sandbox is bubblewrap on Linux (needs `bwrap`) and
Seatbelt (`sandbox-exec`) on macOS; headless runs use Workers AI models for now.

## Sandboxed agents

Turn on Sandbox agents in Settings › Project and the agents
of that project's sessions run in a sandbox: Seatbelt on macOS, bubblewrap on Linux
(needs `bwrap`). They can write the environment, the repository's Git folder (not
its config or hooks), package caches and temporary folders; other tools' logins and
tokens (`GH_TOKEN`, cloud keys), browser profiles and files that look like secrets
(`.env`, keys) stay hidden. On Linux only the secret files a scan finds at the
start are hidden (up to 8 folders deep, 50,000 entries, skipping `node_modules`,
`target` and the like); macOS also hides ones made later, by name. An environment
whose `.git` file leads to a folder its repository doesn't list won't start. They
keep the network and their own login (on macOS the keychain), and reach Arakun
through a socket of their own that refuses `arakun run`, `arakun agent` and anything
else that would act outside. On Linux, sandboxed Claude Code keeps its settings in
`~/.claude/.claude.json` (a copy of `~/.claude.json` at first), because bubblewrap
can't let it write beside the file in `$HOME`. Workers AI sessions and Windows can't
be sandboxed yet. A sandboxed
agent can still change what you later run outside it: the project's files, and its
own settings such as `~/.claude.json`.

## Asking before an agent reads a secret file

With or without the sandbox, and
with auto-allow on (`arakun agent new`), an agent asks before it reads a secret
file. This covers a read tool, `fs/read_text_file`, or a command that plainly
prints the file, such as `cat .env`, `grep KEY ~/.aws/credentials` or
`node app.js < .env`. The card names the file and says why it matters: what the
agent reads goes to its model, and shows in the chat and on your phone. **Allow
secret files this session** stops the read tools from asking again in that
session; a command still asks. Output from a secret file stays hidden in the chat
until you press **Show**, and the phone never gets it.

A secret file is:
- an env file: `.env`, `.env.*`, `.envrc`, `*.env`, `.dev.vars`;
- a key or key store: `*.pem`, `*.key`, `*.p12`, `*.pfx`, `*.jks`, `*.keystore`,
  `*.ppk`, `*.kdbx`, `*.gpg`, `*.asc`, `id_rsa` and the other `id_*` keys;
- a credentials file: `.netrc`, `.npmrc`, `.pypirc`, `.git-credentials`,
  `.pgpass`, `.htpasswd`, `kubeconfig`, `auth.json`, `credentials*`, a service
  account's JSON key, `.docker/config.json`;
- Terraform state and variables;
- a data file named for a secret, such as `password.json` or `gh-token.txt`;
- anything under `.ssh/`, `.gnupg/`, `.aws/`, `.kube/`, `.azure/` or
  `.config/gcloud/`.

Templates (`.env.example`, `.sample`, `.template`, `.dist`) are not secret files.
Claude Code reads files itself, so Arakun gives it `permissions.ask` rules for
these names on top of your own settings. A rule can't say everything above, so
some reads, such as `password.json`, ask only when Claude Code would ask anyway.

## Agents don't get your tokens

Every agent, sandboxed or not, starts without the
variables of Arakun's environment that look like secrets: names with `TOKEN`,
`SECRET`, `PASSWORD`, `CREDENTIAL`, `API_KEY` or `PRIVATE` in them or ending in
`KEY`, and ones of other tools such as `GH_*`, `GITHUB_*`, `AWS_*`, `CLOUDFLARE_*`,
`STRIPE_*`, `HF_*`, `DOCKER_*` and `NETRC`. Model providers' keys and settings
(`ANTHROPIC_*`, `CLAUDE_*`, `OPENAI_*`, `GEMINI_*` and the like) and `ARAKUN_*`
stay, and so do `AWS_*` for Bedrock in Claude Code and OpenCode. An agent outside
the sandbox also keeps the addresses of your sockets and display (`SSH_AUTH_SOCK`,
`DBUS_SESSION_BUS_ADDRESS`, `DISPLAY`), so `git push` over SSH and sign-ins still
work; the sandbox drops them with the sockets it hides. Without `GH_TOKEN`, `gh` in
a session uses the login from `gh auth login`. To give a project's agents a variable
anyway, name it under **Pass to agents** in Settings › Project (for example
`GH_TOKEN NPM_TOKEN`). Terminals keep the whole environment.
