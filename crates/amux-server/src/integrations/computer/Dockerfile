# amux computer-use sandbox (AMUX-5300), built locally on first `amux computer start`.
#
# Base: trycua/cua-ubuntu, CUA's Kasm Ubuntu desktop. It runs `computer-server`
# (the HTTP /cmd API amux drives) on :8000 and noVNC on :6901, and it is the one
# CUA desktop image published for linux/arm64 WITH computer-server inside
# (trycua/cua-xfce is amd64-only at 7.4 GB; trycua/xfce-cua ships cua-driver, an
# MCP-over-stdio runtime, without the HTTP API).
FROM trycua/cua-ubuntu:latest

USER root

# Chromium, because the ladder's rung 3 exists for sites a worker could not
# reach through rungs 1-2, and those are Chrome sites. Google Chrome has no
# linux/arm64 build and Ubuntu's chromium is a snap (no snapd in a container),
# so this is Playwright's Chromium build, which ships for both arches.
ENV PLAYWRIGHT_BROWSERS_PATH=/opt/pw-browsers
RUN python3.12 -m playwright install --with-deps chromium \
 && CH="$(ls -d /opt/pw-browsers/chromium-*/chrome-linux*/chrome | head -1)" \
 && test -x "$CH" \
 && ln -sf "$CH" /usr/local/bin/chromium \
 && chmod -R a+rX /opt/pw-browsers \
 && rm -rf /root/.cache /var/lib/apt/lists/*

# Upstream's startup runs `pip install --upgrade cua-computer-server cua-agent`
# on EVERY boot: a network dependency and a minute of CPU per sandbox start, on
# a box that is often CPU-pressured. Start the server that is already installed.
RUN printf '#!/bin/bash\nexec /usr/bin/python3 -m computer_server\n' > /dockerstartup/custom_startup.sh \
 && chmod +x /dockerstartup/custom_startup.sh

LABEL amux-computer.image="1"
USER 1000
