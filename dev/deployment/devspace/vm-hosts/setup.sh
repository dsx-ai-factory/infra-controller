#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
export LOCAL_DEV_NAMESPACE="${1:-nico-system}"
script_dir="$(cd -- "$(dirname -- "$0")" && pwd)"
python3 "${script_dir}/setup.py"
