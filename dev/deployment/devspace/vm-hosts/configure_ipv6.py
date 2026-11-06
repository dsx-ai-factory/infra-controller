#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Add IPv6 provisioning endpoints to an existing local vm-hosts deployment."""

import argparse
import copy
import hashlib
import ipaddress
import json
import os
import re
from pathlib import Path

import lab

OWNER = {"experiment": "issue-84-ipv6-lifecycle"}
ALIASES = {
    "nico-api": "carbide-api.forge",
    "nico-pxe": "carbide-pxe.forge carbide-static-pxe.forge",
}


def companion(service):
    return {
        "apiVersion": "v1",
        "kind": "Service",
        "metadata": {
            "name": service["metadata"]["name"] + "-ipv6-lab",
            "namespace": service["metadata"]["namespace"],
            "labels": OWNER,
        },
        "spec": {
            "ipFamilies": ["IPv6"],
            "ipFamilyPolicy": "SingleStack",
            "selector": service["spec"]["selector"],
            "ports": [
                {key: value for key, value in port.items() if key != "nodePort"}
                for port in service["spec"]["ports"]
            ],
        },
    }


def dns_config(text, addresses):
    pattern = r"(?ms)^    # vm-hosts begin\n(.*?)^    # vm-hosts end\n"
    blocks = list(re.finditer(pattern, text))
    if len(blocks) != 1 or blocks[0][1].count("    hosts {\n") != 1:
        raise ValueError(
            "require one vm-hosts DNS block; run the vm-hosts profile first"
        )
    block = blocks[0][1]
    lines = []
    for line in block.splitlines(keepends=True):
        parts = line.split()
        if len(parts) > 1 and " ".join(parts[1:]) in ALIASES.values():
            if ipaddress.ip_address(parts[0]).version == 6:
                continue
        lines.append(line)
    block = "".join(lines).replace(
        "    hosts {\n",
        "    hosts {\n"
        + "".join(
            f"        {ipaddress.IPv6Address(addresses[name])} {aliases}\n"
            for name, aliases in ALIASES.items()
        ),
        1,
    )
    return text[: blocks[0].start(1)] + block + text[blocks[0].end(1) :]


def dhcp_config(config, addresses):
    result = copy.deepcopy(config)
    hooks = [
        hook
        for hook in result["Dhcp6"]["hooks-libraries"]
        if hook["library"].endswith("/libdhcp.so")
    ]
    if len(hooks) != 1:
        raise ValueError("require exactly one NICo DHCPv6 hook")
    parameters = hooks[0]["parameters"]
    parameters["hook-dns-servers-ipv6"] = str(
        ipaddress.IPv6Address(addresses["kube-dns"])
    )
    parameters["hook-provisioning-server-ipv6"] = str(
        ipaddress.IPv6Address(addresses["nico-pxe"])
    )
    return result


def add_route(state_path, prefix, gateway):
    routes = json.loads(lab.run("ip", "-6", "-j", "route", "show", "exact", prefix))
    if routes:
        if any(route.get("gateway") != gateway for route in routes):
            raise ValueError(f"refusing to replace unrelated route for {prefix}")
        return
    state = json.loads(state_path.read_text())
    owned = ["6", prefix, gateway]
    if owned not in state["routes"]:
        state["routes"].append(owned)
        state_path.write_text(json.dumps(state))
    lab.run("sudo", "ip", "-6", "route", "add", prefix, "via", gateway)


def apply_config(namespace, name, deployment, key, original, updated):
    if original != updated:
        lab.run(
            "kubectl",
            "-n",
            namespace,
            "patch",
            "configmap",
            name,
            "--type=merge",
            "-p",
            json.dumps({"data": {key: updated}}),
        )
    # Reconcile the pod template even after an interrupted ConfigMap update.
    digest = hashlib.sha256(updated.encode()).hexdigest()
    lab.run(
        "kubectl",
        "-n",
        namespace,
        "patch",
        "deployment",
        deployment,
        "--type=merge",
        "-p",
        json.dumps(
            {
                "spec": {
                    "template": {
                        "metadata": {
                            "annotations": {
                                "devspace.nvidia.com/ipv6-provisioning-sha256": digest
                            }
                        }
                    }
                }
            }
        ),
    )
    lab.run(
        "kubectl",
        "-n",
        namespace,
        "rollout",
        "status",
        "deployment/" + deployment,
        "--timeout=180s",
        timeout=210,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="snapshot/result directory; retain it with the lab",
    )
    args = parser.parse_args()
    root = Path(
        os.environ.get(
            "LOCAL_DEV_VM_HOSTS_DIR", str(Path.home() / ".local/share/nico-vm-hosts")
        )
    ).resolve()
    namespace = os.environ.get("LOCAL_DEV_NAMESPACE", "nico-system")
    lab.read_inventory(root)
    context = lab.run("kubectl", "config", "current-context")
    if not context.startswith("kind-"):
        raise ValueError("IPv6 lab setup requires a local kind context")
    nodes = lab.run("kind", "get", "nodes", "--name", context[5:]).splitlines()
    if len(nodes) != 1:
        raise ValueError("IPv6 lab setup requires a single kind node")

    def get(ns, kind, name):
        return json.loads(lab.run("kubectl", "-n", ns, "get", kind, name, "-o", "json"))

    cluster_uid = get("kube-system", "namespace", "kube-system")["metadata"]["uid"]
    if (root / "cluster-uid").read_text() != cluster_uid:
        raise ValueError("vm-hosts inventory belongs to a different cluster")
    node_ip = lab.inspect("container", nodes[0])["NetworkSettings"]["Networks"]["kind"][
        "GlobalIPv6Address"
    ]
    ipaddress.IPv6Address(node_ip)
    state_path = root / "network-state.json"
    if json.loads(state_path.read_text())["node"] != nodes[0]:
        raise ValueError("network state belongs to a different kind node")
    args.output.mkdir(parents=True, exist_ok=True)
    identity = args.output / "cluster-uid"
    if identity.exists() and identity.read_text() != cluster_uid:
        raise ValueError("snapshot directory belongs to a different cluster")
    identity.write_text(cluster_uid)

    def save(name, value):
        path = args.output / name
        if not path.exists():
            path.write_text(json.dumps(value, indent=2) + "\n")

    services = {}
    for ns, name in (
        (namespace, "nico-api"),
        (namespace, "nico-pxe"),
        ("kube-system", "kube-dns"),
    ):
        original = get(ns, "service", name)
        save("original-" + name + ".json", original)
        desired = companion(original)
        companion_name = desired["metadata"]["name"]
        existing = json.loads(
            lab.run("kubectl", "-n", ns, "get", "services", "-o", "json")
        )["items"]
        found = next(
            (s for s in existing if s["metadata"]["name"] == companion_name), None
        )
        if found and any(
            found["metadata"].get("labels", {}).get(k) != v for k, v in OWNER.items()
        ):
            raise ValueError(f"refusing to adopt unrelated service {companion_name}")
        path = args.output / (companion_name + ".json")
        path.write_text(json.dumps(desired, indent=2) + "\n")
        lab.run("kubectl", "apply", "-f", str(path))
        services[name] = get(ns, "service", companion_name)["spec"]["clusterIP"]
        ipaddress.IPv6Address(services[name])
        prefix = services[name] + "/128"
        add_route(state_path, prefix, node_ip)

    dns = get("kube-system", "configmap", "coredns")
    dhcp = get(namespace, "configmap", "nico-dhcp6-config")
    save("original-coredns.json", dns)
    save("original-dhcp6.json", dhcp)
    corefile = dns_config(dns["data"]["Corefile"], services)
    kea = dhcp_config(json.loads(dhcp["data"]["kea_config6.json"]), services)
    for ns, name, deployment, key, original, updated in (
        (
            "kube-system",
            "coredns",
            "coredns",
            "Corefile",
            dns["data"]["Corefile"],
            corefile,
        ),
        (
            namespace,
            "nico-dhcp6-config",
            "nico-dhcp6",
            "kea_config6.json",
            json.dumps(json.loads(dhcp["data"]["kea_config6.json"])),
            json.dumps(kea),
        ),
    ):
        apply_config(ns, name, deployment, key, original, updated)
    (args.output / "ipv6-services.json").write_text(
        json.dumps(services, indent=2) + "\n"
    )
    print(json.dumps(services, indent=2))


if __name__ == "__main__":
    main()
