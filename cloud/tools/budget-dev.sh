#!/usr/bin/env bash
# Local rehearsal on loopback. Never use a production config or its .dev.vars.
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
state="${BETA_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/arakun-cloud-rehearsal}"
mkdir -p "$state"
chmod 700 "$state"
if [[ ! -f "$state/beta.env" ]]; then
  (set -o noclobber; printf 'BETA_TOKEN=%s\n' "$(openssl rand -hex 32)" > "$state/beta.env")
fi
# The image gets only the runner and the committed teams/ tree of #272, never
# a worktree, its dependencies, .dev.vars, .git or host credentials.
context=.wrangler/budget-image
rm -rf "$context"
mkdir -p "$context"
install -D -m 644 runner/budget-box.ts "$context/runner/budget-box.ts"
git -C "$(git rev-parse --show-toplevel)" archive 7f0cf45530f4b488d263418e647e8c62788c1c4c teams | tar -x -C "$context"
printf '**\n!runner/\n!runner/budget-box.ts\n!teams/\n!teams/**\n' > "$context/.dockerignore"
unset CLOUDFLARE_API_TOKEN CLOUDFLARE_API_KEY
export WRANGLER_SEND_METRICS=false
if [[ ${BETA_NATIVE_LOCAL:-0} != 1 ]]; then
  image=$(docker build -q -f Dockerfile "$context")
  exec node tools/budget-local.mjs "$state" "${BETA_PORT:-18747}" "$image"
fi
exec node_modules/.bin/wrangler dev --local --ip 127.0.0.1 --port "${BETA_PORT:-18747}" \
  --inspector-port "${BETA_INSPECTOR_PORT:-18748}" -c wrangler.jsonc --env-file "$state/beta.env" --persist-to "$state/state"
