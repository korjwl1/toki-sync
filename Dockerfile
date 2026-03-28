# ── Stage 1: Build ────────────────────────────────────────────────────────────
# Alpine-based rust image uses musl by default → static binary
FROM rust:1.82-alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

ENV RUSTFLAGS="-C target-feature=+crt-static"
RUN cargo build --release

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM alpine:3.20

RUN apk add --no-cache ca-certificates tzdata && \
    addgroup -S toki && adduser -S -G toki toki

WORKDIR /app
COPY --from=builder /app/target/release/toki-sync /app/toki-sync

RUN mkdir -p /app/data /app/config && chown -R toki:toki /app

USER toki

ENV TOKI_SYNC_CONFIG=/app/config/config.toml

EXPOSE 9091 9090

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:9091/health || exit 1

ENTRYPOINT ["/app/toki-sync"]
