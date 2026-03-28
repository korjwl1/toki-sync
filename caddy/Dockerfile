# Build Caddy with caddy-l4 plugin for Layer 4 (TCP) proxy support.
# caddy-l4 enables TLS passthrough on port 9090 → toki-sync TCP server.
FROM caddy:2-builder AS builder

RUN xcaddy build \
    --with github.com/mholt/caddy-l4

FROM caddy:2-alpine
COPY --from=builder /usr/bin/caddy /usr/bin/caddy
