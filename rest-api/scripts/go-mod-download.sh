#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Runs `go mod download`, retrying when proxy.golang.org drops a download
# mid-stream. Go keeps the modules it already fetched, so a retry only fetches
# what failed. Run it before any step that builds, tests, or lints Go code,
# since those download missing modules themselves and do not retry.

set -eu

attempts=5
attempt=1
until go mod download; do
  if [ "$attempt" -ge "$attempts" ]; then
    echo "go mod download failed after ${attempts} attempts" >&2
    exit 1
  fi
  delay=$((attempt * 10))
  echo "go mod download failed (attempt ${attempt} of ${attempts}), retrying in ${delay}s" >&2
  sleep "$delay"
  attempt=$((attempt + 1))
done
