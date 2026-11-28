# syntax=docker/dockerfile:1.7

# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# Build from the repo root:
#
#   docker buildx build --platform linux/amd64 \
#     -f crates/machine-a-tron/Dockerfile \
#     --build-arg VERSION=0.1.0 -t machine-a-tron:latest .
#
# Use --platform linux/arm64 for an aarch64 image.
#
# NOTE: Cross-compilation architecture
# The BUILDER stage always runs NATIVE on the build host ($BUILDPLATFORM) and
# cross-compiles to the target platform ($TARGETARCH). This avoids QEMU
# emulation when building on a host of the other architecture, which would
# otherwise SEGFAULT while assembling aws-lc-sys's hand-written assembly code.
# The runtime stage runs apt-get for the target platform, so build each
# architecture on a native runner (or a host with binfmt emulation).
#
ARG RUST_VERSION=1.100.0

FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-bookworm AS builder

ARG TARGETARCH
ARG VERSION
ARG CI_COMMIT_SHORT_SHA
ENV VERSION=${VERSION}
ENV CI_COMMIT_SHORT_SHA=${CI_COMMIT_SHORT_SHA}

RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) triple=x86_64-unknown-linux-gnu; cross_pkgs="gcc-x86-64-linux-gnu libc6-dev-amd64-cross" ;; \
      arm64) triple=aarch64-unknown-linux-gnu; cross_pkgs="gcc-aarch64-linux-gnu libc6-dev-arm64-cross" ;; \
      *) echo "unsupported TARGETARCH '$TARGETARCH'" >&2; exit 1 ;; \
    esac; \
    apt-get update; \
    apt-get install -y --no-install-recommends git pkg-config protobuf-compiler libprotobuf-dev gcc libc6-dev; \
    if [ "$(dpkg --print-architecture)" != "$TARGETARCH" ]; then \
      apt-get install -y --no-install-recommends $cross_pkgs; \
    fi; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "$triple"; \
    echo "$triple" > /target-triple

ENV CARGO_HOME=/cargo-home
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_TARGET_DIR=/cargo-target
ENV RUST_BACKTRACE=1

WORKDIR /workspace
COPY . .

# Build release binary with cross-compilation
RUN --mount=type=cache,id=nico-mat-cross-cargo-home,target=/cargo-home,sharing=locked \
    --mount=type=cache,id=nico-mat-cross-cargo-target,target=/cargo-target,sharing=locked \
    set -eu; \
    triple=$(cat /target-triple); \
    if [ "$(dpkg --print-architecture)" != "$TARGETARCH" ]; then \
      gcc="${triple%%-unknown-*}-linux-gnu-gcc"; \
      export "CARGO_TARGET_$(echo "$triple" | tr 'a-z-' 'A-Z_')_LINKER=$gcc" \
             "CC_$(echo "$triple" | tr '-' '_')=$gcc"; \
    fi; \
    cargo build -p carbide-machine-a-tron --bin machine-a-tron --locked --release \
      --target "$triple"; \
    mkdir -p /artifacts; \
    cp "/cargo-target/$triple/release/machine-a-tron" /artifacts/machine-a-tron

# Runtime image
FROM debian:bookworm-slim

ARG VERSION
ARG CI_COMMIT_SHORT_SHA
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${CI_COMMIT_SHORT_SHA}"
LABEL org.opencontainers.image.title="machine-a-tron"
LABEL org.opencontainers.image.description="NICo Hardware Simulation Tool"
LABEL org.opencontainers.image.vendor="NVIDIA"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    ipmitool \
    iproute2 \
    iputils-ping \
    libssl3 \
    libudev1 \
    openipmi \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /opt/machine-a-tron/bin /tmp/machine-a-tron-data

COPY --from=builder /artifacts/machine-a-tron /opt/machine-a-tron/bin/machine-a-tron

WORKDIR /opt/machine-a-tron
ENTRYPOINT ["/opt/machine-a-tron/bin/machine-a-tron"]
