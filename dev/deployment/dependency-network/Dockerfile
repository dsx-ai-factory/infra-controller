# syntax=docker/dockerfile:1.7
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

ARG BUILD_IMAGE=nico-dependency-build
FROM ${BUILD_IMAGE}
ARG VERSION
ARG CI_COMMIT_SHORT_SHA
ARG SOURCE_REVISION
LABEL org.opencontainers.image.revision=${SOURCE_REVISION}
ENV CARGO_HOME=/cargo-home CARGO_TARGET_DIR=/cargo-target CARGO_NET_GIT_FETCH_WITH_CLI=true
ENV CARGO_PROFILE_DEV_DEBUG=line-tables-only CARBIDE_BUILD_HELM_VERSION=0.0.0-qualification
ENV VERSION=${VERSION} CI_COMMIT_SHORT_SHA=${CI_COMMIT_SHORT_SHA}
WORKDIR /workspace
COPY . .
ARG TARGETARCH
RUN --mount=type=cache,id=nico-dependency-cargo-home-${TARGETARCH},target=/cargo-home,sharing=locked \
    --mount=type=cache,id=nico-dependency-cargo-target-${TARGETARCH},target=/cargo-target,sharing=locked <<'BUILD'
set -eu
test -n "$VERSION" && test -n "$CI_COMMIT_SHORT_SHA" && test -n "$SOURCE_REVISION"
cargo test --locked --jobs 4 --no-run --lib -p carbide-api -p carbide-secrets \
    --message-format=json-render-diagnostics > /tmp/artifacts.json
mkdir /qualification
python3 - <<'PY'
import json
import shutil
from pathlib import Path

rows = [json.loads(line) for line in Path('/tmp/artifacts.json').read_text().splitlines()]
binaries = [row for row in rows if row.get('reason') == 'compiler-artifact' and row.get('executable')]
assert {row['target']['name'] for row in binaries} == {'carbide', 'carbide_secrets'}
for row in binaries:
    shutil.copy(row['executable'], '/qualification/' + row['target']['name'])
PY
{ rustc --version; cargo --version; vault --version; /usr/lib/postgresql/15/bin/postgres --version; } > /qualification/versions.txt
BUILD
