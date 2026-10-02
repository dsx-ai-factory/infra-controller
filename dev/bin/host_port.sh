#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

host_port() {
  local host="$1"
  case "$host" in
    \[*\]) ;;
    *:*) host="[$host]" ;;
  esac
  printf '%s:%s' "$host" "$2"
}
