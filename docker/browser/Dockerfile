# syntax=docker/dockerfile:1.7

ARG RUST_IMAGE=rust:1.98-slim-trixie
ARG DEBIAN_IMAGE=debian:trixie-20261027-slim

FROM ${RUST_IMAGE} AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target,id=chromine-agent-target \
    cargo build --release --locked -p chromine-agent \
 && install -m 0755 target/release/chromine-agent /usr/local/bin/chromine-agent

FROM ${DEBIAN_IMAGE} AS extension
RUN apt-get update \
 && apt-get install -y --no-install-recommends python3 openssl \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /ext
COPY docker/browser/download-monitor ./monitor
COPY docker/browser/video-downloader ./video
COPY docker/browser/pack-crx.py ./
RUN set -eu; \
    mkdir -p out/extensions out/native; \
    pack() { \
      openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out key.pem 2>/dev/null; \
      id="$(python3 -I pack-crx.py "$1" key.pem "out/$2.crx")"; \
      rm key.pem; \
      version="$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["version"])' "$1/manifest.json")"; \
      printf '{"external_crx": "/opt/chromine/%s.crx", "external_version": "%s"}\n' "$2" "$version" > "out/extensions/$id.json"; \
      printf '{"name": "%s", "description": "%s", "path": "%s", "type": "stdio", "allowed_origins": ["chrome-extension://%s/"]}\n' \
        "$3" "$4" "$5" "$id" > "out/native/$3.json"; \
    }; \
    pack monitor chromine-download-monitor com.chromine.agent "Chromine download monitor" /usr/local/bin/chromine-agent; \
    pack video chromine-video-downloader com.chromine.video "Chromine video downloader" /usr/local/bin/chromine-video-host

FROM ${DEBIAN_IMAGE}

ARG GOOGLE_SIGNING_KEY_FPR=EB4C1BFD4F042F6DDDCCEC917721F63BD38B4796
ARG CHROME_PACKAGE=google-chrome-stable
ARG YTDLP_SIGNING_KEY_FPR=AC0CBBE6848D6A873464AF4E57CF65933B5A7581
ARG YTDLP_VERSION=latest
ARG DENO_VERSION=latest

SHELL ["/bin/bash", "-o", "pipefail", "-c"]
ENV DEBIAN_FRONTEND=noninteractive

RUN arch="$(dpkg --print-architecture)"; \
    if [ "$arch" != "amd64" ]; then \
      echo "chromine's browser image supports linux/amd64 only (building for $arch)" >&2; exit 1; \
    fi

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl gnupg gpgv unzip \
 && curl -fsSL https://dl.google.com/linux/linux_signing_key.pub -o /tmp/google.pub \
 && fpr="$(gpg --show-keys --with-colons /tmp/google.pub | awk -F: '/^fpr:/ {print $10; exit}')" \
 && if [ "$fpr" != "$GOOGLE_SIGNING_KEY_FPR" ]; then echo "unexpected Google signing key: $fpr" >&2; exit 1; fi \
 && gpg --dearmor < /tmp/google.pub > /usr/share/keyrings/google-linux-signing.gpg \
 && echo "deb [arch=amd64 signed-by=/usr/share/keyrings/google-linux-signing.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
      > /etc/apt/sources.list.d/google-chrome.list \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
      "${CHROME_PACKAGE}" \
      tigervnc-standalone-server \
      openbox \
      pulseaudio \
      pulseaudio-utils \
      ffmpeg \
      fonts-liberation \
      fonts-dejavu-core \
      fonts-noto-color-emoji \
      util-linux \
 && mkdir /tmp/ytdlp && cd /tmp/ytdlp \
 && if [ "$YTDLP_VERSION" = latest ]; then base=https://github.com/yt-dlp/yt-dlp/releases/latest/download; \
    else base="https://github.com/yt-dlp/yt-dlp/releases/download/$YTDLP_VERSION"; fi \
 && curl -fsSL -o yt-dlp_linux.zip "$base/yt-dlp_linux.zip" \
 && curl -fsSL -o SHA2-256SUMS "$base/SHA2-256SUMS" \
 && curl -fsSL -o SHA2-256SUMS.sig "$base/SHA2-256SUMS.sig" \
 && curl -fsSL -o public.key https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/public.key \
 && fpr="$(gpg --show-keys --with-colons public.key | awk -F: '/^fpr:/ {print $10; exit}')" \
 && if [ "$fpr" != "$YTDLP_SIGNING_KEY_FPR" ]; then echo "unexpected yt-dlp signing key: $fpr" >&2; exit 1; fi \
 && gpg --dearmor < public.key > ytdlp.gpg \
 && gpgv --keyring ./ytdlp.gpg SHA2-256SUMS.sig SHA2-256SUMS \
 && expected="$(awk '$2 == "yt-dlp_linux.zip" {print $1}' SHA2-256SUMS)" \
 && echo "$expected  yt-dlp_linux.zip" | sha256sum -c - \
 && unzip -q yt-dlp_linux.zip -d /opt/yt-dlp \
 && chmod -R u=rwX,go=rX /opt/yt-dlp \
 && ln -s /opt/yt-dlp/yt-dlp_linux /usr/local/bin/yt-dlp \
 && if [ "$DENO_VERSION" = latest ]; then base=https://github.com/denoland/deno/releases/latest/download; \
    else base="https://github.com/denoland/deno/releases/download/$DENO_VERSION"; fi \
 && curl -fsSL -o deno.zip "$base/deno-x86_64-unknown-linux-gnu.zip" \
 && curl -fsSL -o deno.zip.sha256sum "$base/deno-x86_64-unknown-linux-gnu.zip.sha256sum" \
 && expected="$(grep -oiE '[0-9a-f]{64}' deno.zip.sha256sum | head -1)" \
 && echo "$expected  deno.zip" | sha256sum -c - \
 && unzip -q deno.zip deno -d /usr/local/bin \
 && chmod 0755 /usr/local/bin/deno \
 && cd / && rm -rf /tmp/ytdlp \
 && apt-get purge -y --auto-remove curl gnupg unzip \
 && rm -rf /var/lib/apt/lists/* /tmp/google.pub \
 && google-chrome-stable --version > /etc/chrome-version \
 && HOME=/tmp /usr/local/bin/yt-dlp --version > /etc/yt-dlp-version \
 && HOME=/tmp DENO_DIR=/tmp/deno /usr/local/bin/deno --version | head -1 > /etc/deno-version \
 && rm -rf /tmp/deno \
 && chmod 0755 /opt/google/chrome/chrome-sandbox

COPY docker/browser/openbox/menu.xml /etc/xdg/openbox/menu.xml

COPY docker/browser/extensions/ /opt/google/chrome/extensions/
COPY --from=extension /ext/out/chromine-download-monitor.crx /opt/chromine/chromine-download-monitor.crx
COPY --from=extension /ext/out/chromine-video-downloader.crx /opt/chromine/chromine-video-downloader.crx
COPY --from=extension /ext/out/extensions/ /opt/google/chrome/extensions/
COPY --from=extension /ext/out/native/com.chromine.agent.json /etc/opt/chrome/native-messaging-hosts/com.chromine.agent.json
COPY --from=extension /ext/out/native/com.chromine.video.json /etc/opt/chrome/native-messaging-hosts/com.chromine.video.json

RUN groupadd --gid 10000 chromine \
 && useradd --uid 10001 --gid 10000 --no-create-home --home-dir /tmp/chrome-home --shell /usr/sbin/nologin chrome \
 && install -d -o 10001 -g 10000 -m 2770 /downloads /downloads/incoming /downloads/events /downloads/events/completed /downloads/video \
 && install -d -o 10002 -g 10000 -m 0700 /downloads/jobs \
 && install -d -o 10001 -g 10000 -m 0700 /profile /users \
 && install -d -o 10001 -g 10000 -m 2770 /vnc /code-links

COPY --from=build /usr/local/bin/chromine-agent /usr/local/bin/chromine-agent
RUN ln -s chromine-agent /usr/local/bin/chromine-video-host

ENV DOWNLOAD_DIR=/downloads \
    CHROME_PROFILE_DIR=/profile \
    CHROME_BIN=/usr/bin/google-chrome-stable \
    HOME=/tmp/chrome-home \
    XDG_RUNTIME_DIR=/tmp/chrome-runtime \
    VNC_SOCKET=/vnc/vnc.sock \
    USERS_DIR=/users \
    VIDEO_DIR=/tmp/chromine-video \
    YTDLP_BIN=/usr/local/bin/yt-dlp \
    LANG=C.UTF-8

USER 10001:10000
VOLUME ["/profile", "/users", "/downloads", "/vnc", "/code-links"]
ENTRYPOINT ["/usr/local/bin/chromine-agent"]
CMD ["run"]
