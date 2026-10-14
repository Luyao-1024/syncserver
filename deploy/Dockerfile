FROM docker.io/library/rust:1.88-bookworm AS build
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin syncserver

FROM docker.io/library/debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 syncserver \
    && useradd --uid 10001 --gid 10001 --no-create-home syncserver \
    && install -d -o 10001 -g 10001 -m 0700 /var/lib/syncserver
COPY --from=build /build/target/release/syncserver /usr/local/bin/syncserver
USER 10001:10001
ENV SYNC_BIND=0.0.0.0:8790 SYNC_DATA_DIR=/var/lib/syncserver
EXPOSE 8790
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/syncserver"]
