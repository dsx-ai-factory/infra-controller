#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check that every machine-a-tron BMC and NVOS network in a values file lies outside the cluster ServiceCIDR."""

import ipaddress
import os
import re
import subprocess
import sys

try:
    import tomllib
except ImportError:
    raise SystemExit("machine-a-tron ServiceCIDR preflight requires python3 3.11 or later")
try:
    import yaml
except ImportError:
    raise SystemExit("machine-a-tron ServiceCIDR preflight requires PyYAML in the python3 environment")

# Relay keys per published network and group kind; the second name of each
# pair is the deprecated alias the chart accepts. BMC leases are published as
# Service externalIPs, and so are the NVOS leases of simulated NVLink switches
# (for the NMX-C mock), which come from the underlay relay's network. The
# underlay relay is optional in the chart, so its absence is not an error.
RELAY_KINDS = (
    ("BMC", True, {
        "machines": ("bmcDhcpRelayAddress", "oobDhcpRelayAddress"),
        "racks": ("bmc_dhcp_relay_address", "oob_dhcp_relay_address"),
    }),
    ("NVOS", False, {
        "machines": ("underlayDhcpRelayAddress", "adminDhcpRelayAddress"),
        "racks": ("underlay_dhcp_relay_address", "admin_dhcp_relay_address"),
    }),
)


def parse_networks(tokens, source):
    """Parse CIDR tokens and report, rather than drop, a token that is not a CIDR."""
    networks = []
    errors = []
    for token in tokens:
        try:
            networks.append(ipaddress.ip_network(token, strict=False))
        except ValueError:
            errors.append(f"{source} entry {token!r} is not a CIDR")
    return networks, errors


def site_prefixes(site_config):
    """Read the [networks.*] prefixes of a site config, given as TOML or as the Core values YAML that embeds it."""
    if not site_config.strip():
        return []
    try:
        config = tomllib.loads(site_config)
    except tomllib.TOMLDecodeError as toml_error:
        values = yaml.safe_load(site_config)
        try:
            embedded = values["nico-api"]["siteConfig"]["nicoApiSiteConfig"]
        except (KeyError, TypeError):
            embedded = None
        if not isinstance(embedded, str):
            raise ValueError("site config is neither TOML nor Core values with "
                             f"nico-api.siteConfig.nicoApiSiteConfig: {toml_error}") from None
        try:
            config = tomllib.loads(embedded)
        except tomllib.TOMLDecodeError as error:
            raise ValueError(f"nico-api.siteConfig.nicoApiSiteConfig is not TOML: {error}") from None
    networks = config.get("networks", {})
    if not isinstance(networks, dict):
        raise ValueError("site config networks must be a table")
    return [str(network["prefix"]) for network in networks.values()
            if isinstance(network, dict) and network.get("prefix") is not None]


def relay_addresses(values):
    """Collect the BMC and NVOS DHCP relay addresses of every configured machine and rack group."""
    if values is None:
        values = {}
    if not isinstance(values, dict):
        raise ValueError("machine-a-tron values must be a YAML mapping")
    dhcp_relay = values.get("dhcpRelay")
    if dhcp_relay is not None and not isinstance(dhcp_relay, dict):
        raise ValueError("dhcpRelay must be a mapping")
    # DHCP relay mode places the BMC network inside a dedicated ServiceCIDR by design.
    if dhcp_relay and dhcp_relay.get("baseIP"):
        raise ValueError("dhcpRelay.baseIP is set: DHCP relay mode is not covered by this check")
    controller = values.get("mat-k8s-controller")
    if controller is not None and not isinstance(controller, dict):
        raise ValueError("mat-k8s-controller must be a mapping")
    if "pods" not in values:
        # Helm keeps the chart's default group when the values define no pods.
        if controller and controller.get("enabled"):
            raise ValueError("mat-k8s-controller.enabled is true but pods is not set, so the chart's default "
                             "machine group would deploy unchecked; pass the complete values file of the install")
        return []
    pods = values["pods"]
    if pods is None:
        return []
    if not isinstance(pods, dict):
        raise ValueError("pods must be a mapping")
    relays = []
    for pod_name, pod in pods.items():
        # A null pod disables a pod that the chart or another values file defines.
        if pod is None:
            continue
        if not isinstance(pod, dict):
            raise ValueError(f"pods.{pod_name} must be a mapping")
        for section in ("machines", "racks"):
            groups = pod.get(section)
            if groups is None:
                continue
            if not isinstance(groups, dict):
                raise ValueError(f"pods.{pod_name}.{section} must be a mapping")
            for group_name, group in groups.items():
                if group is None:
                    continue
                if not isinstance(group, dict):
                    raise ValueError(f"pods.{pod_name}.{section}.{group_name} must be a mapping")
                owner = f"pods.{pod_name}.{section}.{group_name}"
                for label, required, keys_by_section in RELAY_KINDS:
                    keys = keys_by_section[section]
                    relay = next((group[key] for key in keys if group.get(key) is not None), None)
                    if relay is None:
                        if required:
                            raise ValueError(f"{owner}.{keys[0]} is not set; the chart substitutes its own "
                                             "default, which this check cannot vouch for")
                        continue
                    try:
                        relays.append((owner, ipaddress.ip_address(str(relay).strip()), label))
                    except ValueError:
                        raise ValueError(f"{owner}.{keys[0]} {relay!r} is not an IP address") from None
    return relays


def check_service_cidr(stream, service_cidrs, bmc_prefixes=(), site_config=""):
    """Resolve every relay to its BMC or NVOS network and report overlaps with the Service CIDRs.

    Return the checked networks and the errors; an empty error list means the deployment is safe.
    """
    relays = relay_addresses(yaml.safe_load(stream))
    service_networks, errors = parse_networks(service_cidrs, "SCALE_SERVICE_CIDRS")
    extra_networks, extra_errors = parse_networks(bmc_prefixes, "SCALE_BMC_PREFIXES")
    site_networks, site_errors = parse_networks(site_prefixes(site_config), "site config")
    errors += extra_errors + site_errors
    if not service_networks and not errors:
        errors.append("cannot determine the cluster ServiceCIDR: set SCALE_SERVICE_CIDRS=\"<cidr> ...\"")
    known = site_networks + extra_networks
    # Each checked network with the kinds of lease it carries, for the error text.
    labels = {network: {"BMC"} for network in extra_networks}
    for owner, relay, label in relays:
        # A relay sits inside its own segment and inside any wider declared range.
        containing = [network for network in known if relay in network]
        if not containing:
            errors.append(f"{owner}: cannot determine the {label} network of relay {relay}; "
                          "declare its [networks.*] prefix in the site config or set SCALE_BMC_PREFIXES")
            continue
        labels.setdefault(max(containing, key=lambda network: network.prefixlen), set()).add(label)
    ordered = sorted(labels, key=lambda network: (network.version, int(network.network_address), network.prefixlen))
    for prefix in ordered:
        for cidr in service_networks:
            if prefix.overlaps(cidr):
                errors.append(f"{'/'.join(sorted(labels[prefix]))} network {prefix} "
                              f"overlaps the cluster ServiceCIDR {cidr}")
    return ordered, errors


def cluster_service_cidrs():
    """Read the Service CIDRs with kubectl: ServiceCIDR objects, then kubeadm-config, then the apiserver flag."""
    probes = (
        (["kubectl", "get", "servicecidrs", "-o", "jsonpath={.items[*].spec.cidrs[*]}"],
         lambda output: output.split()),
        (["kubectl", "get", "cm", "kubeadm-config", "-n", "kube-system", "-o", "jsonpath={.data.ClusterConfiguration}"],
         lambda output: [cidr for line in output.splitlines() if line.strip().startswith("serviceSubnet:")
                         for cidr in line.split(":", 1)[1].replace(",", " ").split()]),
        (["kubectl", "cluster-info", "dump"],
         lambda output: next((match.group(1).replace(",", " ").split()
                              for match in re.finditer(r'service-cluster-ip-range=([^" ]+)', output)), [])),
    )
    for command, extract in probes:
        try:
            result = subprocess.run(command, capture_output=True, text=True, check=False)
        except OSError:
            return []
        if result.returncode != 0:
            continue
        cidrs = extract(result.stdout)
        if cidrs:
            return cidrs
    return []


if __name__ == "__main__":
    if len(sys.argv) not in (2, 4) or (len(sys.argv) == 4 and sys.argv[2] != "--site-config"):
        raise SystemExit("usage: check-mat-service-cidr.py MAT_VALUES_FILE [--site-config CORE_VALUES_OR_SITE_CONFIG_FILE]")
    try:
        service_cidrs = os.environ.get("SCALE_SERVICE_CIDRS", "").split() or cluster_service_cidrs()
        site_config = ""
        if len(sys.argv) == 4:
            with open(sys.argv[3], encoding="utf-8") as stream:
                site_config = stream.read()
        with open(sys.argv[1], encoding="utf-8") as stream:
            prefixes, errors = check_service_cidr(
                stream, service_cidrs, os.environ.get("SCALE_BMC_PREFIXES", "").split(), site_config)
        for error in errors:
            print(f"ERROR: {error}")
        if errors:
            sys.exit(1)
        print(f"OK: BMC/NVOS networks {' '.join(str(prefix) for prefix in prefixes)} "
              f"are outside the ServiceCIDR {' '.join(service_cidrs)}")
    except (OSError, ValueError, yaml.YAMLError) as error:
        raise SystemExit(f"Cannot check the machine-a-tron BMC/NVOS networks: {error}") from error
