#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Extract an ARM64 Image from Alpine's gzip EFI zboot executable."""
import gzip
import struct
import sys
from pathlib import Path

src, dst = map(Path, sys.argv[1:])
data = src.read_bytes()
if data[4:8] == b"zimg":
    offset, size = struct.unpack_from("<II", data, 8)
    if data[24:28] != b"gzip":
        raise SystemExit("Unsupported EFI zboot compression (expected gzip)")
    data = gzip.decompress(data[offset:offset + size])
elif data.startswith(b"\x1f\x8b"):
    data = gzip.decompress(data)
if data[56:60] != b"ARM\x64":
    raise SystemExit("Extracted kernel is not an uncompressed ARM64 Image")
dst.write_bytes(data)
