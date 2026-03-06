# ── Stage 1: build ──────────────────────────────────────────────────────────
FROM rust:1-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ src/

RUN cargo build --release && strip target/release/gitea-autoscaler

# ── Stage 2: minimal runtime ───────────────────────────────────────────────
FROM debian:bookworm-slim

# Reuse the CA trust store from the builder image so the runtime stage does not
# depend on a separate apt transaction during image builds.
COPY --from=builder /etc/ssl/certs /etc/ssl/certs
COPY --from=builder /usr/share/ca-certificates /usr/share/ca-certificates

COPY --from=builder /app/target/release/gitea-autoscaler /usr/local/bin/gitea-autoscaler

USER 65534:65534
ENTRYPOINT ["gitea-autoscaler"]
