# syntax=docker/dockerfile:1.7

ARG CODE_SERVER_IMAGE=codercom/code-server:4.141.1
FROM ${CODE_SERVER_IMAGE}

USER root
COPY docker/code/chromine-open docker/code/chromine-code-start /usr/bin/
COPY docker/code/apt-remember.conf /etc/apt/apt.conf.d/99chromine-remember
RUN set -eu; \
    chmod 0755 /usr/bin/chromine-open /usr/bin/chromine-code-start; \
    test -x /usr/lib/code-server/lib/node; \
    ln -sf /usr/bin/chromine-open /usr/local/bin/xdg-open; \
    helpers="$(find /usr/lib/code-server -path '*/bin/helpers/browser.sh')"; \
    test -n "$helpers"; \
    for h in $helpers; do ln -sf /usr/bin/chromine-open "$h"; done; \
    mkdir -p /etc/chromine; \
    apt-mark showmanual > /etc/chromine/base-packages; \
    groupadd --gid 10000 chromine; \
    install -d -o 0 -g 10000 -m 2770 /code-socket /code-links; \
    install -d -m 0755 /root/workspace

ENV HOME=/root \
    BROWSER=/usr/bin/chromine-open
WORKDIR /root/workspace
VOLUME ["/root", "/usr/local", "/code-socket", "/code-links"]
ENTRYPOINT ["/usr/bin/chromine-code-start"]
CMD ["--auth", "none", "--socket", "/code-socket/code.sock", "--socket-mode", "660", "--disable-workspace-trust", \
     "--disable-telemetry", "--disable-update-check", "/root/workspace"]
