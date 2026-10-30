#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Validate IPv6-only networking inside an installed tenant guest; emit JSON."""

import argparse
import ipaddress
import json
import socket
import subprocess


def dns_matches(output, expected):
    try:
        addresses = [
            ipaddress.ip_address(value) for value in output.split(":", 1)[1].split()
        ]
    except (IndexError, ValueError):
        return False
    return ipaddress.IPv6Address(expected) in addresses and all(
        address.version == 6 for address in addresses
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--address",
        required=True,
        type=ipaddress.IPv6Address,
        help="expected guest IPv6 address",
    )
    parser.add_argument(
        "--peer",
        required=True,
        type=ipaddress.IPv6Address,
        help="other installed guest IPv6 address",
    )
    parser.add_argument(
        "--dns",
        required=True,
        type=ipaddress.IPv6Address,
        help="expected upstream DNS IPv6 address",
    )
    parser.add_argument(
        "--interface", required=True, help="tenant network interface name"
    )
    args = parser.parse_args()
    if args.address == args.peer:
        parser.error("peer must be a different guest address")
    for address in (args.address, args.peer, args.dns):
        if address.is_unspecified or address.is_multicast or address.is_loopback:
            parser.error("require unicast non-loopback IPv6 addresses")
    OWN, PEER, DNS = str(args.address), str(args.peer), str(args.dns)
    rows = []

    def run(name, argv, check=lambda r: r.returncode == 0):
        r = subprocess.run(argv, capture_output=True, text=True, timeout=40)
        ok = check(r)
        rows.append(
            {
                "check": name,
                "passed": ok,
                "returncode": r.returncode,
                "stdout": r.stdout,
                "stderr": r.stderr,
            }
        )
        return r

    r = run("addresses", ["ip", "-j", "address"])
    a = json.loads(r.stdout)
    if any(
        ipaddress.IPv6Address(address["local"]) == args.peer
        for interface in a
        for address in interface["addr_info"]
        if address["family"] == "inet6"
    ):
        parser.error("peer is configured on this guest; require a different host")
    nonloop = [x for v in a if v["ifname"] != "lo" for x in v["addr_info"]]
    rows.append(
        {
            "check": "no-ipv4-address",
            "passed": not any(x["family"] == "inet" for x in nonloop),
        }
    )
    rows.append(
        {
            "check": "expected-ipv6",
            "passed": any(
                x["family"] == "inet6" and x["local"] == OWN for x in nonloop
            ),
        }
    )
    r = run(
        "no-ipv4-routes",
        ["ip", "-j", "-4", "route", "show", "table", "all"],
        lambda r: r.returncode == 0
        and all(
            x.get("dev") == "lo"
            and ipaddress.ip_network(x.get("dst", "0.0.0.0/0"), strict=False).subnet_of(
                ipaddress.ip_network("127.0.0.0/8")
            )
            for x in json.loads(r.stdout)
        ),
    )
    run("ipv6-routes", ["ip", "-6", "route"])
    r = run(
        "dns",
        ["resolvectl", "dns", args.interface],
        lambda r: r.returncode == 0 and dns_matches(r.stdout, DNS),
    )
    run("flush-dns-cache", ["sudo", "-n", "resolvectl", "flush-caches"])
    run("resolve-aaaa", ["resolvectl", "query", "--type=AAAA", "www.google.com"])
    run(
        "internet-ipv6",
        [
            "curl",
            "-q",
            "--noproxy",
            "*",
            "-6",
            "--interface",
            OWN,
            "--fail",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "15",
            "--max-time",
            "30",
            "-o",
            "/dev/null",
            "-w",
            "status=%{http_code} local=%{local_ip} remote=%{remote_ip} ssl_verify=%{ssl_verify_result}",
            "https://www.google.com/",
        ],
        lambda r: r.returncode == 0
        and "status=200 " in r.stdout
        and "local=" + OWN + " " in r.stdout
        and r.stdout.endswith("ssl_verify=0"),
    )
    run(
        "internet-normal-selection",
        [
            "curl",
            "-q",
            "--noproxy",
            "*",
            "--fail",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "15",
            "--max-time",
            "30",
            "-o",
            "/dev/null",
            "-w",
            "status=%{http_code} local=%{local_ip} remote=%{remote_ip} ssl_verify=%{ssl_verify_result}",
            "https://www.google.com/",
        ],
        lambda r: r.returncode == 0
        and "status=200 " in r.stdout
        and "local=" + OWN + " " in r.stdout
        and r.stdout.endswith("ssl_verify=0"),
    )
    run(
        "ipv4-negative-control",
        [
            "curl",
            "-q",
            "--noproxy",
            "*",
            "-4",
            "--connect-timeout",
            "3",
            "--max-time",
            "5",
            "http://1.1.1.1/",
        ],
        lambda r: r.returncode in (7, 28),
    )
    run("peer-ipv6-icmp", ["ping", "-6", "-c", "3", "-W", "3", PEER])
    try:
        with socket.create_connection((PEER, 22), timeout=10) as s:
            banner = s.recv(256).decode()
        rows.append(
            {
                "check": "peer-ipv6-ssh",
                "passed": banner.startswith("SSH-2.0-"),
                "banner": banner,
            }
        )
    except Exception as e:
        rows.append({"check": "peer-ipv6-ssh", "passed": False, "error": str(e)})
    r = run(
        "cloud-init",
        ["cloud-init", "status", "--format=json"],
        lambda r: r.returncode in (0, 2),
    )
    try:
        status = json.loads(r.stdout)
    except json.JSONDecodeError:
        status = None
    status_valid = isinstance(status, dict)
    rows[-1]["passed"] = (
        rows[-1]["passed"] and status_valid and status.get("status") == "done"
    )
    if not status_valid:
        rows[-1]["error"] = "cloud-init status did not return a JSON object"

    def scan(value, key):
        if isinstance(value, dict):
            for k, v in value.items():
                if k == key and v:
                    yield v
                yield from scan(v, key)
        elif isinstance(value, list):
            for v in value:
                yield from scan(v, key)

    errors = list(scan(status, "errors"))
    warnings = list(scan(status, "recoverable_errors"))
    rows.append(
        {
            "check": "no-cloud-init-errors",
            "passed": status_valid and not errors,
            "errors": errors,
        }
    )
    # Custom network is validated separately by NICo, but this PXE revision also
    # returns it in user-data. Keep the raw schema result for explicit review.
    run(
        "cloud-init-schema",
        ["sudo", "-n", "cloud-init", "schema", "--system"],
        lambda r: r.returncode == 0,
    )
    rows.append(
        {
            "check": "no-unreviewed-cloud-init-warnings",
            "passed": status_valid and not warnings,
            "warnings": warnings,
        }
    )
    print(json.dumps(rows, indent=2))
    raise SystemExit(0 if all(x["passed"] for x in rows) else 1)


if __name__ == "__main__":
    main()
