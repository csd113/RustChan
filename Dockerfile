# syntax=docker/dockerfile:1.7

FROM rust:1.91.0-bookworm AS builder
WORKDIR /build

# BuildKit retains compiled dependencies between builds without carrying Cargo
# caches or the source tree into the final image.
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --locked --release --bin rustchan-cli && \
    install -Dm755 target/release/rustchan-cli /out/rustchan-cli

FROM debian:bookworm-slim AS runtime
LABEL org.opencontainers.image.title="RustChan" \
      org.opencontainers.image.description="Self-hosted imageboard" \
      org.opencontainers.image.source="https://github.com/csd113/RustChan" \
      org.opencontainers.image.licenses="MIT"

# FFmpeg and ffprobe enable RustChan's complete media pipeline. curl provides
# a bounded readiness probe; ca-certificates support outbound TLS and Arti.
RUN apt-get update && apt-get install -y --no-install-recommends \
        ca-certificates curl ffmpeg && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --system --gid 10001 rustchan && \
    useradd --system --uid 10001 --gid rustchan --home-dir /data \
        --no-create-home --shell /usr/sbin/nologin rustchan && \
    mkdir /data && chown rustchan:rustchan /data

COPY --from=builder /out/rustchan-cli /usr/local/bin/rustchan-cli

ENV CHAN_HOST=0.0.0.0 \
    CHAN_PORT=8080 \
    CHAN_TOR_SUPPORT=false

USER rustchan:rustchan
WORKDIR /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD curl --fail --silent --show-error --max-time 4 "http://127.0.0.1:${CHAN_PORT:-8080}/readyz" > /dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/rustchan-cli", "--data-dir", "/data"]
CMD ["serve"]
