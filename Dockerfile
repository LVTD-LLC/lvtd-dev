FROM rust:1.99-bookworm AS chef
RUN cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /app
ENV SQLX_OFFLINE=true

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# Keep compiled dependencies in an ordinary layer, exported by CI's mode=max
# cache. Source edits and revision arguments below do not invalidate this layer.
RUN cargo chef cook --locked --release --package lvtd-dev --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY .sqlx .sqlx
COPY migrations migrations
COPY crates crates
COPY app app
# Git sha of the build; served as the x-deployment-revision header.
ARG BUILD_REVISION=development
ENV BUILD_REVISION=${BUILD_REVISION}
RUN cargo build --locked --release -p lvtd-dev

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home app
WORKDIR /app
COPY --from=builder /app/target/release/lvtd-dev /usr/local/bin/lvtd-dev
# Uploads without a bucket land here; mount a volume or set STORAGE_S3_* in production.
RUN mkdir -p /app/storage && chown app:app /app/storage
ENV BIND_ADDR=0.0.0.0:3000
ENV STORAGE_LOCAL_DIR=/app/storage
USER app
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl --fail --silent http://127.0.0.1:3000/healthz || exit 1
ENTRYPOINT ["lvtd-dev"]
