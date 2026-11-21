FROM oven/bun:1-slim@sha256:b780593f3e10918cbdaec3d5a08d52224b9198069a65eb47812085c2ec007643 AS bunbin
FROM node:22-bookworm-slim@sha256:e9f565d32859f0cbfb52125eafe8be39390f24dac302dc3cdf566e8ba3ab21ee
COPY --from=bunbin /usr/local/bin/bun /usr/local/bin/bun
WORKDIR /app/teams
COPY teams/package.json teams/package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
# workerd initializes its TLS store even for offline tests.
RUN mkdir -p /etc/ssl/certs && node -e 'require("fs").writeFileSync("/etc/ssl/certs/ca-certificates.crt",require("tls").rootCertificates.join("\n"))'
COPY --chown=node:node teams/ ./
WORKDIR /app
COPY --chown=node:node runner/budget-box.ts ./runner/
ENV CI=1 WRANGLER_SEND_METRICS=false HOME=/tmp
USER node
ENTRYPOINT ["bun", "runner/budget-box.ts"]
