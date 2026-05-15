FROM rust:1.86-slim AS builder

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock ./
COPY proxy_app/Cargo.toml ./proxy_app/Cargo.toml
COPY crates/config/Cargo.toml ./crates/config/Cargo.toml
COPY crates/models/Cargo.toml ./crates/models/Cargo.toml
COPY crates/rotator/Cargo.toml ./crates/rotator/Cargo.toml

RUN mkdir -p proxy_app/src crates/config/src crates/models/src crates/rotator/src \
    && printf 'fn main() {}\n' > proxy_app/src/main.rs \
    && printf 'pub fn build_app() -> axum::Router { axum::Router::new() }\n' > proxy_app/src/lib.rs \
    && printf '' > crates/config/src/lib.rs \
    && printf '' > crates/models/src/lib.rs \
    && printf '' > crates/rotator/src/lib.rs \
    && cargo build --release -p proxy_app \
    && rm -rf proxy_app/src crates/config/src crates/models/src crates/rotator/src

COPY proxy_app ./proxy_app
COPY crates ./crates

RUN cargo build --release -p proxy_app

FROM debian:bookworm-slim AS runtime

WORKDIR /app

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && mkdir -p /app/data

COPY --from=builder /app/target/release/proxy_app /proxy_app

EXPOSE 8000

HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8000/ || exit 1

ENTRYPOINT ["/proxy_app"]
