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
# Container for running job test:machine_lifecycle_test
#
# The admin-cli is not baked in: it runs in-pod via `kubectl exec` into the
# site's NICo API deployment, using the site's own binary and the pod's
# identity (see lib/admin_cli.py). This image therefore needs only Python, uv
# and kubectl.
#
FROM python:3.12-slim

ENV PYTHONUNBUFFERED=1

# Install various necessary tools
RUN apt-get update && \
    apt-get install -y \
    curl \
    jq \
    openssh-client

RUN rm -rf /var/lib/apt/lists/*

# kubectl - the admin-cli runs in-pod via `kubectl exec`
ARG KUBECTL_VERSION=v1.31.4
RUN arch="$(dpkg --print-architecture)" && \
    curl -fsSLo /usr/local/bin/kubectl "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${arch}/kubectl" && \
    curl -fsSL "https://dl.k8s.io/release/${KUBECTL_VERSION}/bin/linux/${arch}/kubectl.sha256" \
      | awk -v f=/usr/local/bin/kubectl '{print $1"  "f}' | sha256sum -c - && \
    chmod +x /usr/local/bin/kubectl && \
    kubectl version --client

# Install uv (Python manager). Versioned tarball + checksum.
# Bump UV_VERSION and both sums together (each sha256 is published alongside
# its release asset as uv-<triple>.tar.gz.sha256).
ARG UV_VERSION=0.11.32
ARG UV_SHA256_AMD64=aab924fd522efd06f1c5f3b93a243864fc453132c94b2dc49f1371b528a4b967
ARG UV_SHA256_ARM64=4d4fa08d95b06642e5800df6a22bd71455f23f988269e18da2847971d8c0bf31
RUN arch="$(dpkg --print-architecture)" && \
    case "${arch}" in \
      amd64) triple="x86_64-unknown-linux-gnu";  sha256="${UV_SHA256_AMD64}" ;; \
      arm64) triple="aarch64-unknown-linux-gnu"; sha256="${UV_SHA256_ARM64}" ;; \
      *) echo "no uv build pinned for architecture ${arch}" >&2; exit 1 ;; \
    esac && \
    cd /tmp && \
    curl -fsSLo uv.tar.gz "https://github.com/astral-sh/uv/releases/download/${UV_VERSION}/uv-${triple}.tar.gz" && \
    echo "${sha256}  uv.tar.gz" | sha256sum -c - && \
    tar -xzf uv.tar.gz && \
    mv "uv-${triple}/uv" "uv-${triple}/uvx" /usr/local/bin/ && \
    rm -rf uv.tar.gz "uv-${triple}" && \
    uv --version

WORKDIR /test

# Copy project metadata to the current directory
COPY pyproject.toml uv.lock ./

# Create a Python virtual env and install dependencies (no dev deps)
ENV UV_NO_DEV=1
RUN uv sync --frozen

# Point uv to the virtual env
ENV UV_PROJECT_ENVIRONMENT=/test/.venv

COPY . .
