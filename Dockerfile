FROM rust:1.94.0-bookworm@sha256:365468470075493dc4583f47387001854321c5a8583ea9604b297e67f01c5a4f AS build

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY crates crates

RUN cargo build --release --locked -p powder-server -p powder-cli -p powder-mcp

FROM debian:bookworm-slim

RUN apt-get update -y && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/*

COPY --from=litestream/litestream@sha256:5572700ba18710cb010a0e415e36abf5cc0b4d74a2ad7b6d6a387142c0c99604 /usr/local/bin/litestream /usr/local/bin/litestream

WORKDIR /app

RUN useradd --create-home app && \
    mkdir -p /app/bin /data && \
    chown -R app:app /app /data

COPY --from=build --chown=app:app /app/target/release/powder-server /app/bin/powder-server
COPY --from=build --chown=app:app /app/target/release/powder /app/bin/powder
COPY --from=build --chown=app:app /app/target/release/powder-mcp /app/bin/powder-mcp
COPY --chown=app:app litestream.yml /etc/litestream.yml
COPY --chown=app:app bin/entrypoint.sh /app/bin/entrypoint.sh
RUN chmod +x /app/bin/entrypoint.sh /app/bin/powder-server /app/bin/powder /app/bin/powder-mcp

USER app

ENV POWDER_DB_PATH=/data/powder.db
ENV PORT=4000

CMD ["/app/bin/entrypoint.sh"]
