FROM rust:1-slim-bookworm AS builder
WORKDIR /src
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY static ./static
RUN cargo build --release --locked || cargo build --release

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd -m -u 1000 app \
    && mkdir -p /app/data /app/static /app/samples \
    && chown -R app:app /app
WORKDIR /app
COPY --from=builder /src/target/release/ravito_gpx /usr/local/bin/ravito_gpx
COPY static /app/static
USER app
ENV PORT=3000
ENV DB_PATH=/app/data/ravito.db
ENV SAMPLE_GPX_DIR=/app/samples
ENV OVERPASS_URL=https://overpass-api.de/api/interpreter
ENV RUST_LOG=info,ravito_gpx=info
EXPOSE 3000
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -qO- "http://127.0.0.1:${PORT}/" >/dev/null || exit 1
CMD ["ravito_gpx"]
