# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=rust:1.98-slim-trixie
ARG DEBIAN_IMAGE=debian:trixie-20261027-slim
ARG NODE_IMAGE=node:24.22.2-trixie-slim

FROM ${NODE_IMAGE} AS ui
WORKDIR /ui
ENV NEXT_TELEMETRY_DISABLED=1
COPY ui/package.json ui/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm npm ci --no-audit --no-fund
COPY ui/ ./
RUN npm run build

FROM ${DEBIAN_IMAGE} AS novnc
WORKDIR /tmp/novnc
RUN apt-get update \
 && apt-get download novnc \
 && dpkg-deb -x novnc_*.deb x \
 && cp -rL x/usr/share/novnc /novnc \
 && rm -rf /var/lib/apt/lists/*

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=chromine-worker-target \
    cargo build --release --locked -p chromine-worker \
 && install -m 0755 target/release/chromine-worker /usr/local/bin/chromine-worker

FROM ${DEBIAN_IMAGE}
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates ffmpeg \
 && rm -rf /var/lib/apt/lists/*

RUN groupadd --gid 10000 chromine \
 && useradd --uid 10002 --gid 10000 --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin worker \
 && install -d -o 10001 -g 10000 -m 2770 /downloads /downloads/incoming /downloads/events /downloads/events/completed \
 && install -d -o 10002 -g 10000 -m 0700 /downloads/jobs \
 && install -d -o 10002 -g 10002 -m 0700 /state \
 && install -d -o 10001 -g 10000 -m 2770 /vnc \
 && install -d -o 10002 -g 10000 -m 2770 /code-socket /code-links

COPY --from=build /usr/local/bin/chromine-worker /usr/local/bin/chromine-worker
COPY --from=ui /ui/out /usr/share/chromine/ui
COPY --from=novnc /novnc /usr/share/novnc

ENV DOWNLOAD_DIR=/downloads \
    STATE_DIR=/state \
    UI_DIR=/usr/share/chromine/ui \
    NOVNC_DIR=/usr/share/novnc \
    VNC_SOCKET=/vnc/vnc.sock \
    WORKER_LISTEN=0.0.0.0:8000

USER 10002:10000
VOLUME ["/downloads", "/state", "/vnc", "/code-socket", "/code-links"]
EXPOSE 8000 8001 8080
ENTRYPOINT ["/usr/local/bin/chromine-worker"]
CMD ["serve"]
