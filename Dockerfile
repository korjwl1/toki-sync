# ── Stage 1: Build ────────────────────────────────────────────────────────────
# Pin the builder to bookworm: it matches the runtime stage's libssl, and
# trixie's gcc-14 collect2 segfaults randomly under qemu-x86_64 (multi-arch
# builds on Apple Silicon).
FROM rust:1.92-slim-bookworm AS builder

RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static

# Cache mounts keep dependency artifacts across builds, so a flaky failure
# doesn't force a from-scratch recompile. The target cache is per-platform —
# amd64 and arm64 artifacts must not share a directory.
ARG TARGETPLATFORM
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target,id=cargo-target-${TARGETPLATFORM} \
    cargo build --release && cp target/release/toki-sync /build/toki-sync

# ── Stage 2: Runtime ──────────────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 wget && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd -r toki && useradd -r -g toki toki

COPY --from=builder /build/toki-sync /usr/local/bin/toki-sync

RUN mkdir -p /data /etc/toki-sync && chown -R toki:toki /data

USER toki

EXPOSE 9090 9091

HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:9091/health || exit 1

ENTRYPOINT ["toki-sync"]
