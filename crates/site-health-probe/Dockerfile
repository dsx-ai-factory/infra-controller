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
#     -f crates/site-health-probe/Dockerfile \
#     --build-arg VERSION=0.1.0 -t site-health-probe:latest .
#
# NOTE: Cross-compilation architecture
# The BUILDER stage always runs NATIVE on the build host ($BUILDPLATFORM) and
# cross-compiles to the TARGETARCH's gnu triple. This avoids QEMU emulation,
# which SEGFAULTS while assembling aws-lc-sys's hand-written assembly code.
#
# Builder pinned by manifest-list digest so a rebuild cannot pick up different
# toolchain content without a source change; the tag documents the version and
# must match rust-toolchain.toml.
ARG BUILDER_BASE=rust:1.97.1-slim-bookworm@sha256:2775a09d208ff0d7c1f50490c45b62db929e87ba1dcbc3f2132ac71a704bcdd3
# One overridable runtime base consumed by every target architecture: a single
# manifest-list digest, so the per-arch base versions cannot drift.
ARG RUNTIME_BASE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

FROM --platform=$BUILDPLATFORM ${BUILDER_BASE} AS builder

ARG TARGETARCH
ARG VERSION
ARG CI_COMMIT_SHORT_SHA
ENV VERSION=${VERSION}
ENV CI_COMMIT_SHORT_SHA=${CI_COMMIT_SHORT_SHA}

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    pkg-config \
    protobuf-compiler \
    libprotobuf-dev \
    && case "$TARGETARCH" in \
         amd64) \
           if [ "$(uname -m)" != "x86_64" ]; then \
             apt-get install -y --no-install-recommends \
               gcc-x86-64-linux-gnu libc6-dev-amd64-cross; \
           else \
             apt-get install -y --no-install-recommends gcc libc6-dev; \
           fi ;; \
         arm64) \
           if [ "$(uname -m)" != "aarch64" ]; then \
             apt-get install -y --no-install-recommends \
               gcc-aarch64-linux-gnu libc6-dev-arm64-cross; \
           else \
             apt-get install -y --no-install-recommends gcc libc6-dev; \
           fi ;; \
         *) echo "unsupported TARGETARCH=$TARGETARCH" >&2; exit 1 ;; \
       esac \
    && rm -rf /var/lib/apt/lists/*

ENV CARGO_HOME=/cargo-home
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_TARGET_DIR=/cargo-target
ENV RUST_BACKTRACE=1

WORKDIR /workspace
COPY . .

# Build the release binary, cross-compiling when builder and target differ.
RUN --mount=type=cache,id=nico-shp-cargo-home-$TARGETARCH,target=/cargo-home,sharing=locked \
    --mount=type=cache,id=nico-shp-cargo-target-$TARGETARCH,target=/cargo-target,sharing=locked \
    case "$TARGETARCH" in \
      amd64) \
        RUST_TARGET=x86_64-unknown-linux-gnu; \
        if [ "$(uname -m)" != "x86_64" ]; then \
          export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
                 CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc; \
        fi ;; \
      arm64) \
        RUST_TARGET=aarch64-unknown-linux-gnu; \
        if [ "$(uname -m)" != "aarch64" ]; then \
          export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
                 CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc; \
        fi ;; \
    esac && \
    rustup target add "$RUST_TARGET" && \
    cargo build -p carbide-site-health-probe --bin site-health-probe --locked --release \
      --target "$RUST_TARGET" && \
    mkdir -p /artifacts && \
    cp "/cargo-target/$RUST_TARGET/release/site-health-probe" /artifacts/site-health-probe

# Runtime image: distroless with glibc and CA certificates (for the REST
# probes' system trust roots), nothing else.
FROM ${RUNTIME_BASE}

ARG VERSION
ARG CI_COMMIT_SHORT_SHA
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${CI_COMMIT_SHORT_SHA}"
LABEL org.opencontainers.image.title="site-health-probe"
LABEL org.opencontainers.image.description="Synthetic monitoring probes against a NICo site's APIs"
LABEL org.opencontainers.image.vendor="NVIDIA"

COPY --from=builder /artifacts/site-health-probe /site-health-probe

# Numeric UID:GID — the chart sets runAsNonRoot, and the kubelet rejects a
# named user there with CreateContainerConfigError.
USER 65532:65532

ENTRYPOINT ["/site-health-probe"]
