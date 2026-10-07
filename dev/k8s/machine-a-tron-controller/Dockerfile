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

# mat-k8s-controller image: the Go controller plus mat-protocol-gateway for the
# controller pod's gateway container. The build context is the repository root.

ARG GO_IMAGE=golang:1.26.4-alpine@sha256:3ad57304ad93bbec8548a0437ad9e06a455660655d9af011d58b993f6f615648
ARG RUST_IMAGE=rust:1.97.1-slim-bookworm@sha256:2775a09d208ff0d7c1f50490c45b62db929e87ba1dcbc3f2132ac71a704bcdd3
ARG RUNTIME_IMAGE=gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

# Go controller build stage
FROM --platform=$BUILDPLATFORM ${GO_IMAGE} AS controller-builder

ARG TARGETARCH

WORKDIR /src

COPY dev/k8s/machine-a-tron-controller/go.mod dev/k8s/machine-a-tron-controller/go.sum ./
RUN go mod download

COPY dev/k8s/machine-a-tron-controller/ ./
RUN CGO_ENABLED=0 GOOS=linux GOARCH=${TARGETARCH} \
    go build -ldflags="-s -w" -o /out/mat-k8s-controller ./cmd/mat-k8s-controller

# Rust gateway build stage
FROM --platform=$BUILDPLATFORM ${RUST_IMAGE} AS gateway-builder

ARG TARGETARCH
ARG VERSION
ARG CI_COMMIT_SHORT_SHA
ENV VERSION=${VERSION}
ENV CI_COMMIT_SHORT_SHA=${CI_COMMIT_SHORT_SHA}

WORKDIR /workspace
COPY rust-toolchain.toml ./

# Select the Rust target triple and linker for TARGETARCH. protoc runs on the
# build host for the librms build script, whatever the target.
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) triple=x86_64-unknown-linux-gnu; cross="gcc-x86-64-linux-gnu libc6-dev-amd64-cross"; host=x86_64 ;; \
      arm64) triple=aarch64-unknown-linux-gnu; cross="gcc-aarch64-linux-gnu libc6-dev-arm64-cross"; host=aarch64 ;; \
      *) echo "unsupported TARGETARCH: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    apt-get update; \
    apt-get install -y --no-install-recommends git pkg-config protobuf-compiler libprotobuf-dev; \
    if [ "$(uname -m)" != "${host}" ]; then \
      apt-get install -y --no-install-recommends ${cross}; \
      linker="${host}-linux-gnu-gcc"; \
    else \
      apt-get install -y --no-install-recommends gcc libc6-dev; \
      linker=gcc; \
    fi; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "${triple}"; \
    printf 'RUST_TARGET=%s\nRUST_LINKER=%s\n' "${triple}" "${linker}" > /rust-target.env

ENV CARGO_HOME=/cargo-home
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_TARGET_DIR=/cargo-target
ENV RUST_BACKTRACE=1

COPY Cargo.toml Cargo.lock ./
COPY .cargo/ .cargo/
COPY crates/ crates/

RUN --mount=type=cache,id=mat-gateway-cargo-home,target=/cargo-home,sharing=locked \
    --mount=type=cache,id=mat-gateway-cargo-target-${TARGETARCH},target=/cargo-target,sharing=locked \
    set -eu; \
    . /rust-target.env; \
    triple_env="$(echo "${RUST_TARGET}" | tr 'a-z-' 'A-Z_')"; \
    triple_cc="$(echo "${RUST_TARGET}" | tr '-' '_')"; \
    export "CARGO_TARGET_${triple_env}_LINKER=${RUST_LINKER}" \
           "CC_${triple_cc}=${RUST_LINKER}"; \
    cargo build -p carbide-mat-protocol-gateway --bin mat-protocol-gateway \
      --locked --release --target "${RUST_TARGET}"; \
    mkdir -p /artifacts; \
    cp "/cargo-target/${RUST_TARGET}/release/mat-protocol-gateway" /artifacts/mat-protocol-gateway

# Runtime stage
FROM ${RUNTIME_IMAGE}

ARG VERSION
ARG CI_COMMIT_SHORT_SHA
LABEL org.opencontainers.image.version="${VERSION}"
LABEL org.opencontainers.image.revision="${CI_COMMIT_SHORT_SHA}"
LABEL org.opencontainers.image.title="mat-k8s-controller"
LABEL org.opencontainers.image.description="machine-a-tron Kubernetes controller and protocol gateway"
LABEL org.opencontainers.image.vendor="NVIDIA"

COPY --from=controller-builder /out/mat-k8s-controller /mat-k8s-controller
COPY --from=gateway-builder /artifacts/mat-protocol-gateway /mat-protocol-gateway

USER nonroot:nonroot

ENTRYPOINT ["/mat-k8s-controller"]
