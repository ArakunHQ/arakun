# syntax=docker/dockerfile:1
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0 (see LICENSE in this folder)
# A cloud machine (cloud/src/cloud_machine.ts): `arakun-server` from the latest
# release, the Claude Code and Codex ACP adapters it runs sessions with, and git. The build
# context is the repository; /.dockerignore lets in only what this file copies.
#
# Two users. `arakun` runs the server and holds its sign-in and relay key.
# `agent` runs the agents: the server starts an adapter through
# /opt/arakun/bin/claude-acp or codex-acp, which may only become `agent`, with
# no capabilities, and only to run an adapter or an agent's CLI. Both are in the group `work`,
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

# The adapters at the versions the server pins (CLAUDE_ACP_PACKAGE,
# CODEX_ACP_PACKAGE), with the Claude Code binary and the `codex` they bring,
# each in a folder of its own as the server installs them. Root owns it, so an agent cannot change
# what the next session runs. /opt/arakun/agent/claude is that binary, the one
# of the two (glibc, musl) that runs here: `claude` on the PATH starts it as
# `agent`, for the owner's sign-in.
COPY core/src/agent/launch.rs /tmp/launch.rs
RUN package=$(sed -n 's/^pub const CLAUDE_ACP_PACKAGE: &str = "\(.*\)";$/\1/p' /tmp/launch.rs) \
    && test -n "$package" \
    && npm install --prefix /opt/arakun/agent --include=optional --no-audit --no-fund --loglevel=error "$package" \
    && test -x /opt/arakun/agent/node_modules/.bin/claude-agent-acp \
    && for bin in /opt/arakun/agent/node_modules/@anthropic-ai/claude-agent-sdk-linux-*/claude; do \
         if "$bin" --version >/dev/null 2>&1; then ln -s "$bin" /opt/arakun/agent/claude; break; fi; \
       done \
    && /opt/arakun/agent/claude --version \
    && codex=$(sed -n 's/^pub const CODEX_ACP_PACKAGE: &str = "\(.*\)";$/\1/p' /tmp/launch.rs) \
    && test -n "$codex" \
    && npm install --prefix /opt/arakun/codex --include=optional --no-audit --no-fund --loglevel=error "$codex" \
    && test -x /opt/arakun/codex/node_modules/.bin/codex-acp \
    && /opt/arakun/codex/node_modules/.bin/codex --version \
    && rm /tmp/launch.rs

# machine/run under the four names it answers to: the adapters for the server,
# the CLIs on the PATH for the owner's sign-in.
COPY cloud/machine/entrypoint.sh cloud/machine/run cloud/machine/as-agent cloud/machine/model-env /opt/arakun/bin/
COPY cloud/machine/sudoers /etc/sudoers.d/arakun
RUN chown root:root /opt/arakun/bin/* /etc/sudoers.d/arakun \
    && chmod 755 /opt/arakun/bin/* \
    && ln -s run /opt/arakun/bin/claude-acp \
    && ln -s run /opt/arakun/bin/codex-acp \
    && ln -s /opt/arakun/bin/run /usr/local/bin/claude \
    && ln -s /opt/arakun/bin/run /usr/local/bin/codex \
    && chmod 440 /etc/sudoers.d/arakun \
    && visudo -cf /etc/sudoers.d/arakun

ENTRYPOINT ["/opt/arakun/bin/entrypoint.sh"]
