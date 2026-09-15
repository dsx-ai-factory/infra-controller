#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Report enabled external LoadBalancer Services without VIPs using parsed Core site values."""

import sys

try:
    import yaml
except ImportError:
    raise SystemExit("Core VIP preflight requires PyYAML in the python3 environment")


def missing_vips(stream):
    """Find missing VIPs without making Service enablement depend on YAML formatting."""
    # Resolve YAML syntax before applying the Service enablement rules.
    values = yaml.safe_load(stream)
    if values is None:
        values = {}
    if not isinstance(values, dict):
        raise ValueError("Core values must be a YAML mapping")

    missing = []
    for component, config in values.items():
        # Only an explicit false disables the owning chart in the site values.
        if not isinstance(config, dict) or config.get("enabled") is False:
            continue
        for name in ("externalService", "v6ExternalService"):
            service = config.get(name) or {}
            if not isinstance(service, dict):
                raise ValueError(f"{component}.{name} must be a mapping")
            if not service.get("enabled"):
                continue
            # externalService honors type; the DHCPv6 template always renders LoadBalancer.
            if name == "externalService" and (service.get("type") or "LoadBalancer") != "LoadBalancer":
                continue
            # The v6 external flag alone cannot create a DHCPv6 workload.
            if name == "v6ExternalService":
                dhcp = config.get("dhcp") or {}
                if not isinstance(dhcp, dict):
                    raise ValueError(f"{component}.dhcp must be a mapping")
                if not dhcp.get("v6Enabled"):
                    continue

            # Only DNS and NTP render per-pod annotations; other charts ignore that field.
            annotations = (service.get("perPodAnnotations") or [None]
                           if component in ("nico-dns", "nico-ntp")
                           else [service.get("annotations")])
            if not isinstance(annotations, list):
                raise ValueError(f"{component}.{name}.perPodAnnotations must be a list")
            for entry in annotations:
                if entry is not None and not isinstance(entry, dict):
                    raise ValueError(f"{component}.{name} annotations must be mappings")
                # Preserve both MetalLB annotation spellings accepted by existing site files.
                vips = [value for key, value in (entry or {}).items()
                        if key in ("metallb.universe.tf/loadBalancerIPs", "metallb.io/loadBalancerIPs")]
                if not vips or any(vip is None or not str(vip).strip() for vip in vips):
                    missing.append(f"{component}.{name}")
                    break
    return missing


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: check-external-service-vips.py CORE_VALUES_FILE")
    try:
        # Parse the complete document before printing diagnostics to avoid partial success.
        with open(sys.argv[1], encoding="utf-8") as stream:
            services = missing_vips(stream)
        for service in services:
            print(service)
    except (OSError, ValueError, yaml.YAMLError) as error:
        raise SystemExit(f"Cannot check external Service VIPs: {error}") from error
