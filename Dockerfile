# syntax=docker/dockerfile:1
#
# Haloid in one image: the frontend built and compiled into the server
# binary, on a slim Debian with the media tools the pipeline runs (vips,
# ffmpeg, ffprobe). See docs/deploy.md.

# 1. The frontend.
FROM node:24-trixie AS web
WORKDIR /src/frontend
RUN corepack enable
COPY frontend/package.json frontend/pnpm-lock.yaml frontend/pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY frontend/ ./
RUN pnpm build

# 2. The server, with the frontend inside it.
FROM rust:1-trixie AS server
WORKDIR /src
COPY . .
COPY --from=web /src/frontend/build frontend/build
RUN cargo build --release --locked -p haloid-server --features embed-frontend \
    && strip target/release/haloid-server

# 3. What runs.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates libvips-tools ffmpeg \
        libheif-plugin-libde265 libheif-plugin-dav1d libheif-plugin-x265 libheif-plugin-aomenc \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /data --shell /usr/sbin/nologin haloid \
    && mkdir -p /data && chown haloid /data
COPY --from=server /src/target/release/haloid-server /usr/local/bin/haloid-server
USER haloid
WORKDIR /data
ENV HALOID_BIND=0.0.0.0:8080 \
    HALOID_IN_CONTAINER=1 \
    HALOID_DATA_DIR=/data \
    HALOID_LOG=info
VOLUME /data
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD ["/bin/bash", "-c", "exec 3<>/dev/tcp/127.0.0.1/8080 && printf 'GET /api/v2/healthz HTTP/1.0\\r\\n\\r\\n' >&3 && grep -q '\"ok\"' <&3"]
CMD ["haloid-server"]
