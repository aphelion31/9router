# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 ninerouter \
 && useradd --system --uid 10001 --gid ninerouter --home-dir /data --shell /usr/sbin/nologin ninerouter \
 && mkdir -p /data \
 && chown ninerouter:ninerouter /data

COPY --from=build /src/target/release/ninerouter /usr/local/bin/ninerouter

ENV DATA_DIR=/data \
    PORT=20127 \
    HOST=0.0.0.0
VOLUME ["/data"]
EXPOSE 20127
USER ninerouter
WORKDIR /data
ENTRYPOINT ["ninerouter"]
CMD ["serve"]
