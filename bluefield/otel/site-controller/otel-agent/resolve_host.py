# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Resolve API host aliases for the optional OTEL Pod."""

import socket
import sys


if __name__ == "__main__":
    hostname = sys.argv[1]
    try:
        answers = socket.getaddrinfo(hostname, None, socket.AF_UNSPEC, socket.SOCK_STREAM)
    except socket.gaierror as error:
        sys.exit(f"Failed to resolve {hostname}: {error}")

    # NSS can return duplicate host entries; keep one alias per address in resolver order.
    for address in dict.fromkeys(answer[4][0] for answer in answers):
        print(address)
