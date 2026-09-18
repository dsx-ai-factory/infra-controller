#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Validate enabled external LoadBalancer Service VIPs against parsed site values and pools."""

import ipaddress
import sys

try:
    import yaml
except ImportError:
    raise SystemExit("Core VIP preflight requires PyYAML in the python3 environment")


def check_vips(stream, metallb_stream=None):
    """Check active Service VIPs and optional rendered pools without relying on YAML formatting.

    Return Service errors, warnings, and pool errors separately so preflight identifies the right file.
    """
    # Resolve YAML syntax before applying the Service enablement rules.
    values = yaml.safe_load(stream)
    if values is None:
        values = {}
    if not isinstance(values, dict):
        raise ValueError("Core values must be a YAML mapping")

    errors = []
    warnings = []
    pool_errors = []
    pools = []
    # Use the rendered IPAddressPools, excluding unrelated addresses such as BGP peers.
    if metallb_stream is not None:
        for document in yaml.safe_load_all(metallb_stream):
            if document is None:
                continue
            if not isinstance(document, dict):
                raise ValueError("MetalLB resources must be YAML mappings")
            if document.get("kind") != "IPAddressPool":
                continue
            spec = document.get("spec", {})
            if not isinstance(spec, dict):
                raise ValueError("MetalLB IPAddressPool.spec must be a mapping")
            blocks = spec.get("addresses", [])
            if not isinstance(blocks, list):
                raise ValueError("MetalLB IPAddressPool.spec.addresses must be a list")
            if not blocks:
                pool_errors.append("MetalLB IPAddressPool has no addresses (add your VIP CIDR(s)/range(s))")
            for block in blocks:
                try:
                    if not isinstance(block, str):
                        raise ValueError("expected a CIDR or address range")
                    if "-" in block:
                        first, last = (ipaddress.ip_address(value.strip()) for value in block.split("-", 1))
                        if first.version != last.version or int(first) > int(last):
                            raise ValueError("range endpoints must use one family in ascending order")
                    else:
                        if "/" not in block:
                            raise ValueError("expected a CIDR or address range")
                        network = ipaddress.ip_network(block, strict=False)
                        first, last = network.network_address, network.broadcast_address
                    pools.append((first, last))
                except ValueError as error:
                    pool_errors.append(f"MetalLB pool entry {block!r} is invalid: {error}")

    seen_vips = {}
    for component, config in values.items():
        # Only an explicit false disables the owning chart in the site values.
        if not isinstance(config, dict) or config.get("enabled") is False:
            continue
        for name in ("externalService", "v6ExternalService"):
            service = config.get(name)
            # Omission and null disable the Service; other types must still be validated.
            if service is None:
                continue
            if not isinstance(service, dict):
                raise ValueError(f"{component}.{name} must be a mapping")
            if not service.get("enabled"):
                continue
            # externalService honors type; the DHCPv6 template always renders LoadBalancer.
            if name == "externalService" and (service.get("type") or "LoadBalancer") != "LoadBalancer":
                continue
            # The v6 external flag alone cannot create a DHCPv6 workload.
            if name == "v6ExternalService":
                dhcp = config.get("dhcp")
                if dhcp is None:
                    continue
                if not isinstance(dhcp, dict):
                    raise ValueError(f"{component}.dhcp must be a mapping")
                if not dhcp.get("v6Enabled"):
                    continue

            # Only DNS and NTP render per-pod annotations; other charts ignore that field.
            annotations = (service.get("perPodAnnotations")
                           if component in ("nico-dns", "nico-ntp")
                           else [service.get("annotations")])
            if annotations is not None and not isinstance(annotations, list):
                raise ValueError(f"{component}.{name}.perPodAnnotations must be a list")
            # Only valid empty lists and omission use the automatic-allocation fallback.
            for index, entry in enumerate(annotations or [None]):
                if entry is not None and not isinstance(entry, dict):
                    raise ValueError(f"{component}.{name} annotations must be mappings")
                # Preserve both MetalLB annotation spellings accepted by existing site files.
                vips = [value for key, value in (entry or {}).items()
                        if key in ("metallb.universe.tf/loadBalancerIPs", "metallb.io/loadBalancerIPs")]
                # Existing Services may use automatic allocation; DHCPv6 requires an explicit relay VIP.
                if (not vips and name == "v6ExternalService") or any(
                        vip is None or not str(vip).strip() for vip in vips):
                    errors.append(f"{component}.{name} needs loadBalancerIPs from your MetalLB pool")
                    break
                owner = f"{component}.{name}[{index}]"
                for value in vips:
                    for vip in str(value).split(","):
                        try:
                            address = ipaddress.ip_address(vip.strip())
                        except ValueError:
                            errors.append(f"{component}.{name}: loadBalancerIP {vip.strip()!r} is not a valid IP address")
                            continue
                        # The DHCPv6 relay Service is explicitly SingleStack IPv6.
                        if name == "v6ExternalService" and address.version != 6:
                            errors.append(f"{component}.{name}: VIP {address} must be an IPv6 address")
                            continue
                        # These external Services explicitly render SingleStack IPv4.
                        if name == "externalService" and component in ("nico-dhcp", "unbound") and address.version != 4:
                            errors.append(f"{component}.{name}: VIP {address} must be an IPv4 address")
                            continue
                        # Normalize addresses, but do not count annotation aliases as separate Services.
                        if seen_vips.get(address) == owner:
                            continue
                        if address in seen_vips:
                            warnings.append(f"VIP {address} is assigned to more than one service (each service needs a unique IP)")
                        seen_vips[address] = owner
                        # Missing rendered pools retain the existing format-only validation behavior.
                        if pools and not any(first.version == address.version and first <= address <= last
                                             for first, last in pools):
                            errors.append(f"{component}.{name}: VIP {address} is not within any MetalLB IPAddressPool")
    return errors, warnings, pool_errors


if __name__ == "__main__":
    if len(sys.argv) not in (2, 3) or (len(sys.argv) == 3 and sys.argv[2] != "--metallb-stdin"):
        raise SystemExit("usage: check-external-service-vips.py CORE_VALUES_FILE [--metallb-stdin]")
    try:
        # Parse the complete document before printing diagnostics to avoid partial success.
        with open(sys.argv[1], encoding="utf-8") as stream:
            errors, warnings, pool_errors = check_vips(stream, sys.stdin if len(sys.argv) == 3 else None)
        for error in pool_errors:
            print(f"ERROR[pool]: {error}")
        for error in errors:
            print(f"ERROR: {error}")
        for warning in warnings:
            print(f"WARNING: {warning}")
    except (OSError, ValueError, yaml.YAMLError) as error:
        raise SystemExit(f"Cannot check external Service VIPs: {error}") from error
