# Builds the Spike relay from source. Build context is the repository root (see docker-compose.yml).
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p spike-relay

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin spike \
    && mkdir /data && chown spike:spike /data
COPY --from=build /src/target/release/spike-relay /usr/local/bin/spike-relay
USER spike
VOLUME /data
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/spike-relay"]
