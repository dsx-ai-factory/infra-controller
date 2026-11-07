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

# Sidecar image for the wobcom transceiver-exporter, scraped by the
# prometheus/transceiver receiver in the nico-otelcol collector. There is no
# upstream container image, so we package the published arm64 release binary
# (the same artifact the forge-dpu .deb installs as a host systemd service).
#
# Keep TRANSCEIVER_VERSION in sync with bluefield/transceiver_exporter/version.txt.

ARG TRANSCEIVER_VERSION=1.5.0

# Runtime base, matching the other bluefield images (carbide-fmds,
# forge-dhcp-server, forge-dpu-agent). Declared before the first FROM so the
# values are in scope for the runtime stage's FROM below.
ARG CC_VERSION=v3.4.0 # NVIDIA Distroless Container version
ARG TAG=${CC_VERSION}
ARG IMG=nvcr.io/nvidia/distroless/cc
ARG DEBUG_DISTROLESS=true
ARG DISTROLESS_DEBUG_TAG=${DEBUG_DISTROLESS:+"dev"} # 'dev' provides a busybox shell

# ---------------------------------------------------------------------------
# Downloader: fetch and extract the release tarball
# ---------------------------------------------------------------------------
FROM debian:12-slim AS downloader

ARG TRANSCEIVER_VERSION

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    wget \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /dl
# The release tarball holds the binary and LICENSE.md at the top level (no
# nested directory), matching the download-transceiver-exporter Make task.
RUN wget -q --tries=5 --timeout=300 \
        "https://github.com/wobcom/transceiver-exporter/releases/download/v${TRANSCEIVER_VERSION}/transceiver-exporter-v${TRANSCEIVER_VERSION}-linux-arm64.tar.gz" \
        -O transceiver-exporter.tar.gz \
    && mkdir -p extract \
    && tar xzf transceiver-exporter.tar.gz -C extract \
    && rm transceiver-exporter.tar.gz

# ---------------------------------------------------------------------------
# Runtime (arm64) — NVIDIA Distroless Container
# ---------------------------------------------------------------------------
# NVIDIA Distroless Container
FROM --platform=linux/arm64 ${IMG}:${TAG}-${DISTROLESS_DEBUG_TAG}

COPY --from=downloader --chmod=755 /dl/extract/transceiver-exporter /usr/bin/transceiver-exporter
COPY --from=downloader /dl/extract/LICENSE.md /usr/share/licenses/transceiver-exporter/LICENSE.md

ENTRYPOINT ["/usr/bin/transceiver-exporter"]
