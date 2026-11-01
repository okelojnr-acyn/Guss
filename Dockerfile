# Builds the `guss` binary without the `local-whisper` feature by default
# (text + TTS voice replies work fully either way). Pass
# --build-arg FEATURES=local-whisper to also build local Whisper
# speech-to-text - cmake/clang are installed either way so that path
# works out of the box, at the cost of a larger/slower image when unused.
FROM rust:1-slim-bookworm AS builder
WORKDIR /build

ARG FEATURES=""

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev ca-certificates cmake clang libclang-dev \
    && rm -rf /var/lib/apt/lists/*

COPY . .
RUN if [ -n "$FEATURES" ]; then \
      cargo build --release -p guss --features "$FEATURES"; \
    else \
      cargo build --release -p guss; \
    fi

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/guss /usr/local/bin/guss

VOLUME ["/app/data"]
ENV GUSS_STORE__URL=sqlite:///app/data/guss.db

EXPOSE 8080
ENTRYPOINT ["guss"]
