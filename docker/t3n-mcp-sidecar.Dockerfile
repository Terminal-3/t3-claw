FROM node:20-alpine

WORKDIR /app

RUN apk add --no-cache dumb-init

ARG GITHUB_TOKEN

# Configure the GitHub npm registry for the @terminal-3 scope only.
# @terminal-3/t3n-mcp is a private package on npm.pkg.github.com.
# @terminal3/* (no hyphen) dependencies are public packages on npmjs.com — do NOT
# route that scope to GitHub Packages or they will 404.
# GITHUB_TOKEN is required — add it to your .env (GITHUB_TOKEN=ghp_...) before building.
RUN test -n "$GITHUB_TOKEN" || { echo "ERROR: GITHUB_TOKEN is required (read:packages on Terminal-3/trinity). Add it to .env."; exit 1; } && \
    printf "@terminal-3:registry=https://npm.pkg.github.com\n//npm.pkg.github.com/:_authToken=%s\n" "${GITHUB_TOKEN}" > /root/.npmrc

# Install t3n-mcp directly from the GitHub npm registry.
# The published package ships a pre-built dist/ (ESM output + shared binaries)
# so no compile step is needed — npm install is all that's required.
RUN npm install @terminal-3/t3n-mcp && rm -f /root/.npmrc

COPY docker/t3n-mcp-bridge.mjs /bridge/t3n-mcp-bridge.mjs

RUN addgroup -S t3n \
    && adduser -S -G t3n t3n \
    && chown -R t3n:t3n /app /bridge

USER t3n

ENV NODE_ENV=production
ENV LOG_LEVEL=info
# The bridge spawns dist/esm/index.js relative to T3N_PROJECT_DIR.
# Point it at the installed package rather than the build root.
ENV T3N_PROJECT_DIR=/app/node_modules/@terminal-3/t3n-mcp
ENV MCP_SOCKET_PATH=/var/run/t3n-mcp/t3n-mcp.sock

ENTRYPOINT ["dumb-init", "--"]
CMD ["node", "/bridge/t3n-mcp-bridge.mjs"]
