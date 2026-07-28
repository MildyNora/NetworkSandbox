FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        iproute2 \
        nftables \
        python3 \
        util-linux \
    && rm -rf /var/lib/apt/lists/*

COPY linux-host-scenario.sh /usr/local/bin/linux-host-scenario
RUN chmod 755 /usr/local/bin/linux-host-scenario

CMD ["sleep", "infinity"]
