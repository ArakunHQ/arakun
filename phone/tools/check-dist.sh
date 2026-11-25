#!/bin/sh
# Fails when dist/ is not what `npm run build` writes now, so a change to the
# page cannot ship without its built copy.
set -eu
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
npx vite build --outDir "$tmp/dist" --emptyOutDir >"$tmp/log" 2>&1 || { cat "$tmp/log" >&2; exit 1; }
if ! diff -r dist "$tmp/dist"; then
  echo "phone/dist is out of date: run 'npm run build' in phone/ and commit it." >&2
  exit 1
fi
