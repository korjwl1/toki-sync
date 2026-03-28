# ── Stage 1: Build ────────────────────────────────────────────────────────────
# Alpine-based rust image uses musl by default → static binary
FROM rust:1.82-alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev openssl-libs-static

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src

ENV RUSTFLAGS="-C target-feature=+crt-static"
RUN cargo build --release

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM alpine:3.20

RUN apk add --no-cache ca-certificates tzdata && \
    addgroup -S toki && adduser -S -G toki toki

COPY --from=builder /build/target/release/toki-sync /usr/local/bin/toki-sync

RUN mkdir -p /data /etc/toki-sync && chown -R toki:toki /data

USER toki

EXPOSE 9090 9091

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:9091/health || exit 1

ENTRYPOINT ["toki-sync"]
