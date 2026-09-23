# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Render the optional OTEL Pod with one host alias per resolved API address."""

import sys

import yaml


if __name__ == "__main__":
    template_path, hostname, addresses = sys.argv[1:]
    with open(template_path, encoding="utf-8") as stream:
        pod = yaml.safe_load(stream)
    # Keep every DNS answer as its own Kubernetes host alias.
    pod["spec"]["hostAliases"] = [
        {"ip": address, "hostnames": [hostname]}
        for address in addresses.split()
    ]
    yaml.safe_dump(pod, sys.stdout, sort_keys=False)
