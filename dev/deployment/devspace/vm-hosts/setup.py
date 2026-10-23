#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Connect the private target bridge to local kind and register expected hosts."""

import ipaddress
import json
import os
import re
import subprocess
from pathlib import Path

import lab

HERE = Path(__file__).resolve().parent
NAMESPACE = os.environ.get("LOCAL_DEV_NAMESPACE", "nico-system")
SEGMENT_ID = "00840000-0000-4000-8000-000000000030"


def kubectl(*args):
    return lab.run("kubectl", "-n", NAMESPACE, *args, timeout=360)


def admin(*args):
    return lab.run("bash", str(HERE.parent / "core-admin.sh"), *args)


def network_state(root, node, bridge, uplink):
    path = root / "network-state.json"
    current = dict(node=node, bridge=bridge, uplink=uplink)
    if path.exists():
        saved = json.loads(path.read_text())
        if {key: saved[key] for key in current} != current:
            lab.cleanup_network(root)
    state = json.loads(path.read_text()) if path.exists() else dict(current, routes=[])
    path.write_text(json.dumps(state))
    return state


def main():
    root = Path(
        os.environ.get(
            "LOCAL_DEV_VM_HOSTS_DIR", str(Path.home() / ".local/share/nico-vm-hosts")
        )
    ).resolve()
    inventory = lab.read_inventory(root)
    architecture = inventory["architecture"]
    context = lab.run("kubectl", "config", "current-context")
    if not context.startswith("kind-"):
        raise ValueError("vm-hosts supports only a local kind cluster")
    nodes = lab.run("kind", "get", "nodes", "--name", context[5:]).splitlines()
    if len(nodes) != 1:
        raise ValueError("vm-hosts requires a single-node kind cluster")
    node = nodes[0]
    cluster_uid = kubectl(
        "get", "namespace", "kube-system", "-o", "jsonpath={.metadata.uid}"
    )
    cluster_file = root / "cluster-uid"
    if cluster_file.exists() and cluster_file.read_text() != cluster_uid:
        raise ValueError(
            "kind was recreated while guest disks and BMC state were retained; recover the previous inventory/credentials before deployment"
        )
    kubectl("rollout", "status", "deployment/nico-api", "--timeout=90s")
    machines = json.loads(admin("-f", "json", "em", "show"))["expected_machines"]
    allowed = {host["bmc_mac"] for host in inventory["targets"]}
    if any(machine["bmc_mac_address"].lower() not in allowed for machine in machines):
        raise ValueError(
            "the site contains other expected machines; vm-hosts will not migrate or erase MAT inventory"
        )
    cluster_file.write_text(cluster_uid)

    # Verify real payloads before starting discovery; an HTTP-ready PXE pod can
    # otherwise report success with an empty boot-artifact volume.
    kubectl("rollout", "status", "deployment/nico-pxe", "--timeout=300s")
    kubectl(
        "exec",
        "deployment/nico-pxe",
        "--",
        "sh",
        "-ec",
        f"test -s /forge-boot-artifacts/blobs/internal/{architecture}/ipxe.efi; "
        f"test -s /forge-boot-artifacts/blobs/internal/{architecture}/scout.efi; "
        f"test -s /forge-boot-artifacts/blobs/internal/{architecture}/scout.squashfs; "
        f"test -s /forge-boot-artifacts/blobs/internal/{architecture}/qcow-imager.efi",
    )
    services = {}
    for name in (
        "nico-api",
        "nico-pxe",
        "nico-dns",
        "nico-ntp-client",
        "nico-dhcp",
        "nico-dhcp6",
    ):
        services[name] = json.loads(kubectl("get", "service", name, "-o", "json"))[
            "spec"
        ]["clusterIP"]
    # Published boot artifacts use standard HTTP(S) ports. Retain the chart's
    # original ports for cluster clients and add aliases for real guests.
    for name, port_name, port_number in (
        ("nico-pxe", "http", 80),
        ("nico-api", "grpc", 443),
    ):
        service = json.loads(kubectl("get", "service", name, "-o", "json"))
        ports = service["spec"]["ports"]
        if not any(port["port"] == port_number for port in ports):
            original = next(port for port in ports if port["name"] == port_name)
            ports.append(
                dict(
                    name=f"{port_name}-boot",
                    port=port_number,
                    protocol="TCP",
                    targetPort=original.get("targetPort", original["port"]),
                )
            )
            kubectl(
                "patch",
                "service",
                name,
                "--type=merge",
                "-p",
                json.dumps({"spec": {"ports": ports}}),
            )
    dns = lab.run(
        "kubectl",
        "-n",
        "kube-system",
        "get",
        "service",
        "kube-dns",
        "-o",
        "jsonpath={.spec.clusterIP}",
    )
    services["kube-dns"] = dns
    node_network = lab.inspect("container", node)["NetworkSettings"]["Networks"]["kind"]
    bridge = "br-" + lab.inspect("network", "kind")["Id"][:12]

    def host(*args):
        return lab.run("docker", "exec", lab.NAME, *args)

    def on_node(*args):
        return lab.run("docker", "exec", node, *args)

    uplink = json.loads(host("ip", "-json", "route", "get", "1.1.1.1"))[0]["dev"]
    state_path = root / "network-state.json"
    state = network_state(root, node, bridge, uplink)
    for address in services.values():
        version = ipaddress.ip_address(address).version
        prefix = f"{address}/{32 if version == 4 else 128}"
        gateway = node_network["IPAddress" if version == 4 else "GlobalIPv6Address"]
        if not gateway:
            raise ValueError(
                "kind must have an address matching the provisioning services"
            )
        existing = json.loads(
            host("ip", f"-{version}", "-json", "route", "show", "exact", prefix)
        )
        if existing and any(route.get("gateway") != gateway for route in existing):
            raise ValueError(f"refusing to replace an unrelated route for {prefix}")
        if not existing:
            state["routes"].append([str(version), prefix, gateway])
            state_path.write_text(json.dumps(state))
            host("ip", f"-{version}", "route", "add", prefix, "via", gateway)
    for where, family, table, chain, rule in lab.network_rules(bridge, uplink):
        execute = host if where == "host" else on_node
        try:
            execute(family, "-w", "-t", table, "-C", chain, *rule)
        except subprocess.CalledProcessError:
            execute(family, "-w", "-t", table, "-I", chain, "1", *rule)

    dns_config = json.loads(
        lab.run(
            "kubectl", "-n", "kube-system", "get", "configmap", "coredns", "-o", "json"
        )
    )
    original = dns_config["data"]["Corefile"]
    corefile = re.sub(
        r"(?ms)^    # vm-hosts begin\n.*?^    # vm-hosts end\n", "", original
    )
    aliases = (
        "\n    # vm-hosts begin\n    hosts {\n"
        f"        {services['nico-api']} carbide-api.forge\n"
        f"        {services['nico-pxe']} carbide-pxe.forge carbide-static-pxe.forge\n"
        "        fallthrough\n    }\n    # vm-hosts end\n"
    )
    corefile, matches = re.subn(
        r"(\.:53\s*\{)\n", lambda match: match[1] + aliases, corefile, count=1
    )
    if matches != 1:
        raise ValueError("unsupported CoreDNS root server configuration")
    if corefile != original:
        lab.run(
            "kubectl",
            "-n",
            "kube-system",
            "patch",
            "configmap",
            "coredns",
            "--type=merge",
            "-p",
            json.dumps({"data": {"Corefile": corefile}}),
        )
        lab.run(
            "kubectl", "-n", "kube-system", "rollout", "restart", "deployment/coredns"
        )
        lab.run(
            "kubectl",
            "-n",
            "kube-system",
            "rollout",
            "status",
            "deployment/coredns",
            "--timeout=120s",
            timeout=180,
        )

    # Use cluster DNS for service names in the boot URLs. Address allocation
    # remains exclusively in NICo's Kea hooks.
    config = json.loads(kubectl("get", "configmap", "nico-dhcp-config", "-o", "json"))
    kea = json.loads(config["data"]["kea_config.json"])
    for hook in kea["Dhcp4"]["hooks-libraries"]:
        parameters = hook.get("parameters", {})
        for key in ("nico-nameservers", "carbide-nameservers"):
            if key in parameters:
                parameters[key] = dns
    patch = json.dumps({"data": {"kea_config.json": json.dumps(kea)}})
    if json.loads(config["data"]["kea_config.json"]) != kea:
        kubectl("patch", "configmap", "nico-dhcp-config", "--type=merge", "-p", patch)
        kubectl("rollout", "restart", "deployment/nico-dhcp")
    kubectl("rollout", "status", "deployment/nico-dhcp", "--timeout=300s")
    kubectl("rollout", "status", "deployment/nico-dhcp6", "--timeout=300s")

    for suffix, args in (
        (
            "dhcp4",
            [
                "dhcrelay",
                "-4",
                "-d",
                "-i",
                lab.BRIDGE,
                "-i",
                bridge,
                services["nico-dhcp"],
            ],
        ),
        (
            "dhcp6",
            [
                "python3",
                "/usr/local/lib/nico-dhcp6-relay.py",
                "--interface",
                lab.BRIDGE,
                "--link-address",
                str(ipaddress.ip_network(lab.IPV6_SUBNET).network_address + 1),
                "--server",
                services["nico-dhcp6"],
                *[
                    arg
                    for host in inventory["targets"]
                    for arg in ("--mac", host["host_mac"])
                ],
            ],
        ),
    ):
        name = f"{lab.NAME}-{suffix}"
        existing = lab.inspect("container", name)
        if existing:
            lab.owned(existing, root)
            if existing["Config"]["Cmd"] != args:
                lab.run("docker", "stop", name)
                lab.run("docker", "rm", name)
        lab.container(
            root,
            name,
            lab.LIBVIRT_IMAGE,
            ["--network", "host", "--cap-add", "NET_RAW", "--cap-add", "NET_ADMIN"],
            args,
        )
    radvd = root / "radvd.conf"
    radvd.write_text(
        f"interface {lab.BRIDGE} {{ AdvSendAdvert on; AdvManagedFlag on; "
        f"AdvOtherConfigFlag on; prefix {lab.IPV6_SUBNET} {{ AdvOnLink on; AdvAutonomous off; }}; }};\n"
    )
    lab.container(
        root,
        f"{lab.NAME}-ra",
        lab.LIBVIRT_IMAGE,
        [
            "--network",
            "host",
            "--cap-add",
            "NET_RAW",
            "--cap-add",
            "NET_ADMIN",
            "-v",
            f"{radvd}:/etc/radvd.conf:ro",
        ],
        ["radvd", "-n", "-m", "stderr"],
    )

    lab.resume_paused(root)
    segments = json.loads(admin("-f", "json", "network-segment", "show"))[
        "network_segments"
    ]
    if not any(segment["id"] == SEGMENT_ID for segment in segments):
        domains = json.loads(admin("-f", "json", "domain", "show"))["domains"]
        domain_id = next(
            domain["id"] for domain in domains if domain["name"] == "local.nico"
        )
        admin(
            "--cloud-unsafe-op",
            "Initialize the isolated DevSpace infrastructure segment",
            "network-segment",
            "create",
            "--id",
            SEGMENT_ID,
            "--name",
            lab.NAME,
            "--segment-type",
            "host-inband",
            "--prefix",
            lab.IPV4_SUBNET,
            "--prefix",
            lab.IPV6_SUBNET,
            "--gateway",
            "10.84.30.1",
            "--reserve-first",
            "10",
            "--subdomain-id",
            domain_id,
        )
    registered = {machine["bmc_mac_address"].lower() for machine in machines}
    for target in inventory["targets"]:
        if target["bmc_mac"] in registered:
            continue
        interfaces = [
            dict(
                mac_address=target["host_mac"],
                role="host",
                primary=True,
                network_segment_type=3,
            ),
            dict(
                mac_address=target["bmc_mac"],
                role="host_bmc",
                ip_allocation="fixed",
                fixed_ip=target["bmc_ip"],
                network_segment_type=3,
            ),
        ]
        admin(
            "em",
            "add",
            "--bmc-mac-address",
            target["bmc_mac"],
            "--bmc-username",
            "root",
            "--bmc-password",
            "factory_password",
            "--chassis-serial-number",
            target["serial"],
            "--dpu-policy",
            "ignore",
            "--dpf-enabled",
            "false",
            "--meta-name",
            target["name"],
            "--interfaces",
            json.dumps(interfaces),
        )
    print(
        "Registered real zero-DPU targets; waiting for normal Scout ingestion through Core/REST."
    )


if __name__ == "__main__":
    main()
