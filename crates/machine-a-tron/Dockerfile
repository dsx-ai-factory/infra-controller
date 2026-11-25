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
# NOTE: Cross-compilation architecture
# The BUILDER stage always runs NATIVE on the build host ($BUILDPLATFORM) and
# cross-compiles to x86_64-unknown-linux-gnu. This avoids QEMU emulation when
# building on Apple Silicon hosts, which would otherwise SEGFAULT while
# assembling aws-lc-sys's hand-written x86-64 assembly code.
#
ARG RUST_VERSION=1.100.0

FROM --platform=linux/amd64 debian:bookworm-slim AS freeipmi-builder

ARG FREEIPMI_VERSION=1.6.20
RUN apt-get update && \
	DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
		build-essential ca-certificates curl libgcrypt20-dev && \
	curl -fsSL "https://ftpmirror.gnu.org/freeipmi/freeipmi-${FREEIPMI_VERSION}.tar.gz" \
		-o /tmp/freeipmi.tar.gz && \
	echo "9cc644530ee629ffb94d6fd209e7cb4a8b8ae6677f89d2dc461efed9b2e709f2  /tmp/freeipmi.tar.gz" | sha256sum -c - && \
	cd /tmp && tar xzf freeipmi.tar.gz && \
	cd "freeipmi-${FREEIPMI_VERSION}" && \
	./configure --prefix=/usr --sysconfdir=/etc --quiet && \
	make -j"$(nproc)" --quiet && \
	make install DESTDIR=/tmp/freeipmi-install --quiet && \
	rm -rf /tmp/freeipmi.tar.gz /tmp/freeipmi-${FREEIPMI_VERSION}

FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-slim-bookworm AS builder

ARG VERSION
ARG CI_COMMIT_SHORT_SHA
ENV VERSION=${VERSION}
ENV CI_COMMIT_SHORT_SHA=${CI_COMMIT_SHORT_SHA}

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    pkg-config \
    protobuf-compiler \
    libprotobuf-dev \
    && if [ "$(uname -m)" != "x86_64" ]; then \
         apt-get install -y --no-install-recommends \
           gcc-x86-64-linux-gnu libc6-dev-amd64-cross; \
       else \
         apt-get install -y --no-install-recommends gcc libc6-dev; \
       fi \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-unknown-linux-gnu

ENV CARGO_HOME=/cargo-home
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_TARGET_DIR=/cargo-target
ENV RUST_BACKTRACE=1

WORKDIR /workspace
COPY . .

# Build release binary with cross-compilation
RUN --mount=type=cache,id=nico-mat-cross-cargo-home,target=/cargo-home,sharing=locked \
    --mount=type=cache,id=nico-mat-cross-cargo-target,target=/cargo-target,sharing=locked \
    if [ "$(uname -m)" != "x86_64" ]; then \
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
             CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc; \
    fi && \
    cargo build -p carbide-machine-a-tron --bin machine-a-tron --locked --release \
      --target x86_64-unknown-linux-gnu && \
    mkdir -p /artifacts && \
    cp /cargo-target/x86_64-unknown-linux-gnu/release/machine-a-tron /artifacts/machine-a-tron

# Runtime image
FROM --platform=linux/amd64 debian:bookworm-slim

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

# Replace vulnerable Debian-packaged freeipmi 1.6.10 with source-built 1.6.20.
# Copy all source-built libraries, purge Debian packages so scanners no
# longer report the old version, then re-link with ldconfig.
COPY --from=freeipmi-builder /tmp/freeipmi-install/usr/lib/ /tmp/freeipmi-libs/
RUN dpkg --purge --force-depends libfreeipmi17 freeipmi-common libipmimonitoring6 || true && \
	find /tmp/freeipmi-libs -maxdepth 1 -name '*.so.*' ! -type l \
		-exec cp {} /usr/lib/x86_64-linux-gnu/ \; && \
	rm -rf /tmp/freeipmi-libs && \
	ldconfig

RUN mkdir -p /opt/machine-a-tron/bin /tmp/machine-a-tron-data

COPY --from=builder /artifacts/machine-a-tron /opt/machine-a-tron/bin/machine-a-tron

WORKDIR /opt/machine-a-tron
ENTRYPOINT ["/opt/machine-a-tron/bin/machine-a-tron"]
