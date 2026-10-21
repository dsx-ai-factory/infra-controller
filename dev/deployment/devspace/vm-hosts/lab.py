#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Persistent native KVM target guests controlled by standalone bmc-mock endpoints."""

import argparse
import fcntl
import ipaddress
import json
import os
import platform
import subprocess
import sys
import tempfile
import time
import uuid
import xml.etree.ElementTree as ET
from pathlib import Path

NAME = "nico-vm-hosts"
BRIDGE = "br-nico-vm"
LABEL = "com.nvidia.devspace.vm-hosts"
IPV4_SUBNET = "10.84.30.0/24"
LIBVIRT_IMAGE = "nico-devspace-libvirt:latest"
BMC_IMAGE = "nico-devspace-bmc-mock:latest"


def run(*args, check=True, timeout=120):
    return subprocess.run(
        args, check=check, text=True, stdout=subprocess.PIPE, timeout=timeout
    ).stdout.strip()


def target(index):
    name = f"{NAME}-{index}"
    bmc_mac = f"02:00:00:{index:02x}:00:01"
    return dict(
        name=name,
        index=index,
        uuid=str(uuid.uuid5(uuid.NAMESPACE_DNS, name)),
        bmc_mac=bmc_mac,
        host_mac=f"02:00:00:{index:02x}:00:02",
        serial=bmc_mac.replace(":", ""),
        bmc_ip=f"10.84.30.{100 + index}",
    )


def native_architecture():
    arch = {"arm64": "aarch64", "amd64": "x86_64"}.get(
        platform.machine(), platform.machine()
    )
    if arch not in ("aarch64", "x86_64"):
        raise ValueError(f"unsupported native architecture: {arch}")
    return arch


def configuration(count, memory, cpus, disk):
    if (
        count not in (1, 2)
        or not 1024 <= memory <= 65536
        or not 1 <= cpus <= 32
        or not 8 <= disk <= 1024
    ):
        raise ValueError(
            "require 1–2 hosts, 1024–65536 MiB, 1–32 vCPUs, and 8–1024 GiB disks"
        )
    return dict(
        version=2,
        architecture=native_architecture(),
        memory_mib=memory,
        cpus=cpus,
        disk_gib=disk,
        targets=[target(index) for index in range(1, count + 1)],
    )


def requested_configuration(root, count, memory=None, cpus=None, disk=None):
    saved = read_inventory(root) if (root / "inventory.json").exists() else {}
    return configuration(
        count,
        memory if memory is not None else saved.get("memory_mib", 4096),
        cpus if cpus is not None else saved.get("cpus", 2),
        disk if disk is not None else saved.get("disk_gib", 24),
    )


def read_inventory(root):
    return json.loads((root / "inventory.json").read_text())


def save_inventory(root, config):
    path = root / "inventory.json"
    if path.exists():
        if read_inventory(root) != config:
            raise ValueError(
                "configuration differs from existing inventory; retain it or explicitly destroy the lab first"
            )
    else:
        with path.open("x") as stream:
            json.dump(config, stream, indent=2)
    return config


def grow_inventory(root, count):
    """Append targets without changing existing identities or resource sizes."""
    current = read_inventory(root)
    desired = configuration(
        count, current["memory_mib"], current["cpus"], current["disk_gib"]
    )
    previous_count = len(current["targets"])
    if count < previous_count or current != {
        **desired,
        "targets": desired["targets"][:previous_count],
    }:
        raise ValueError(
            "growth must preserve existing inventory and cannot remove targets"
        )
    if current != desired:
        with tempfile.NamedTemporaryFile(mode="w", dir=root, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                json.dump(desired, stream, indent=2)
                stream.flush()
                os.fsync(stream.fileno())
                temporary.replace(root / "inventory.json")
            finally:
                temporary.unlink(missing_ok=True)
    return desired


def domain_xml(host, root, memory, cpus, bridge, architecture=None):
    architecture = architecture or native_architecture()
    if architecture == "aarch64":
        machine, firmware = "virt", "/usr/share/AAVMF/AAVMF_{}.fd"
    elif architecture == "x86_64":
        machine, firmware = "q35", "/usr/share/OVMF/OVMF_{}_4M.fd"
    else:
        raise ValueError(f"unsupported guest architecture: {architecture}")
    domain = ET.Element("domain", type="kvm")

    def node(parent, tag, text=None, **attrs):
        result = ET.SubElement(parent, tag, attrs)
        result.text = str(text) if text is not None else None
        return result

    node(domain, "name", host["name"])
    node(domain, "uuid", host["uuid"])
    node(domain, "description", NAME)
    node(domain, "memory", memory, unit="MiB")
    node(domain, "vcpu", cpus)
    os_node = node(domain, "os")
    node(os_node, "type", "hvm", arch=architecture, machine=machine)
    node(
        os_node,
        "loader",
        firmware.format("CODE"),
        readonly="yes",
        type="pflash",
    )
    node(
        os_node,
        "nvram",
        str(root / "disks" / f"{host['name']}.fd"),
        template=firmware.format("VARS"),
    )
    node(os_node, "boot", dev="network")
    node(os_node, "boot", dev="hd")
    node(os_node, "smbios", mode="sysinfo")
    system = node(node(domain, "sysinfo", type="smbios"), "system")
    node(system, "entry", host["serial"], name="serial")
    node(system, "entry", host["uuid"], name="uuid")
    node(node(domain, "features"), "acpi")
    node(domain, "cpu", mode="host-passthrough")
    node(domain, "on_poweroff", "destroy")
    node(domain, "on_reboot", "restart")
    node(domain, "on_crash", "destroy")
    devices = node(domain, "devices")
    node(devices, "emulator", f"/usr/bin/qemu-system-{architecture}")
    disk = node(devices, "disk", type="file", device="disk")
    node(disk, "driver", name="qemu", type="qcow2")
    node(disk, "source", file=str(root / "disks" / f"{host['name']}.qcow2"))
    node(disk, "target", dev="vda", bus="virtio")
    nic = node(devices, "interface", type="bridge")
    node(nic, "mac", address=host["host_mac"])
    node(nic, "source", bridge=bridge)
    node(nic, "model", type="virtio")
    if architecture == "x86_64":
        node(nic, "rom", enabled="yes", file="/usr/lib/ipxe/qemu/efi-virtio.rom")
    else:
        node(nic, "rom", enabled="no")
    console = node(devices, "console", type="pty")
    node(
        console,
        "log",
        file=str(root / "logs" / f"{host['name']}-console.log"),
        append="on",
    )
    node(console, "target", type="serial", port="0")
    return ET.tostring(domain, encoding="unicode")


def inspect(kind, name):
    result = subprocess.run(
        ["docker", kind, "inspect", name], text=True, capture_output=True
    )
    if result.returncode:
        # Distinguish an absent resource from an unavailable Docker daemon.
        run("docker", "info", "--format", "{{.ServerVersion}}")
        return None
    return json.loads(result.stdout)[0]


def owned(resource, root, network=False):
    labels = (
        resource.get("Labels", {}) if network else resource["Config"].get("Labels", {})
    )
    if (labels or {}).get(LABEL) != str(root):
        raise ValueError(f"refusing to adopt resource not owned by {root}")


def container(root, name, image, options, command=()):
    existing = inspect("container", name)
    if existing:
        owned(existing, root)
        desired_image = run("docker", "image", "inspect", image, "--format", "{{.Id}}")
        if existing["Image"] != desired_image:
            raise ValueError(f"{name} image changed; stop the lab before redeploying")
        if not existing["State"]["Running"]:
            run("docker", "start", name)
        return
    run(
        "docker",
        "run",
        "-d",
        "--name",
        name,
        "--label",
        f"{LABEL}={root}",
        "--restart",
        "unless-stopped",
        *options,
        image,
        *command,
    )


def virsh(*args):
    return run("docker", "exec", NAME, "virsh", "-c", "qemu:///system", *args)


def preflight():
    if platform.system() != "Linux" or native_architecture() not in (
        "aarch64",
        "x86_64",
    ):
        raise ValueError(
            "vm-hosts requires a native ARM64 or x86_64 Linux Docker host with KVM"
        )
    if not Path("/dev/kvm").exists():
        raise ValueError(
            "/dev/kvm is absent; enable nested virtualization on the outer VM"
        )
    run("docker", "info", "--format", "{{.Architecture}}")
    for image in (LIBVIRT_IMAGE, BMC_IMAGE):
        run("docker", "image", "inspect", image)


def validate_routes(routes, cidr):
    subnet = ipaddress.ip_network(cidr)
    for route in routes:
        if route["dst"] != "default" and subnet.overlaps(
            ipaddress.ip_network(route["dst"], strict=False)
        ):
            raise ValueError(f"{cidr} overlaps an existing route: {route['dst']}")


def up(root, config):
    preflight()
    save_inventory(root, config)
    for directory in ("disks", "logs", "domains", "run", "bmc", "libvirt"):
        (root / directory).mkdir(exist_ok=True)
    network = inspect("network", NAME)
    if network:
        owned(network, root, network=True)
    else:
        for version, cidr in ((4, IPV4_SUBNET),):
            existing_routes = json.loads(
                run("ip", f"-{version}", "-json", "route", "show")
            )
            validate_routes(existing_routes, cidr)
        run(
            "docker",
            "network",
            "create",
            "--driver",
            "bridge",
            "--subnet",
            IPV4_SUBNET,
            "--opt",
            f"com.docker.network.bridge.name={BRIDGE}",
            "--opt",
            "com.docker.network.bridge.enable_ip_masquerade=false",
            "--label",
            f"{LABEL}={root}",
            NAME,
        )
    container(
        root,
        NAME,
        LIBVIRT_IMAGE,
        [
            "--init",
            "--privileged",
            "--network",
            "host",
            "-v",
            f"{root}:{root}",
            "-v",
            f"{root}/run:/run/libvirt",
            "-v",
            f"{root}/domains:/etc/libvirt/qemu",
            "-v",
            f"{root}/libvirt:/var/lib/libvirt",
        ],
    )
    for attempt in range(30):
        try:
            virsh("list", "--all")
            break
        except subprocess.CalledProcessError:
            if attempt == 29:
                raise
            time.sleep(1)
    defined = virsh("list", "--all", "--name").splitlines()
    saved = virsh("list", "--all", "--with-managed-save", "--name").splitlines()
    for host in config["targets"]:
        disk = root / "disks" / f"{host['name']}.qcow2"
        if not disk.exists():
            run(
                "docker",
                "exec",
                NAME,
                "qemu-img",
                "create",
                "-f",
                "qcow2",
                str(disk),
                f"{config['disk_gib']}G",
            )
        if host["name"] not in defined:
            xml = root / f"{host['name']}.xml"
            xml.write_text(
                domain_xml(
                    host,
                    root,
                    config["memory_mib"],
                    config["cpus"],
                    BRIDGE,
                    config["architecture"],
                )
            )
            virsh("define", str(xml))
        elif virsh("domuuid", host["name"]) != host["uuid"]:
            raise ValueError(f"unexpected domain identity: {host['name']}")
        if host["name"] in saved:
            virsh("start", host["name"], "--paused")
        state_dir = root / "bmc" / host["name"]
        state_dir.mkdir(exist_ok=True)
        container(
            root,
            f"{host['name']}-bmc",
            BMC_IMAGE,
            [
                "--network",
                NAME,
                "--ip",
                host["bmc_ip"],
                "--mac-address",
                host["bmc_mac"],
                "-v",
                f"{root}/run:/run/libvirt",
                "-v",
                f"{state_dir}:/state",
            ],
            [
                "--machine-role",
                "host",
                "--hardware-profile",
                "generic_ami",
                "--dpu-count",
                "0",
                "--instance-index",
                str(host["index"]),
                "--port",
                "443",
                "--redfish-auth",
                "--state-backend",
                "libvirt",
                "--libvirt-domain",
                host["name"],
                "--state-file",
                "/state/bmc.json",
            ],
        )
    print(json.dumps(config, indent=2))


def resume_paused(root):
    config = read_inventory(root)
    paused = virsh("list", "--all", "--state-paused", "--name").splitlines()
    for host in config["targets"]:
        if host["name"] not in paused:
            continue
        if virsh("domuuid", host["name"]) != host["uuid"]:
            raise ValueError(f"unexpected domain identity: {host['name']}")
        virsh("resume", host["name"])


def network_rules(bridge, uplink):
    for family, subnet in (("iptables", IPV4_SUBNET),):
        # Docker drops direct traffic to unpublished container addresses before
        # FORWARD. Allow only the local kind bridge to reach the lab BMCs.
        yield (
            "host",
            family,
            "raw",
            "PREROUTING",
            [
                "-i",
                bridge,
                "-d",
                subnet,
                "-m",
                "comment",
                "--comment",
                NAME,
                "-j",
                "ACCEPT",
            ],
        )
        for incoming, outgoing in (
            (BRIDGE, bridge),
            (bridge, BRIDGE),
            (BRIDGE, uplink),
        ):
            yield (
                "host",
                family,
                "filter",
                "FORWARD",
                [
                    "-i",
                    incoming,
                    "-o",
                    outgoing,
                    "-m",
                    "comment",
                    "--comment",
                    NAME,
                    "-j",
                    "ACCEPT",
                ],
            )
        yield (
            "host",
            family,
            "filter",
            "FORWARD",
            [
                "-i",
                uplink,
                "-o",
                BRIDGE,
                "-m",
                "conntrack",
                "--ctstate",
                "ESTABLISHED,RELATED",
                "-m",
                "comment",
                "--comment",
                NAME,
                "-j",
                "ACCEPT",
            ],
        )
        yield (
            "node",
            family,
            "nat",
            "POSTROUTING",
            ["-s", subnet, "-m", "comment", "--comment", NAME, "-j", "ACCEPT"],
        )

    yield (
        "host",
        "iptables",
        "nat",
        "POSTROUTING",
        [
            "-s",
            IPV4_SUBNET,
            "-o",
            uplink,
            "-m",
            "comment",
            "--comment",
            NAME,
            "-j",
            "MASQUERADE",
        ],
    )


def cleanup_network(root):
    path = root / "network-state.json"
    if not path.exists():
        return
    state = json.loads(path.read_text())
    for where, family, table, chain, rule in network_rules(
        state["bridge"], state["uplink"]
    ):
        container_name = NAME if where == "host" else state["node"]
        resource = inspect("container", container_name)
        if resource is None:
            continue
        if not resource["State"]["Running"]:
            raise ValueError(
                f"{container_name} must be running to remove the lab network rules"
            )
        prefix = ["docker", "exec", container_name, family, "-w", "-t", table]
        # Report absence inside the container so a Docker transport failure
        # cannot be mistaken for iptables' exit status for a missing rule.
        exists = run(
            "docker",
            "exec",
            container_name,
            "sh",
            "-c",
            'if "$@"; then printf present; else code=$?; '
            '[ "$code" -eq 1 ] || exit "$code"; printf absent; fi',
            "sh",
            family,
            "-w",
            "-t",
            table,
            "-C",
            chain,
            *rule,
        )
        if exists == "present":
            run(*prefix, "-D", chain, *rule)
        elif exists != "absent":
            raise ValueError(f"unexpected rule check result: {exists!r}")
    for version, prefix, gateway in state["routes"]:
        routes = json.loads(
            run(
                "docker",
                "exec",
                NAME,
                "ip",
                f"-{version}",
                "-json",
                "route",
                "show",
                "exact",
                prefix,
            )
        )
        if any(route.get("gateway") == gateway for route in routes):
            run(
                "docker",
                "exec",
                NAME,
                "ip",
                f"-{version}",
                "route",
                "del",
                prefix,
                "via",
                gateway,
            )
    path.unlink()


def stop(root):
    config = read_inventory(root)
    hypervisor = inspect("container", NAME)
    if hypervisor:
        owned(hypervisor, root)
    # Stop BMC writers first. Remove containers, retaining all domain/disk/BMC state.
    names = [f"{host['name']}-bmc" for host in config["targets"]]
    for name in names:
        resource = inspect("container", name)
        if resource:
            owned(resource, root)
            run("docker", "stop", name)
            run("docker", "rm", name)
    if hypervisor:
        if hypervisor["State"]["Running"]:
            active = virsh("list", "--name").splitlines()
            for host in config["targets"]:
                if host["name"] in active:
                    virsh("managedsave", host["name"])
    cleanup_network(root)
    relay = f"{NAME}-dhcp4"
    resource = inspect("container", relay)
    if resource:
        owned(resource, root)
        run("docker", "stop", relay)
        run("docker", "rm", relay)
    if hypervisor:
        run("docker", "stop", NAME)
        run("docker", "rm", NAME)


def destroy(root):
    # Validate ownership of every container before stopping anything.
    config = read_inventory(root)
    names = [NAME] + [f"{host['name']}-bmc" for host in config["targets"]]
    names.append(f"{NAME}-dhcp4")
    for name in names:
        resource = inspect("container", name)
        if resource:
            owned(resource, root)
    network = inspect("network", NAME)
    if network:
        owned(network, root, network=True)
        if any(
            endpoint["Name"] not in names
            for endpoint in network.get("Containers", {}).values()
        ):
            raise ValueError(
                "lab network has unrelated endpoints; refusing destruction"
            )
    stop(root)
    if network:
        run("docker", "network", "rm", NAME)
    # Files created by root inside the runtime are removed through that same
    # runtime. The bind mount limits deletion to the explicitly selected lab.
    run(
        "docker",
        "run",
        "--rm",
        "-v",
        f"{root}:/lab",
        LIBVIRT_IMAGE,
        "sh",
        "-ec",
        "rm -rf /lab/disks /lab/logs /lab/domains /lab/run /lab/bmc /lab/libvirt",
    )
    for path in root.iterdir():
        if (
            path.name in ("inventory.json", "cluster-uid", "routes.json")
            or path.name.startswith(NAME + "-")
            and path.suffix == ".xml"
        ):
            path.unlink()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(
            os.environ.get(
                "LOCAL_DEV_VM_HOSTS_DIR",
                str(Path.home() / ".local/share/nico-vm-hosts"),
            )
        ),
    )
    commands = parser.add_subparsers(dest="command", required=True)
    create = commands.add_parser(
        "up",
        help="create or reconcile persistent targets; leave new domains powered off",
    )
    create.add_argument("--count", type=int, default=2)
    create.add_argument(
        "--memory-mib",
        type=int,
        help="new lab default: 4096; otherwise retain saved size",
    )
    create.add_argument(
        "--cpus", type=int, help="new lab default: 2; otherwise retain saved size"
    )
    create.add_argument(
        "--disk-gib", type=int, help="new lab default: 24; otherwise retain saved size"
    )
    grow = commands.add_parser(
        "grow", help="add targets while preserving existing guest state"
    )
    grow.add_argument("--count", type=int, required=True)
    commands.add_parser(
        "check-purge", help="refuse cluster reset while target inventory is retained"
    )
    commands.add_parser(
        "status", help="show saved inventory and observed domain states"
    )
    commands.add_parser(
        "stop",
        help="save running guests and remove containers, preserving disks and state",
    )
    delete = commands.add_parser(
        "destroy", help="delete this lab, including target disks and BMC state"
    )
    delete.add_argument("--delete-disks", required=True, action="store_true")
    args = parser.parse_args()
    root = args.root.expanduser().resolve()
    root.mkdir(parents=True, exist_ok=True, mode=0o700)
    with (root / ".lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        if args.command == "check-purge":
            if (root / "inventory.json").exists():
                raise ValueError(
                    "release instances and explicitly destroy the target lab before purging kind; retained disks and BMC credentials must stay paired with site state"
                )
        elif args.command == "up":
            up(
                root,
                requested_configuration(
                    root, args.count, args.memory_mib, args.cpus, args.disk_gib
                ),
            )
        elif args.command == "grow":
            preflight()
            up(root, grow_inventory(root, args.count))
        elif args.command == "stop":
            stop(root)
        elif args.command == "destroy":
            destroy(root)
        else:
            print(json.dumps(read_inventory(root), indent=2))
            print(virsh("list", "--all"))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
