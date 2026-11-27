#!/bin/sh
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see LICENSE in this folder)
# The cloud machine's first process, as root. The machine's object
# (cloud/src/cloud_machine.ts) starts the container with:
#
#   ARAKUN_CLOUD_NAME            the machine's name on the account
#   ARAKUN_CLOUD_BASE            the account host, such as https://app.arakun.dev
#   ARAKUN_CLOUD_SESSION         a `cloud` session, good only at this label's door
#   ARAKUN_CLOUD_SESSION_ID      its public id
#   ARAKUN_CLOUD_GITHUB_ID       the owner
#   ARAKUN_CLOUD_LOGIN           the owner's login
#   ARAKUN_CLOUD_IDENTITY        the relay key (PKCS#8, base64url), on a start
#   ARAKUN_CLOUD_IDENTITY_PUBLIC from the image only, with its public half
#
# It writes the sign-in where the server reads it (account.json), the relay key
# into phone.json on the first start, and starts `arakun-server serve` as
# `arakun` with none of these variables. A new session comes with every start;
# the object removes account.json before it snapshots the disk.
set -eu
umask 077

home=/home/arakun
dir=$home/.local/share/arakun/server

fail() {
  echo "cloud machine: $*" >&2
  exit 1
}

# Each value goes into JSON as it is, so each must have the shape it should.
matches() {
  printf '%s\n' "$1" | grep -Eqx "$2"
}
matches "${ARAKUN_CLOUD_BASE:-}" 'https?://[A-Za-z0-9.-]+(:[0-9]+)?' || fail "ARAKUN_CLOUD_BASE is not an origin"
matches "${ARAKUN_CLOUD_SESSION:-}" '[A-Za-z0-9_-]{43}' || fail "ARAKUN_CLOUD_SESSION is missing"
matches "${ARAKUN_CLOUD_SESSION_ID:-}" '[0-9a-f]{32}' || fail "ARAKUN_CLOUD_SESSION_ID is missing"
matches "${ARAKUN_CLOUD_GITHUB_ID:-}" '[1-9][0-9]{0,15}' || fail "ARAKUN_CLOUD_GITHUB_ID is missing"
matches "${ARAKUN_CLOUD_LOGIN:-}" '[A-Za-z0-9_-]{1,39}' || fail "ARAKUN_CLOUD_LOGIN is missing"
[ -n "${ARAKUN_CLOUD_NAME:-}" ] || fail "ARAKUN_CLOUD_NAME is missing"

# Writes stdin to $1 for `arakun` alone, in one rename.
write_private() {
  cat >"$1.new"
  chown arakun:arakun "$1.new"
  chmod 600 "$1.new"
  mv -f "$1.new" "$1"
}

for d in "$home/.local" "$home/.local/share" "$home/.local/share/arakun" "$dir"; do
  install -d -o arakun -g arakun -m 700 "$d"
done

# The first start from the image: the server takes the key from here into its
# own store and rewrites the file without it.
if [ ! -e "$dir/phone.json" ]; then
  matches "${ARAKUN_CLOUD_IDENTITY:-}" '[A-Za-z0-9_-]+' || fail "no phone.json and no ARAKUN_CLOUD_IDENTITY"
  matches "${ARAKUN_CLOUD_IDENTITY_PUBLIC:-}" '[A-Za-z0-9_-]+' || fail "ARAKUN_CLOUD_IDENTITY_PUBLIC is missing"
  write_private "$dir/phone.json" <<EOF
{"enabled":true,"port":7474,"reach":"relay","identity":"$ARAKUN_CLOUD_IDENTITY","identity_public":"$ARAKUN_CLOUD_IDENTITY_PUBLIC","relay_base":"$ARAKUN_CLOUD_BASE","devices":[]}
EOF
fi

write_private "$dir/account.json" <<EOF
{"base":"$ARAKUN_CLOUD_BASE","session":"$ARAKUN_CLOUD_SESSION","session_id":"$ARAKUN_CLOUD_SESSION_ID","github_id":$ARAKUN_CLOUD_GITHUB_ID,"login":"$ARAKUN_CLOUD_LOGIN"}
EOF

base=$ARAKUN_CLOUD_BASE
name=$ARAKUN_CLOUD_NAME
unset ARAKUN_CLOUD_SESSION ARAKUN_CLOUD_SESSION_ID ARAKUN_CLOUD_IDENTITY ARAKUN_CLOUD_IDENTITY_PUBLIC

# As `arakun`, with an environment of its own: nothing of this one, the
# session above least of all. HOSTNAME is the name the relay shows.
as_arakun() {
  setpriv --reuid=arakun --regid=arakun --init-groups -- env -i \
    PATH=/usr/local/bin:/usr/bin:/bin HOME="$home" USER=arakun LOGNAME=arakun LANG=C.UTF-8 \
    HOSTNAME="$name" ARAKUN_TEAMS_URL="$base" ARAKUN_SERVER_DIR="$dir" \
    ARAKUN_CLAUDE_ACP=/opt/arakun/bin/claude-acp \
    sh -c 'umask 002 && exec "$@"' sh "$@"
}

as_arakun arakun-server serve --reach relay --relay "$base" --service &
server=$!
trap 'kill -TERM "$server" 2>/dev/null || true' TERM INT

# Sessions may start in /work. Once per disk: the server keeps the list.
if [ ! -e "$dir/.work-added" ]; then
  for _ in $(seq 60); do
    [ -S "$dir/server.sock" ] && break
    sleep 1
  done
  if as_arakun arakun-server add --agent claude /work; then
    touch "$dir/.work-added"
  fi
fi

# `wait` returns early when the trap runs; wait until the server is gone.
status=0
wait "$server" || status=$?
while kill -0 "$server" 2>/dev/null; do
  status=0
  wait "$server" || status=$?
done
exit "$status"
