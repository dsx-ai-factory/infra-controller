# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Create, connect, release and recreate two MAT instances with dual-stack networking."""

import argparse
import ipaddress
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import uuid
from urllib.parse import urlsplit

import packet_test

RUN_LABEL = "mat-network-lifecycle"
READY = "READY"
SYNCED = "SYNCED"


def ref(value):
    return {"value": value}


def ipv6_prefix(value):
    network = ipaddress.IPv6Network(value)
    if not 64 <= network.prefixlen <= 126:
        raise argparse.ArgumentTypeError(
            "IPv6 prefix must be /64 through /126 so capacity counters are exact"
        )
    return network


class Lifecycle:
    def __init__(self, args):
        self.args = args
        self.run_id = str(uuid.uuid4())
        self.vpc_id = str(uuid.uuid4())
        self.prefix_ids = [str(uuid.uuid4()), str(uuid.uuid4())]
        self.instance_ids = []
        self.result = {
            "result": "FAIL",
            "run_id": self.run_id,
            "generations": [],
            "cleanup_errors": [],
        }

    def rpc(self, method, request):
        response = subprocess.run(
            [str(self.args.rpc_command), method],
            input=json.dumps(request),
            text=True,
            capture_output=True,
            timeout=60,
        )
        if response.returncode:
            raise RuntimeError(f"{method} failed: {response.stderr.strip()}")
        return json.loads(response.stdout)

    def metadata(self, name):
        return {
            "name": f"mat-lifecycle-{self.run_id[:8]}-{name}",
            "labels": [{"key": RUN_LABEL, "value": self.run_id}],
        }

    def require_owned(self, resource):
        if {"key": RUN_LABEL, "value": self.run_id} not in resource.get(
            "metadata", {}
        ).get("labels", []):
            raise ValueError("resource ownership does not match this lifecycle run")

    def save(self):
        self.result.update(
            vpc_id=self.vpc_id,
            prefix_ids=self.prefix_ids,
            instance_ids=self.instance_ids,
        )
        self.args.output.joinpath("result.json").write_text(
            json.dumps(self.result, indent=2) + "\n"
        )

    def wait(self, description, check):
        print(description, flush=True)
        deadline = time.monotonic() + self.args.timeout
        while True:
            value = check()
            if value:
                return value
            if time.monotonic() >= deadline:
                raise TimeoutError(description)
            time.sleep(2)

    def instances(self, ids):
        return (
            self.rpc("FindInstancesByIds", {"instanceIds": [ref(i) for i in ids]}).get(
                "instances", []
            )
            if ids
            else []
        )

    def preflight(self):
        machines = packet_test.mat_request(self.args, "/machines/status")["machines"]
        simulated = {
            m.get("machine_id") for m in machines if m.get("device_kind") == "machine"
        }
        if not set(self.args.machine_id) <= simulated:
            raise ValueError(
                "both targets must be host machines registered by this MAT server"
            )
        if any(
            self.rpc("FindInstanceByMachineID", {"id": machine}).get("instances", [])
            for machine in self.args.machine_id
        ):
            raise ValueError(
                "a selected machine already has an instance; use unallocated MAT hosts"
            )

    def prefixes(self, include_deleted=False):
        return self.rpc(
            "GetVpcPrefixes",
            {
                "vpcPrefixIds": [ref(i) for i in self.prefix_ids],
                "deleted": (
                    "DELETED_FILTER_INCLUDE"
                    if include_deleted
                    else "DELETED_FILTER_EXCLUDE"
                ),
            },
        ).get("vpcPrefixes", [])

    def capacity_restored(self):
        prefixes = self.prefixes()
        if {p["id"]["value"] for p in prefixes} != set(self.prefix_ids):
            return False
        for prefix in prefixes:
            status = prefix.get("status", {})
            if status.get("tenantState") != READY:
                return False
            network = ipaddress.ip_network(prefix["config"]["prefix"])
            expected = 1 << (network.max_prefixlen - 1 - network.prefixlen)
            if int(status.get("availableLinknetSegments", 0)) != expected:
                return False
        return True

    def allocation_requests(self, ids, addresses=None):
        requests = []
        for index, (machine, instance) in enumerate(zip(self.args.machine_id, ids)):
            interface = {
                "functionType": "PHYSICAL",
                "vpcPrefixId": ref(self.prefix_ids[0]),
                "ipv6InterfaceConfig": {"vpcPrefixId": ref(self.prefix_ids[1])},
            }
            if addresses:
                interface["ipAddress"] = addresses[index][1]
                interface["ipv6InterfaceConfig"]["ipAddress"] = addresses[index][2]
            requests.append(
                {
                    "machineId": {"id": machine},
                    "instanceId": ref(instance),
                    "metadata": self.metadata(f"instance-{index}"),
                    "config": {
                        "tenant": {"tenantOrganizationId": self.args.tenant_org},
                        "os": {
                            "ipxe": {"ipxeScript": "#!ipxe\nexit\n"},
                            "phoneHomeEnabled": False,
                        },
                        "network": {"interfaces": [interface]},
                    },
                }
            )
        return requests

    def ready(self, ids):
        instances = self.instances(ids)
        if {i["id"]["value"] for i in instances} != set(ids):
            return False
        for instance in instances:
            self.require_owned(instance)
            status = instance.get("status", {})
            if (
                status.get("tenant", {}).get("state") != READY
                or status.get("network", {}).get("configsSynced", SYNCED) != SYNCED
            ):
                return False
        all_networks = packet_test.fetch_networks(self.args)
        networks = []
        for instance_id in ids:
            matches = [n for n in all_networks if n.get("instance_id") == instance_id]
            if len(matches) != 1:
                return False
            networks.append(packet_test.validate_network(matches[0]))
        packet_test.validate_pair(networks)
        for network in networks:
            for family, prefix in (
                (1, self.args.ipv4_prefix),
                (2, self.args.ipv6_prefix),
            ):
                if (
                    ipaddress.ip_address(packet_test.address(network, family)["ip"])
                    not in prefix
                ):
                    raise ValueError(
                        "MAT reported an address outside the requested tenant prefix"
                    )
        return networks

    def released(self, ids):
        if self.instances(ids):
            return False
        return not any(
            n.get("instance_id") in ids for n in packet_test.fetch_networks(self.args)
        )

    def release_instances(self, ids):
        for instance in self.instances(ids):
            self.require_owned(instance)
            if instance.get("status", {}).get("tenant", {}).get("state") not in (
                "TERMINATING",
                "TERMINATED",
            ):
                self.rpc("ReleaseInstance", {"id": instance["id"]})

    def packet_check(self, ids, generation):
        output = self.args.output / f"generation-{generation}"
        command = [
            sys.executable,
            str(Path(__file__).with_name("packet_test.py")),
            "--mat-url",
            self.args.mat_url,
            "--dhcp-server",
            str(self.args.dhcp_server),
            "--output",
            str(output),
            "--internet-url",
            self.args.internet_url,
            "--dns-upstream",
            str(self.args.dns_upstream),
        ]
        for instance in ids:
            command += ["--instance-id", instance]
        for flag in ("mat_ca", "nat44_interface", "nat66_interface"):
            if value := getattr(self.args, flag):
                command += ["--" + flag.replace("_", "-"), str(value)]
        if self.args.mat_insecure:
            command += ["--mat-insecure"]
        with self.args.output.joinpath(f"generation-{generation}.log").open("w") as log:
            child = None
            try:
                with packet_test.defer_interruptions():
                    child = subprocess.Popen(
                        command, stdout=log, stderr=subprocess.STDOUT
                    )
                code = child.wait(timeout=300)
                if code:
                    raise RuntimeError(f"packet test exited {code}; inspect {log.name}")
            finally:
                if child is not None and child.poll() is None:
                    # Let the packet harness remove namespaces, processes and rules.
                    child.terminate()
                    try:
                        child.wait(timeout=30)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait()
                        self.result["cleanup_errors"].append(
                            "packet harness required SIGKILL; inspect VM network fixtures"
                        )
        result = json.loads(output.joinpath("result.json").read_text())
        if result["result"] != "PASS" or result["cleanup_errors"]:
            raise RuntimeError("packet validation failed")

    def cleanup(self):
        try:
            self.release_instances(self.instance_ids)
            if self.instance_ids:
                self.wait(
                    "Waiting for test instances and MAT observations to disappear",
                    lambda: self.released(self.instance_ids),
                )
            for prefix in self.prefixes():
                self.require_owned(prefix)
                self.rpc("DeleteVpcPrefix", {"id": prefix["id"]})
            self.wait(
                "Waiting for test prefixes to disappear, including soft-deleted rows",
                lambda: not self.prefixes(include_deleted=True),
            )
            vpcs = self.rpc("FindVpcsByIds", {"vpcIds": [ref(self.vpc_id)]}).get(
                "vpcs", []
            )
            for vpc in vpcs:
                self.require_owned(vpc)
                self.rpc("DeleteVpc", {"id": vpc["id"]})
            self.wait(
                "Waiting for test VPC to disappear",
                lambda: not self.rpc(
                    "FindVpcsByIds", {"vpcIds": [ref(self.vpc_id)]}
                ).get("vpcs", []),
            )
        except Exception as error:
            self.result["cleanup_errors"].append(str(error))

    def execute(self):
        self.preflight()
        self.args.output.mkdir(parents=True, exist_ok=False)
        self.save()
        try:
            self.rpc(
                "CreateVpc",
                {
                    "id": ref(self.vpc_id),
                    "tenantOrganizationId": self.args.tenant_org,
                    "networkVirtualizationType": "FNN",
                    "slaacEnabled": False,
                    "metadata": self.metadata("vpc"),
                },
            )
            for index, prefix in enumerate(
                (self.args.ipv4_prefix, self.args.ipv6_prefix)
            ):
                self.rpc(
                    "CreateVpcPrefix",
                    {
                        "id": ref(self.prefix_ids[index]),
                        "vpcId": ref(self.vpc_id),
                        "config": {"prefix": str(prefix)},
                        "metadata": self.metadata(f"prefix-{index}"),
                    },
                )
            self.wait("Waiting for dual-stack VPC prefixes", self.capacity_restored)
            addresses = None
            for generation in (1, 2):
                ids = [str(uuid.uuid4()), str(uuid.uuid4())]
                self.instance_ids.extend(ids)
                self.save()  # Record IDs before an allocation whose reply could be lost.
                self.rpc(
                    "AllocateInstances",
                    {"instanceRequests": self.allocation_requests(ids, addresses)},
                )
                networks = self.wait(
                    f"Waiting for generation {generation} to become Ready and appear in MAT",
                    lambda: self.ready(ids),
                )
                allocated = [
                    {f: packet_test.address(n, f)["ip"] for f in (1, 2)}
                    for n in networks
                ]
                if addresses is not None and addresses != allocated:
                    raise ValueError(
                        "the recreated instances did not reuse their released addresses"
                    )
                addresses = allocated
                self.result["generations"].append(
                    {"instance_ids": ids, "addresses": allocated}
                )
                self.save()
                self.packet_check(ids, generation)
                self.result["generations"][-1]["packet_result"] = "PASS"
                self.save()
                self.release_instances(ids)
                self.wait(
                    f"Waiting for generation {generation} release in Core and MAT",
                    lambda: self.released(ids),
                )
                self.wait(
                    "Waiting for both prefix capacities to recover",
                    self.capacity_restored,
                )
                self.result["generations"][-1]["released_and_capacity_restored"] = True
                self.save()
            self.result["result"] = "PASS"
        except BaseException as error:
            self.result["error"] = str(error)
            raise
        finally:
            self.cleanup()
            if self.result["cleanup_errors"]:
                self.result["result"] = "FAIL"
            self.save()
        if self.result["result"] != "PASS":
            raise RuntimeError("lifecycle cleanup failed; inspect result.json")
        print(json.dumps(self.result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--rpc-command",
        type=Path,
        required=True,
        help="Absolute executable wrapper: method argument, ProtoJSON request on stdin, ProtoJSON response on stdout; 60-second call timeout.",
    )
    parser.add_argument(
        "--machine-id",
        action="append",
        required=True,
        help="Unallocated MAT host ID; specify exactly two distinct IDs.",
    )
    parser.add_argument(
        "--tenant-org",
        required=True,
        help="Existing tenant organization with an FNN routing profile.",
    )
    parser.add_argument(
        "--ipv4-prefix",
        type=ipaddress.IPv4Network,
        required=True,
        help="Available tenant CIDR, /30 or wider, admitted by the site's prefix policy.",
    )
    parser.add_argument(
        "--ipv6-prefix",
        type=ipv6_prefix,
        required=True,
        help="Available tenant CIDR, /64 through /126, admitted by the site's prefix policy; wider prefixes saturate capacity counters.",
    )
    parser.add_argument(
        "--mat-url", required=True, help="MAT HTTP(S) control API base URL."
    )
    tls = parser.add_mutually_exclusive_group()
    tls.add_argument(
        "--mat-ca",
        type=Path,
        help="PEM CA bundle for MAT HTTPS; defaults to system trust.",
    )
    tls.add_argument(
        "--mat-insecure",
        action="store_true",
        help="Disable MAT HTTPS certificate verification for a self-signed test endpoint.",
    )
    parser.add_argument(
        "--dhcp-server",
        type=Path,
        required=True,
        help="Absolute forge-dhcp-server binary path.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="New absolute evidence directory; must not exist.",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=1200,
        help="Positive timeout in seconds for each reconciliation or cleanup stage; default 1200.",
    )
    parser.add_argument(
        "--internet-url",
        required=True,
        help="HTTPS target reachable over both address families.",
    )
    parser.add_argument(
        "--nat44-interface",
        help="Optional explicit VM NAT44 egress interface; omitted uses existing routing/gateway.",
    )
    parser.add_argument(
        "--nat66-interface",
        help="Optional explicit VM NAT66 egress interface; omitted uses existing routing/gateway.",
    )
    parser.add_argument(
        "--dns-upstream",
        type=ipaddress.ip_address,
        default="127.0.0.53",
        help="Numeric DNS fixture upstream; default 127.0.0.53, UDP port 53.",
    )
    args = parser.parse_args()
    if len(args.machine_id) != 2 or len(set(args.machine_id)) != 2:
        parser.error("exactly two distinct MAT machine IDs are required")
    if args.ipv4_prefix.prefixlen > 30:
        parser.error("IPv4 prefix must provide at least two linknets (/30 or wider)")
    if not 0 < args.timeout < float("inf"):
        parser.error("--timeout must be finite and positive")
    for path in (args.rpc_command, args.dhcp_server, args.output):
        if not path.is_absolute():
            parser.error("RPC, DHCP binary, and output paths must be absolute")
    if (
        not args.rpc_command.is_file()
        or not os.access(args.rpc_command, os.X_OK)
        or not args.dhcp_server.is_file()
    ):
        parser.error("RPC wrapper must be executable and DHCP binary must exist")
    if os.geteuid() != 0 or sys.platform != "linux":
        parser.error("run on an isolated Linux test VM as root")
    if (
        urlsplit(args.internet_url).scheme != "https"
        or not urlsplit(args.internet_url).hostname
    ):
        parser.error("--internet-url must be an HTTPS URL")
    for tool in (
        "ip",
        "sysctl",
        "iptables",
        "ip6tables",
        "unshare",
        "mount",
        "ping",
        "curl",
    ):
        if not shutil.which(tool):
            parser.error(f"required program is missing: {tool}")
    for setting in ("net.ipv4.ip_forward", "net.ipv6.conf.all.forwarding"):
        if packet_test.run("sysctl", "-n", setting) != "1":
            parser.error(f"the test VM must already have {setting}=1")

    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"interrupted by signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    Lifecycle(args).execute()


if __name__ == "__main__":
    main()
