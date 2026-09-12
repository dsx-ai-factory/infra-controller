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
#   docker buildx build --platform linux/amd64,linux/arm64 \
#     -f crates/machine-a-tron/Dockerfile \
#     --build-arg VERSION=0.1.0 -t machine-a-tron:latest .
#
# NOTE: Cross-compilation architecture
# The builder always runs natively on $BUILDPLATFORM and compiles for
# $TARGETARCH. This avoids QEMU during compilation while allowing the same
# Dockerfile to publish linux/amd64 and linux/arm64 images.
#
ARG RUST_VERSION=1.97.1

FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-bookworm AS builder

ARG VERSION
ARG CI_COMMIT_SHORT_SHA
ARG TARGETARCH
ENV VERSION=${VERSION}
ENV CI_COMMIT_SHORT_SHA=${CI_COMMIT_SHORT_SHA}

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    pkg-config \
    protobuf-compiler \
    libprotobuf-dev \
    && case "${TARGETARCH}:$(uname -m)" in \
         amd64:x86_64|arm64:aarch64) \
           apt-get install -y --no-install-recommends gcc libc6-dev ;; \
         amd64:*) \
           apt-get install -y --no-install-recommends gcc-x86-64-linux-gnu libc6-dev-amd64-cross ;; \
         arm64:*) \
           apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross ;; \
         *) \
           echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
       esac \
    && rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
      amd64) rustup target add x86_64-unknown-linux-gnu ;; \
      arm64) rustup target add aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

ENV CARGO_HOME=/cargo-home
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_TARGET_DIR=/cargo-target
ENV RUST_BACKTRACE=1

WORKDIR /workspace
COPY . .

# Build the release binary natively or with cross-compilation as needed.
RUN --mount=type=cache,id=nico-mat-cross-cargo-home,target=/cargo-home,sharing=locked \
    --mount=type=cache,id=nico-mat-cross-cargo-target,target=/cargo-target,sharing=locked \
    case "${TARGETARCH}" in \
      amd64) \
        rust_target=x86_64-unknown-linux-gnu; \
        if [ "$(uname -m)" != "x86_64" ]; then \
          export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
                 CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc; \
        fi ;; \
      arm64) \
        rust_target=aarch64-unknown-linux-gnu; \
        if [ "$(uname -m)" != "aarch64" ]; then \
          export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
                 CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc; \
        fi ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac && \
    cargo build -p carbide-machine-a-tron --bin machine-a-tron --locked --release \
      --target "${rust_target}" && \
    mkdir -p /artifacts && \
    cp "/cargo-target/${rust_target}/release/machine-a-tron" /artifacts/machine-a-tron

# Runtime image
FROM --platform=$TARGETPLATFORM debian:bookworm-slim

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
