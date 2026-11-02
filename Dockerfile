# syntax=docker/dockerfile:1

FROM rust:1.99-bookworm AS builder
WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY web ./web
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends ca-certificates tzdata gosu \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1000 tagnest \
    && useradd --uid 1000 --gid 1000 --create-home --shell /usr/sbin/nologin tagnest

COPY --from=builder /src/target/release/tagnest /usr/local/bin/tagnest
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod 0755 /usr/local/bin/tagnest /usr/local/bin/entrypoint.sh

ENV TAGNEST_DATA_DIR=/data \
    TAGNEST_PORT=8080 \
    TAGNEST_BIND=0.0.0.0 \
    TZ=Asia/Shanghai

VOLUME ["/data", "/music"]
EXPOSE 8080
STOPSIGNAL SIGTERM

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD ["/bin/sh", "-c", "/usr/local/bin/tagnest --healthcheck --port ${TAGNEST_PORT:-8080}"]

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["--data", "/data"]
