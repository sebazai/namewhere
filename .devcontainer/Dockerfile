# Pre-built Rust toolchain (avoids long rustup/apt bootstrap on slim images).
FROM mcr.microsoft.com/devcontainers/rust:1-1-bookworm

RUN apt-get update && export DEBIAN_FRONTEND=noninteractive \
    && apt-get install -y --no-install-recommends \
        ffmpeg \
        xdg-utils \
    && apt-get clean && rm -rf /var/lib/apt/lists/*
