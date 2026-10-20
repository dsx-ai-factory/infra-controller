# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Bounded dual-stack software packet test of two live MAT tenant configurations; no HBN/EVPN emulation."""

import argparse
from contextlib import contextmanager
import datetime
import fcntl
import ipaddress
import json
import os
from pathlib import Path
import signal
import ssl
import subprocess
import sys
import time
import uuid
from urllib.parse import urlsplit
from urllib.request import HTTPSHandler, ProxyHandler, build_opener

HERE = Path(__file__).resolve().parent
ROUTER = "fe80::1"
PROBE_NAME = "peer.mat.test"


def validate_network(data, max_age=120):
    """Validate a live MAT observation before making privileged changes."""
    if data.get("schema_version") != 1:
        raise ValueError("unsupported tenant-network schema")
    age = (
        datetime.datetime.now(datetime.timezone.utc)
        - datetime.datetime.fromisoformat(data["updated_at"])
    ).total_seconds()
    if not 0 <= age <= max_age:
        raise ValueError("stale or future-dated MAT tenant-network observation")
    uuid.UUID(data["instance_id"])
    if data["network_virtualization_type"] != 5:
        raise ValueError("only FNN is supported")
    interfaces = data["tenant_interfaces"]
    if len(interfaces) != 1:
        raise ValueError("exactly one tenant interface is required")
    interface = interfaces[0]
    if (
        interface["function_type"] != 0
        or interface["is_l2_segment"]
        or interface["has_network_security_group"]
    ):
        raise ValueError("only physical routed interfaces without NSGs are supported")
    addresses = interface["addresses"]
    if sorted(a["address_family"] for a in addresses) != [1, 2]:
        raise ValueError("exactly one canonical address per family is required")
    for family, bits in [(1, 32), (2, 128)]:
        address = next(a for a in addresses if a["address_family"] == family)
        ip = ipaddress.ip_address(address["ip"])
        prefix = ipaddress.ip_network(address["prefix"])
        binding = ipaddress.ip_network(address["interface_prefix"])
        if (
            ip.max_prefixlen != bits
            or prefix.version != ip.version
            or prefix.prefixlen != bits - 1
            or binding != ipaddress.ip_network(f"{ip}/{bits}")
            or ip not in prefix
            or ip == prefix.network_address
            or address.get("svi_ip")
        ):
            raise ValueError(
                "only stateful /31 and /127 linknets with /32 and /128 host bindings are supported"
            )
        if family == 1 and ipaddress.ip_interface(
            address["gateway"]
        ) != ipaddress.ip_interface(f"{prefix.network_address}/{prefix.prefixlen}"):
            raise ValueError("IPv4 gateway must match the linknet")
    return data


def mat_request(args, path):
    url = urlsplit(args.mat_url)
    if (
        url.scheme not in ("http", "https")
        or not url.hostname
        or url.username
        or url.password
        or url.query
        or url.fragment
    ):
        raise ValueError(
            "--mat-url must be an HTTP(S) control API base URL without credentials, query or fragment"
        )
    if url.scheme != "https" and (args.mat_ca or args.mat_insecure):
        raise ValueError("MAT TLS options require an HTTPS --mat-url")
    context = ssl.create_default_context(cafile=args.mat_ca)
    if args.mat_insecure:
        context.check_hostname = False
        context.verify_mode = ssl.CERT_NONE
    opener = build_opener(ProxyHandler({}), HTTPSHandler(context=context))
    with opener.open(args.mat_url.rstrip("/") + path, timeout=10) as response:
        return json.load(response)


def fetch_networks(args):
    data = mat_request(args, "/machines/tenant-networks")
    if not isinstance(data, list) or any(not isinstance(item, dict) for item in data):
        raise ValueError("MAT tenant-network API must return an array of observations")
    return data


def load_networks(args):
    data = fetch_networks(args)
    result = []
    for instance_id in args.instance_id:
        matches = [item for item in data if item.get("instance_id") == str(instance_id)]
        if len(matches) != 1:
            raise ValueError(
                f"expected one active primary DPU for instance {instance_id}; got {len(matches)}"
            )
        result.append(validate_network(matches[0]))
    return result


def fingerprint(data):
    return {k: v for k, v in data.items() if k != "updated_at"}


def validate_pair(configs):
    if len({c["instance_id"] for c in configs}) != 2:
        raise ValueError("two distinct instances are required")
    interfaces = [c["tenant_interfaces"][0] for c in configs]
    if (
        not interfaces[0]["vpc_vni"]
        or interfaces[0]["vpc_vni"] != interfaces[1]["vpc_vni"]
    ):
        raise ValueError("both endpoints must belong to the same VPC")
    for family in (1, 2):
        prefixes = [
            ipaddress.ip_network(
                next(
                    a["prefix"] for a in i["addresses"] if a["address_family"] == family
                )
            )
            for i in interfaces
        ]
        if prefixes[0].overlaps(prefixes[1]):
            raise ValueError("endpoint linknets must not overlap")


def run(*args, timeout=35):
    try:
        return subprocess.check_output(
            [str(a) for a in args], text=True, stderr=subprocess.STDOUT, timeout=timeout
        ).strip()
    except subprocess.CalledProcessError as error:
        raise RuntimeError(f"{error}: {error.output}") from error


def address(config, family):
    return next(
        a
        for a in config["tenant_interfaces"][0]["addresses"]
        if a["address_family"] == family
    )


def write_lifetime(path, value):
    temporary = path.with_suffix(".next")
    temporary.write_text(str(value))
    temporary.replace(path)


def wait_route(namespace, present, family=6, gateway=ROUTER):
    for _ in range(30):
        routes = json.loads(
            run(
                "ip",
                "netns",
                "exec",
                namespace,
                "ip",
                "-j",
                f"-{family}",
                "route",
                "show",
                "default",
            )
        )
        if present and any(
            r.get("gateway") == gateway and (family == 4 or r.get("protocol") == "ra")
            for r in routes
        ):
            return routes
        if not present and not routes:
            return routes
        time.sleep(0.5)
    raise RuntimeError(f"default route expectation {present} failed: {routes}")


@contextmanager
def defer_interruptions():
    # Queue Python signal handlers during fork/exec and ownership registration.
    # Unlike blocking the signal mask, this leaves exec'd children interruptible.
    pending = []
    previous = {}
    try:
        for signum in (signal.SIGINT, signal.SIGTERM):
            previous[signum] = signal.signal(
                signum, lambda received, frame: pending.append(received)
            )
        yield
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
        if pending:
            signal.raise_signal(pending[0])


class Lab:
    def __init__(self, output):
        self.output = output
        self.undo = []
        self.processes = []
        self.logs = []

    def change(self, command, undo):
        # Every rollback names this run's resources (or removes a newly created empty directory).
        # Record it before the kernel mutation can take effect and be interrupted.
        self.undo.append(undo)
        run(*command)

    def launch(self, command, log):
        stream = open(self.output / log, "w")
        self.logs.append(stream)
        with defer_interruptions():
            child = subprocess.Popen(
                [str(a) for a in command], stdout=stream, stderr=subprocess.STDOUT
            )
            self.processes.append(child)

    def cleanup(self):
        errors = []
        for child in reversed(self.processes):
            child.terminate()
        for child in self.processes:
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        for command in reversed(self.undo):
            try:
                run(*command)
            except Exception as error:
                errors.append(str(error))
        for stream in self.logs:
            stream.close()
        return errors


def execute(args):
    configs = load_networks(args)
    validate_pair(configs)
    if os.geteuid() != 0 or sys.platform != "linux":
        raise ValueError("run on an isolated Linux test VM as root")
    for setting in ("net.ipv4.ip_forward", "net.ipv6.conf.all.forwarding"):
        if run("sysctl", "-n", setting) != "1":
            raise ValueError(f"the test VM must already have {setting}=1")
    if args.internet_url and (
        urlsplit(args.internet_url).scheme != "https"
        or not urlsplit(args.internet_url).hostname
    ):
        raise ValueError("--internet-url must be an HTTPS URL")
    if (args.nat44_interface or args.nat66_interface) and not args.internet_url:
        raise ValueError("NAT interfaces require --internet-url")
    if not args.dhcp_server.is_file():
        raise ValueError(
            "--dhcp-server must name the compiled forge-dhcp-server binary"
        )
    for config in configs:
        for family in (1, 2):
            a = address(config, family)
            if run(
                "ip",
                "-4" if family == 1 else "-6",
                "route",
                "show",
                "exact",
                a["prefix"],
            ):
                raise ValueError(f'linknet already in use: {a["prefix"]}')
    args.output.mkdir(parents=True, exist_ok=False)
    lab = Lab(args.output)
    result = {
        "result": "FAIL",
        "scope": "real NICo DHCPv4/DHCPv6; software RA, DNS and Linux routing; no HBN/EVPN or isolation proof",
        "nat44_interface": args.nat44_interface,
        "nat66_interface": args.nat66_interface,
        "endpoints": [],
        "checks": [],
    }
    token = uuid.uuid4().hex[:8]
    tag = "mat6-" + token
    try:
        fixture = args.output / "http"
        fixture.mkdir()
        (fixture / "probe.txt").write_text("mat-dual-stack-ok\n")
        for directory in [
            Path("/var/support"),
            Path("/var/support/forge-dhcp"),
            Path("/var/support/forge-dhcp/logs"),
            Path("/etc/netns"),
        ]:
            if not directory.exists():
                lab.change(["mkdir", directory], ["rmdir", directory])
        for index, config in enumerate(configs):
            v4, v6 = address(config, 1), address(config, 2)
            gateway4 = str(ipaddress.ip_interface(v4["gateway"]).ip)
            gateway6 = str(ipaddress.ip_network(v6["prefix"]).network_address)
            namespace = f"{tag}-{index}"
            root, peer = f"m{token}{index}", f"p{token}{index}"
            lab.change(
                ["ip", "netns", "add", namespace], ["ip", "netns", "delete", namespace]
            )
            lab.change(
                ["ip", "link", "add", root, "type", "veth", "peer", "name", peer],
                ["ip", "link", "delete", root],
            )
            run("ip", "link", "set", peer, "netns", namespace)
            run(
                "ip",
                "netns",
                "exec",
                namespace,
                "ip",
                "link",
                "set",
                peer,
                "name",
                "eth0",
            )
            run("ip", "netns", "exec", namespace, "ip", "link", "set", "lo", "up")
            run("ip", "link", "set", root, "mtu", "1280", "up")
            run(
                "ip",
                "netns",
                "exec",
                namespace,
                "ip",
                "link",
                "set",
                "eth0",
                "mtu",
                "1280",
                "up",
            )
            run("ip", "addr", "add", v4["gateway"], "dev", root)
            run("ip", "-6", "addr", "add", ROUTER + "/64", "dev", root, "nodad")
            run("ip", "-6", "addr", "add", gateway6 + "/127", "dev", root, "nodad")
            run(
                "ip",
                "netns",
                "exec",
                namespace,
                "sysctl",
                "-qw",
                "net.ipv6.conf.all.forwarding=0",
                "net.ipv6.conf.eth0.accept_ra=1",
            )
            for tool, ip, bits, egress in (
                ("iptables", v4["ip"], 32, args.nat44_interface),
                ("ip6tables", v6["ip"], 128, args.nat66_interface),
            ):
                for direction in ("-i", "-o"):
                    rule = [
                        "FORWARD",
                        direction,
                        root,
                        "-m",
                        "comment",
                        "--comment",
                        tag,
                        "-j",
                        "ACCEPT",
                    ]
                    lab.change([tool, "-I", *rule], [tool, "-D", *rule])
                if egress:
                    rule = [
                        "POSTROUTING",
                        "-s",
                        f"{ip}/{bits}",
                        "-o",
                        egress,
                        "-m",
                        "comment",
                        "--comment",
                        tag,
                        "-j",
                        "MASQUERADE",
                    ]
                    lab.change(
                        [tool, "-t", "nat", "-A", *rule],
                        [tool, "-t", "nat", "-D", *rule],
                    )
            control = args.output / f"ra-{index}.lifetime"
            write_lifetime(control, 30)
            lab.launch(
                [
                    "python3",
                    "-u",
                    HERE / "router_advertisement.py",
                    root,
                    v6["prefix"],
                    control,
                ],
                f"ra-{index}.log",
            )
            for family, gateway in ((4, gateway4), (6, gateway6)):
                lab.launch(
                    [
                        "python3",
                        "-u",
                        HERE / "dns_fixture.py",
                        gateway,
                        address(configs[1 - index], 1)["ip"],
                        address(configs[1 - index], 2)["ip"],
                        args.dns_upstream,
                    ],
                    f"dns-{index}-v{family}.log",
                )
            dhcp = {
                "lease_time_secs": 7200,
                "renewal_time_secs": 1800,
                "rebinding_time_secs": 3600,
                "carbide_nameservers": [gateway4],
                "carbide_nameservers_v6": [gateway6],
                "carbide_ntpservers": [],
                "carbide_api_url": None,
                "carbide_provisioning_server_ipv4": v4["gateway"].split("/")[0],
                "carbide_dhcp_server": v4["gateway"].split("/")[0],
                "dhcpv6_preferred_lifetime_secs": 3600,
                "dhcpv6_valid_lifetime_secs": 7200,
            }
            host = {
                "host_interface_id": config["host_interface_id"],
                "host_ip_addresses": {
                    root: {
                        "address": v4["ip"],
                        "gateway": v4["gateway"].split("/")[0],
                        "prefix": v4["prefix"],
                        "fqdn": "mat.test",
                        "booturl": None,
                        "mtu": 1280,
                        "ipv6": {"address": v6["ip"], "prefix": v6["prefix"]},
                    }
                },
            }
            for name, data in [("dhcp", dhcp), ("host", host)]:
                (args.output / f"{name}-{index}.json").write_text(json.dumps(data))
            state = args.output / f"dhcp-state-{index}"
            state.mkdir()
            lab.launch(
                [
                    "unshare",
                    "--mount",
                    "--propagation",
                    "private",
                    "sh",
                    "-c",
                    'mount --bind "$1" /var/support/forge-dhcp/logs; shift; exec "$@"',
                    "_",
                    state,
                    args.dhcp_server,
                    "--mode",
                    "dpu",
                    "--interfaces",
                    root,
                    "--dhcp-config",
                    args.output / f"dhcp-{index}.json",
                    "--host-config",
                    args.output / f"host-{index}.json",
                ],
                f"dhcp-{index}.log",
            )
            result["endpoints"].append(
                {
                    "instance_id": config["instance_id"],
                    "namespace": namespace,
                    "families": {
                        "4": {
                            "address": v4["ip"],
                            "dns": gateway4,
                            "gateway": gateway4,
                        },
                        "6": {"address": v6["ip"], "dns": gateway6, "gateway": ROUTER},
                    },
                }
            )
        time.sleep(3)
        for index, ep in enumerate(result["endpoints"]):
            namespace = ep["namespace"]
            for family in (4, 6):
                endpoint = ep["families"][str(family)]
                client = "dhcpv4_client.py" if family == 4 else "dhcp_client.py"
                command = [
                    "ip",
                    "netns",
                    "exec",
                    namespace,
                    "python3",
                    HERE / client,
                    "eth0",
                    endpoint["address"],
                ]
                if family == 4:
                    command += [
                        address(configs[index], 1)["prefix"],
                        endpoint["gateway"],
                        endpoint["dns"],
                    ]
                endpoint["lease"] = json.loads(run(*command))
                if endpoint["lease"]["dns_servers"] != [endpoint["dns"]]:
                    raise RuntimeError(
                        f"DHCPv{family} DNS does not match the advertised configuration"
                    )
                endpoint["routes"] = wait_route(
                    namespace, True, family, endpoint["gateway"]
                )
                lab.launch(
                    [
                        "ip",
                        "netns",
                        "exec",
                        namespace,
                        "python3",
                        "-m",
                        "http.server",
                        "18084",
                        "--bind",
                        endpoint["address"],
                        "--directory",
                        fixture,
                    ],
                    f"http-{index}-v{family}.log",
                )
            netns_dir = Path("/etc/netns") / namespace
            lab.change(["mkdir", netns_dir], ["rmdir", netns_dir])
            for name in ("resolv.conf", "nsswitch.conf"):
                path = netns_dir / name
                lab.undo.append(["rm", "-f", path])
                path.write_text("hosts: files dns\n" if name == "nsswitch.conf" else "")
        time.sleep(1)
        for index, ep in enumerate(result["endpoints"]):
            for family in (4, 6):
                endpoint = ep["families"][str(family)]
                ns = ["ip", "netns", "exec", ep["namespace"]]
                peer = result["endpoints"][1 - index]["families"][str(family)][
                    "address"
                ]
                # Each family must resolve through its own DHCP-advertised DNS server.
                (Path("/etc/netns") / ep["namespace"] / "resolv.conf").write_text(
                    f'nameserver {endpoint["lease"]["dns_servers"][0]}\noptions timeout:2 attempts:2\n'
                )
                check = {
                    "instance_id": ep["instance_id"],
                    "family": family,
                    "dns_server": endpoint["lease"]["dns_servers"][0],
                    "ping": run(*ns, "ping", f"-{family}", "-c", "3", "-W", "3", peer),
                }
                check["peer_http"] = run(
                    *ns,
                    "curl",
                    f"-{family}",
                    "--noproxy",
                    "*",
                    "--max-time",
                    "10",
                    "--fail",
                    "-sS",
                    f"http://{PROBE_NAME}:18084/probe.txt",
                )
                if check["peer_http"] != "mat-dual-stack-ok":
                    raise RuntimeError("unexpected peer HTTP response")
                if args.internet_url:
                    check["internet_https"] = run(
                        *ns,
                        "curl",
                        f"-{family}",
                        "--noproxy",
                        "*",
                        "--max-time",
                        "25",
                        "--fail",
                        "-sS",
                        "-o",
                        "/dev/null",
                        "-w",
                        "%{local_ip} %{remote_ip} %{http_code}",
                        args.internet_url,
                    )
                control = args.output / f"ra-{index}.lifetime"
                if family == 6:
                    write_lifetime(control, 0)
                else:
                    run(*ns, "ip", "-4", "route", "del", "default")
                check["withdrawn_routes"] = wait_route(
                    ep["namespace"], False, family, endpoint["gateway"]
                )
                failed = subprocess.run(
                    [*ns, "ping", f"-{family}", "-c", "1", "-W", "1", peer],
                    capture_output=True,
                    text=True,
                    timeout=5,
                )
                if failed.returncode == 0:
                    raise RuntimeError(
                        "peer remained reachable without its default route"
                    )
                check["withdrawal_failure"] = failed.stdout + failed.stderr
                other = 6 if family == 4 else 4
                other_peer = result["endpoints"][1 - index]["families"][str(other)][
                    "address"
                ]
                check["other_family_during_withdrawal"] = run(
                    *ns, "ping", f"-{other}", "-c", "1", "-W", "3", other_peer
                )
                if family == 6:
                    write_lifetime(control, 30)
                else:
                    run(
                        *ns,
                        "ip",
                        "-4",
                        "route",
                        "add",
                        "default",
                        "via",
                        endpoint["lease"]["gateway"],
                        "dev",
                        "eth0",
                    )
                check["restored_routes"] = wait_route(
                    ep["namespace"], True, family, endpoint["gateway"]
                )
                check["recovery"] = run(
                    *ns, "ping", f"-{family}", "-c", "1", "-W", "3", peer
                )
                result["checks"].append(check)
        for current, original in zip(load_networks(args), configs):
            if fingerprint(current) != fingerprint(original):
                raise RuntimeError(
                    "MAT configuration changed during the test; rerun against the current API state"
                )
        result["result"] = "PASS"
    except BaseException as error:
        result["error"] = str(error)
        raise
    finally:
        result["cleanup_errors"] = lab.cleanup()
        if result["cleanup_errors"]:
            result["result"] = "FAIL"
        (args.output / "result.json").write_text(json.dumps(result, indent=2))
    if result["result"] != "PASS":
        raise RuntimeError("cleanup failed; inspect result.json")
    print(json.dumps(result, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--mat-url",
        required=True,
        help="HTTP(S) base URL of the MAT control API; fetched before and after testing (10-second request timeout).",
    )
    parser.add_argument(
        "--instance-id",
        type=uuid.UUID,
        action="append",
        required=True,
        help="Instance UUID to test; specify exactly two distinct instances with observations at most 120 seconds old.",
    )
    tls = parser.add_mutually_exclusive_group()
    tls.add_argument(
        "--mat-ca",
        type=Path,
        help="PEM CA bundle for HTTPS MAT control access; omitted uses system trust.",
    )
    tls.add_argument(
        "--mat-insecure",
        action="store_true",
        help="Explicitly disable certificate verification for a self-signed MAT test endpoint; HTTPS only.",
    )
    parser.add_argument(
        "--dhcp-server",
        type=Path,
        required=True,
        help="Absolute path to the forge-dhcp-server binary built from the revision under test.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        required=True,
        help="New absolute directory for result.json and logs; must not exist.",
    )
    parser.add_argument(
        "--internet-url",
        help="Optional HTTPS target; omitted runs only local cross-endpoint checks.",
    )
    parser.add_argument(
        "--nat44-interface",
        help="Optional VM egress interface for scoped NAT44; requires --internet-url. Omitted leaves egress routing unchanged.",
    )
    parser.add_argument(
        "--nat66-interface",
        help="Optional VM egress interface for scoped NAT66; requires --internet-url. Omitted leaves egress routing unchanged.",
    )
    parser.add_argument(
        "--dns-upstream",
        default="127.0.0.53",
        type=ipaddress.ip_address,
        help="Numeric VM DNS upstream used for non-fixture names; default 127.0.0.53, UDP port 53.",
    )
    args = parser.parse_args()
    if (
        len(args.instance_id) != 2
        or len(set(args.instance_id)) != 2
        or not args.output.is_absolute()
        or not args.dhcp_server.is_absolute()
    ):
        parser.error(
            "exactly two distinct --instance-id values and absolute --output/--dhcp-server paths are required"
        )

    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"interrupted by signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    # Serialize packet tests; overlapping tenant pools cannot share the VM routing table.
    with open("/run/lock/mat-ipv6-packet-test.lock", "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        execute(args)


if __name__ == "__main__":
    main()
