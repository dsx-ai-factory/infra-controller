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

# Multi-stage build for custom OpenTelemetry Collector targeting arm64 DPUs.
#
# Stage 1: Build the custom otelcol-contrib binary with fileresourceprocessor
# and telemetrystatsprocessor using the OpenTelemetry Collector Builder (ocb).
# Runs on the host platform and cross-compiles to arm64 via GOOS/GOARCH.
#
# Stage 2: Download and verify crictl in a throwaway stage so curl/tar stay
# out of the runtime image.
#
# Stage 3: Assemble the runtime image with the binary, crictl, wrapper
# scripts, and default configuration.
#
# NOTE: This image uses debian:12-slim instead of NVIDIA distroless because the
# otelcol-wrapper entrypoint is a bash script that requires a full shell.
#
# The journaldreceiver invokes `journalctl`; install systemd so that binary and
# its libraries are present (see journaldreceiver/examples/container/README.md
# in opentelemetry-collector-contrib).

ARG GO_VERSION=1.26.4
ARG OTELCOL_VERSION=0.155.0
ARG CRICTL_VERSION=1.34.0

# ---------------------------------------------------------------------------
# Builder (host-native Go, cross-compiles to arm64)
# ---------------------------------------------------------------------------
FROM golang:${GO_VERSION} AS builder

ARG OTELCOL_VERSION

WORKDIR /build

# Install ocb
RUN CGO_ENABLED=0 go install \
    go.opentelemetry.io/collector/cmd/builder@v${OTELCOL_VERSION} \
    && mv "$(go env GOPATH)/bin/builder" /usr/local/bin/ocb

# Copy custom processors, receiver, and builder config template
COPY bluefield/otel/fileresourceprocessor /build/fileresourceprocessor
COPY bluefield/otel/telemetrystatsprocessor /build/telemetrystatsprocessor
COPY bluefield/otel/puntstatsreceiver /build/puntstatsreceiver
COPY bluefield/otel/otelcol_builder_config_yaml.txt /build/
COPY bluefield/otel/get_module_version.sh /build/

# Committed go.mod/go.sum produced by running update-ocb-modules.sh locally.
# Using these instead of a fresh `go mod tidy` inside the build makes the
# dependency graph deterministic — two builds from the same commit produce
# identical module sets. Run update-ocb-modules.sh and commit the outputs
# whenever otelcol_builder_config_yaml.txt or component versions change.
COPY bluefield/otel/ocb-generated/go.mod /build/ocb-generated/go.mod
COPY bluefield/otel/ocb-generated/go.sum /build/ocb-generated/go.sum

# Generate the builder config with resolved versions
RUN FILERESOURCE_VERSION=$(bash /build/get_module_version.sh /build/fileresourceprocessor) && \
    TELEMETRYSTATS_VERSION=$(bash /build/get_module_version.sh /build/telemetrystatsprocessor) && \
    PUNTSTATS_VERSION=$(bash /build/get_module_version.sh /build/puntstatsreceiver) && \
    sed -e "s/\${VERSION}/${OTELCOL_VERSION}/g" \
        -e "s/\${FILERESOURCE_VERSION}/${FILERESOURCE_VERSION}/g" \
        -e "s/\${TELEMETRYSTATS_VERSION}/${TELEMETRYSTATS_VERSION}/g" \
        -e "s/\${PUNTSTATS_VERSION}/${PUNTSTATS_VERSION}/g" \
        otelcol_builder_config_yaml.txt > ocb_config.yaml

# Generate Go source only — skip the go mod tidy step that makes module
# resolution non-deterministic across builds.
RUN ocb --config ocb_config.yaml --skip-compilation --skip-get-modules

# Overwrite the incomplete OCB-generated go.mod/go.sum (missing indirect deps)
# with the committed versions that have the full module graph resolved.
RUN cp /build/ocb-generated/go.mod /build/ocb-build/go.mod && \
    cp /build/ocb-generated/go.sum /build/ocb-build/go.sum

# Cross-compile from the pinned module graph.
RUN cd /build/ocb-build && \
    CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -mod=readonly -o otelcol-contrib .

# ---------------------------------------------------------------------------
# Downloader: fetch and verify crictl (build-only; curl/tar stay out of the
# runtime image)
# ---------------------------------------------------------------------------
FROM debian:12-slim AS crictl-downloader

ARG CRICTL_VERSION
# sha256 of crictl-v${CRICTL_VERSION}-linux-arm64.tar.gz from the cri-tools
# release page; update alongside CRICTL_VERSION.
ARG CRICTL_SHA256=c31d252e203df5f4cf37f314bd3092eb79087e791631c1e607087c74b6d0423f

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /dl

# crictl: used only by punt-stats-helper (see below) to exec into the
# doca-hbn container over the containerd socket (mounted at runtime, and
# only into that sidecar) and read punt stats, since doca-hbn's stats file
# is not reachable any other way from the host.
RUN curl -fsSL --retry 5 --retry-connrefused --max-time 300 \
        "https://github.com/kubernetes-sigs/cri-tools/releases/download/v${CRICTL_VERSION}/crictl-v${CRICTL_VERSION}-linux-arm64.tar.gz" \
        -o crictl.tar.gz \
    && echo "${CRICTL_SHA256}  crictl.tar.gz" | sha256sum -c - \
    && mkdir -p extract \
    && tar -xzf crictl.tar.gz -C extract crictl \
    && rm crictl.tar.gz

# ---------------------------------------------------------------------------
# Runtime (arm64)
# ---------------------------------------------------------------------------
FROM debian:12-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    jq \
    libudev1 \
    systemd \
    && rm -rf /var/lib/apt/lists/*

COPY --from=crictl-downloader --chmod=755 /dl/extract/crictl /usr/bin/crictl

# Collector binary
COPY --from=builder --chmod=755 /build/ocb-build/otelcol-contrib /usr/bin/otelcol-contrib

COPY --from=builder /lib/aarch64-linux-gnu/libudev.so.1 /lib/aarch64-linux-gnu/libudev.so.1
# Wrapper scripts
COPY bluefield/otel/otelcol-wrapper /etc/otelcol-contrib/otelcol-wrapper
COPY bluefield/otel/otelcol-wrapper-imports-dpf /etc/otelcol-contrib/otelcol-wrapper-imports
COPY bluefield/otel/otelcol-wrapper-validate /etc/otelcol-contrib/otelcol-wrapper-validate
# punt-stats-helper is the only entrypoint in this image that uses crictl; it
# runs as a separate sidecar container with the containerd-socket mount so
# otelcol-contrib itself never needs CRI exec access (see nico-otelcol
# daemonset.yaml).
COPY bluefield/otel/punt-stats-helper /etc/otelcol-contrib/punt-stats-helper
RUN chmod +x /etc/otelcol-contrib/otelcol-wrapper \
             /etc/otelcol-contrib/otelcol-wrapper-imports \
             /etc/otelcol-contrib/otelcol-wrapper-validate \
             /etc/otelcol-contrib/punt-stats-helper

# Config fragments directory
RUN mkdir -p /etc/otelcol-contrib/config-fragments \
             /var/lib/otelcol-contrib/cursors \
             /run/otelcol-contrib

# Default config is mounted via ConfigMap at runtime
ENTRYPOINT ["/etc/otelcol-contrib/otelcol-wrapper"]
