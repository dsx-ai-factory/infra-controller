# syntax=docker/dockerfile:1.7
#
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
#
# Build from the repository root:
#
#   docker buildx build \
#     --platform linux/amd64 \
#     -f crates/bmc-mock/Dockerfile \
#     --build-arg VERSION=0.1.0 \
#     -t bmc-mock:latest \
#     .

ARG RUST_IMAGE=rust:1.97.1-slim-bookworm@sha256:96c0af8cf054fd006435089f0076729716784ec9be485bd655de59c55df105ce
ARG RUNTIME_IMAGE=debian:bookworm-slim@sha256:7b140f374b289a7c2befc338f42ebe6441b7ea838a042bbd5acbfca6ec875818

FROM --platform=$BUILDPLATFORM ${RUST_IMAGE} AS builder

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
    && case "$(uname -m):${TARGETARCH}" in \
         x86_64:amd64) \
           apt-get install -y --no-install-recommends gcc libc6-dev ;; \
         aarch64:arm64) \
           apt-get install -y --no-install-recommends \
             clang gcc libc6-dev mold ;; \
         aarch64:amd64) \
           apt-get install -y --no-install-recommends \
             gcc-x86-64-linux-gnu libc6-dev-amd64-cross ;; \
         x86_64:arm64) \
           apt-get install -y --no-install-recommends \
             gcc-aarch64-linux-gnu libc6-dev-arm64-cross mold ;; \
         *) echo "unsupported build/target architecture: $(uname -m)/${TARGETARCH}" >&2; exit 1 ;; \
       esac \
    && rm -rf /var/lib/apt/lists/*

RUN case "${TARGETARCH}" in \
      amd64) rustup target add x86_64-unknown-linux-gnu ;; \
      arm64) rustup target add aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac

# Select mold only for ARM64 output. The wrapper keeps the native Clang and
# cross-GCC drivers responsible for their platform-specific defaults.
RUN if [ "${TARGETARCH}" = arm64 ]; then \
      case "$(uname -m)" in \
        aarch64) linker=clang; reader=readelf ;; \
        x86_64) linker=aarch64-linux-gnu-gcc; reader=aarch64-linux-gnu-readelf ;; \
        *) echo "unsupported ARM64 build architecture: $(uname -m)" >&2; exit 1 ;; \
      esac && \
      printf '#!/bin/sh\nexec %s -fuse-ld=mold "$@"\n' "${linker}" \
        >/usr/local/bin/aarch64-mold-linker && \
      chmod 0755 /usr/local/bin/aarch64-mold-linker && \
      printf 'int main(void) { return 0; }\n' | \
        /usr/local/bin/aarch64-mold-linker -x c - -o /tmp/mold-smoke && \
      "${reader}" -p .comment /tmp/mold-smoke | grep -q mold && \
      rm /tmp/mold-smoke; \
    fi

ENV CARGO_HOME=/cargo-home
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_TARGET_DIR=/cargo-target
ENV RUST_BACKTRACE=1

WORKDIR /workspace
COPY . .

RUN --mount=type=cache,id=nico-bmc-mock-cross-cargo-home,target=/cargo-home,sharing=locked \
    --mount=type=cache,id=nico-bmc-mock-cross-cargo-target,target=/cargo-target,sharing=locked \
    case "${TARGETARCH}" in \
      amd64) \
        rust_target=x86_64-unknown-linux-gnu; \
        if [ "$(uname -m)" != x86_64 ]; then \
          export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
                 CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc; \
        fi ;; \
      arm64) \
        rust_target=aarch64-unknown-linux-gnu; \
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=/usr/local/bin/aarch64-mold-linker; \
        if [ "$(uname -m)" != aarch64 ]; then \
          export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc; \
        fi ;; \
      *) echo "unsupported target architecture: ${TARGETARCH}" >&2; exit 1 ;; \
    esac && \
    cargo build -p bmc-mock --bin bmc-mock --locked --release \
      --target "${rust_target}" && \
    mkdir -p /artifacts && \
    cp "/cargo-target/${rust_target}/release/bmc-mock" /artifacts/bmc-mock

FROM --platform=$TARGETPLATFORM ${RUNTIME_IMAGE}

ARG VERSION
ARG CI_COMMIT_SHORT_SHA
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${CI_COMMIT_SHORT_SHA}"
LABEL org.opencontainers.image.title="bmc-mock"
LABEL org.opencontainers.image.description="Redfish BMC simulator with libvirt-backed host control"
LABEL org.opencontainers.image.vendor="NVIDIA"

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libvirt-clients \
    && rm -rf /var/lib/apt/lists/*

RUN mkdir -p /opt/carbide

COPY --from=builder /artifacts/bmc-mock /usr/local/bin/bmc-mock
COPY crates/bmc-mock/tls.crt /opt/carbide/tls.crt
COPY crates/bmc-mock/tls.key /opt/carbide/tls.key

EXPOSE 1266

ENTRYPOINT ["/usr/local/bin/bmc-mock"]
