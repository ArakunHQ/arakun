# syntax=docker/dockerfile:1
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see LICENSE in this folder)
# A cloud machine (cloud/src/cloud_machine.ts): `arakun-server` from the latest
# release, the Claude Code ACP adapter it runs sessions with, and git. The build
# context is the repository; /.dockerignore lets in only what this file copies.
#
# Two users. `arakun` runs the server and holds its sign-in and relay key.
# `agent` runs the agents: the server starts the adapter through
# /opt/arakun/bin/claude-acp, which may only become `agent`, with no
# capabilities, and only to run the adapter. Both are in the group `work`,
# which owns /work, where sessions start.
#
# The release binary, not a build from source: deploys happen on a release
# (.github/workflows/cloud-deploy.yml), so the latest release is the server of
# this version, and a GPUI build would take the deploy most of an hour.
# tools/install-release.sh downloads it and checks SHA256SUMS against Arakun's
# release key, the way every install does. ARAKUN_VERSION pins another
# release.
FROM node:24-trixie-slim

ARG ARAKUN_VERSION=

ENV DEBIAN_FRONTEND=noninteractive \
    NPM_CONFIG_UPDATE_NOTIFIER=false \
    NPM_CONFIG_FUND=false
# openssl checks the release's signature; sudo and setpriv (util-linux) start
# the agents as their own user.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git openssl ripgrep sudo util-linux procps \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd --system work \
    && useradd --create-home --shell /bin/bash --groups work arakun \
    && useradd --create-home --shell /bin/bash --groups work agent \
    && install -d -o agent -g work -m 2775 /work

# Fails the build unless the signature was checked: the installer only warns
# when it has no way to.
COPY tools/install-release.sh /tmp/install-release.sh
RUN HOME=/tmp/install sh /tmp/install-release.sh --server --no-tools ${ARAKUN_VERSION:+--version "$ARAKUN_VERSION"} | tee /tmp/install.log \
    && grep -q "SHA256SUMS is signed with Arakun's" /tmp/install.log \
    && install -m 755 /tmp/install/.local/bin/arakun-server /usr/local/bin/arakun-server \
    && rm -rf /tmp/install /tmp/install.log /tmp/install-release.sh \
    && arakun-server --version

# The adapter at the version the server pins (CLAUDE_ACP_PACKAGE), with the
# Claude Code binary its SDK brings. Root owns it, so an agent cannot change
# what the next session runs.
COPY core/src/agent/launch.rs /tmp/launch.rs
RUN package=$(sed -n 's/^pub const CLAUDE_ACP_PACKAGE: &str = "\(.*\)";$/\1/p' /tmp/launch.rs) \
    && test -n "$package" \
    && npm install --prefix /opt/arakun/agent --include=optional --no-audit --no-fund --loglevel=error "$package" \
    && test -x /opt/arakun/agent/node_modules/.bin/claude-agent-acp \
    && rm /tmp/launch.rs

COPY cloud/machine/entrypoint.sh cloud/machine/claude-acp cloud/machine/as-agent /opt/arakun/bin/
COPY cloud/machine/sudoers /etc/sudoers.d/arakun
RUN chown root:root /opt/arakun/bin/* /etc/sudoers.d/arakun \
    && chmod 755 /opt/arakun/bin/* \
    && chmod 440 /etc/sudoers.d/arakun \
    && visudo -cf /etc/sudoers.d/arakun

ENTRYPOINT ["/opt/arakun/bin/entrypoint.sh"]
