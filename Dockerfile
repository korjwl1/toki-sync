# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM rust:1.92-slim AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static

RUN cargo build --release

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 wget && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd -r toki && useradd -r -g toki toki

COPY --from=builder /build/target/release/toki-sync /usr/local/bin/toki-sync

RUN mkdir -p /data /etc/toki-sync && chown -R toki:toki /data

USER toki

EXPOSE 9090 9091

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:9091/health || exit 1

ENTRYPOINT ["toki-sync"]
