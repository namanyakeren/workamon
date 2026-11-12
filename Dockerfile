# syntax=docker/dockerfile:1

FROM rust:1-slim-bookworm AS builder
WORKDIR /build

RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY assets ./assets

RUN cargo build --release --locked

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 workamon \
    && useradd --uid 1000 --gid 1000 --create-home --shell /usr/sbin/nologin workamon \
    && mkdir -p /data \
    && chown workamon:workamon /data

COPY --from=builder /build/target/release/workamon /usr/local/bin/workamon

ENV WORKAMON_ADDR=0.0.0.0:8080 \
    WORKAMON_DB=/data/workamon.db \
    WORKAMON_HOST_ROOT=/host \
    RUST_LOG=workamon=info,tower_http=warn,warn

USER workamon
EXPOSE 8080
VOLUME ["/data"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["curl", "-fsS", "http://127.0.0.1:8080/healthz"]

ENTRYPOINT ["/usr/local/bin/workamon"]
