# ── Stage 1: build ──────────────────────────────────────────────────────────
FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release && strip target/release/gitea-autoscaler

# ── Stage 2: minimal runtime ───────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/gitea-autoscaler /usr/local/bin/gitea-autoscaler

USER 65534:65534
ENTRYPOINT ["gitea-autoscaler"]
