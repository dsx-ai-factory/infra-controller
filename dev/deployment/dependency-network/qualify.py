#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise NICo's dependency clients against disposable dual-stack services."""

import argparse
import json
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid


DNS_IMAGE = "coredns/coredns:1.12.1@sha256:e8c262566636e6bc340ece6473b0eed193cad045384401529721ddbe6463d31c"
OTEL_IMAGE = "otel/opentelemetry-collector:0.123.0@sha256:e5e4a13f0ea98e7ca1d1d809be040180540146888d0d764abd4e1277cba87350"
ADDRESSES = {"postgres": 10, "vault": 11, "otel": 12, "dns": 53}
PASSWORD = "disposable-qualification-password"
TOKEN = "disposable-qualification-token"


def run(*args, **kwargs):
    return subprocess.run(args, check=kwargs.pop("check", True), text=True, capture_output=True,
                          timeout=kwargs.pop("timeout", 60), **kwargs)


def docker(*args, **kwargs):
    return run("docker", *args, **kwargs)


def address(service, family):
    suffix = ADDRESSES[service]
    return f"172.30.54.{suffix}" if family == 4 else f"fd00:5403::{suffix}"


def authority(host):
    return f"[{host}]" if ":" in host else host


def write_config(path, contents):
    path.write_text(contents)
    path.chmod(0o644)


def certificate(directory):
    run("openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(directory / "ca.key"), "-out", str(directory / "ca.crt"),
        "-days", "2", "-subj", "/CN=NICo disposable qualification CA")
    run("openssl", "req", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(directory / "server.key"), "-out", str(directory / "server.csr"),
        "-subj", "/CN=vault.dependency.test")
    sans = [f"DNS:{service}.dependency.test" for service in ("postgres", "vault")]
    sans += [f"IP:{address(service, family)}" for service in ("postgres", "vault")
             for family in (4, 6)]
    (directory / "extensions").write_text("subjectAltName=" + ",".join(sans) + "\n")
    run("openssl", "x509", "-req", "-in", str(directory / "server.csr"),
        "-CA", str(directory / "ca.crt"), "-CAkey", str(directory / "ca.key"),
        "-CAcreateserial", "-out", str(directory / "server.crt"), "-days", "2",
        "-extfile", str(directory / "extensions"))
    for name in ("ca.crt", "server.crt"):
        (directory / name).chmod(0o644)


def wait_for_listener(host, port):
    deadline = time.monotonic() + 45
    while True:
        try:
            with socket.create_connection((host, port), timeout=1):
                return
        except OSError:
            if time.monotonic() >= deadline:
                raise TimeoutError(f"dependency did not listen on {host}:{port}")
            time.sleep(0.2)


def cleanup(names, output):
    failures = []
    for name in reversed(names):
        try:
            logs = docker("logs", name, check=False)
            (output / (name + ".log")).write_text(logs.stdout + logs.stderr)
        except Exception as error:
            print(f"Could not collect {name} logs: {error}", file=sys.stderr)
        try:
            docker("rm", "-f", name)
        except Exception as error:
            failures.append(f"{name}: {error}")
    if failures:
        raise RuntimeError("Could not remove qualification containers: " + "; ".join(failures))


def qualify(args):
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    network = "nico-dependency-" + uuid.uuid4().hex[:10]
    results = []
    # Each row changes an observable resolution or connection boundary.
    cases = [
        ("ipv4-literal", "literal", (4,), 4),
        ("ipv6-literal", "literal", (6,), 6),
        ("aaaa-only", "dns", (6,), 6),
        ("ipv6-only-network", "ipv6-only", (6,), 6),
        ("dual-stack", "dual", (4, 6), None),
        ("dual-ipv4-refused", "dual", (6,), 6),
        ("dual-ipv6-refused", "dual", (4,), 4),
        ("missing-dns", "missing", (4, 6), None),
    ]
    image_info = json.loads(docker("image", "inspect", args.test_image).stdout)[0]
    metadata = {
        "test_image": image_info["Id"],
        "source_revision": image_info["Config"].get("Labels", {}).get("org.opencontainers.image.revision"),
        "dns_image": DNS_IMAGE,
        "collector_image": OTEL_IMAGE,
        "docker": docker("version", "--format", "{{.Server.Version}}").stdout.strip(),
    }
    (output / "environment.json").write_text(json.dumps(metadata, indent=2) + "\n")
    (output / "versions.txt").write_text(docker("run", "--rm", args.test_image,
                                               "cat", "/qualification/versions.txt").stdout)
    network_created = False
    try:
        with tempfile.TemporaryDirectory(prefix="nico-dependency-") as temporary:
            root = Path(temporary)
            certs = root / "certs"
            certs.mkdir()
            certs.chmod(0o755)
            certificate(certs)
            for label, naming, listening, expected_family in cases:
                print(f"Qualifying {label}", flush=True)
                ipv6_only = naming == "ipv6-only"
                network_args = ["--ipv4=false"] if ipv6_only else ["--subnet", "172.30.54.0/24"]
                docker("network", "create", "--ipv6", *network_args,
                       "--subnet", "fd00:5403::/64", network)
                network_created = True
                config = root / label
                config.mkdir()
                config.chmod(0o755)
                names = []
                case_output = output / label
                case_output.mkdir()

                def start(service, image, *command):
                    name = network + "-" + service
                    ipv4_args = [] if ipv6_only else ["--ip", address(service, 4)]
                    docker("create", "--name", name, "--network", network,
                           *ipv4_args, "--ip6", address(service, 6),
                           "--mount", f"type=bind,src={certs},dst=/certs,readonly",
                           "--mount", f"type=bind,src={config},dst=/config,readonly",
                           image, *command)
                    names.append(name)
                    docker("start", name)
                    return name

                def endpoint(service):
                    return (address(service, expected_family) if naming == "literal"
                            else ("missing-" if naming == "missing" else "") + service + ".dependency.test")

                try:
                    dns_families = (4, 6) if naming == "dual" else listening
                    write_config(config / "hosts", "".join(
                        f"{address(service, family)} {service}.dependency.test\n"
                        for service in ("postgres", "vault", "otel") for family in dns_families))
                    write_config(config / "Corefile",
                        "dependency.test:53 {\n log\n errors\n hosts /config/hosts {\n"
                        "  ttl 1\n  no_reverse\n }\n}\n")
                    dns = start("dns", DNS_IMAGE, "-conf", "/config/Corefile")
                    dns_address = address("dns", 6 if ipv6_only else 4)
                    write_config(config / "resolv.conf",
                                 f"nameserver {dns_address}\noptions timeout:1 attempts:1\n")
                    if ipv6_only:
                        resolver_args = ["--mount", f"type=bind,src={config / 'resolv.conf'},dst=/etc/resolv.conf,readonly"]
                    else:
                        resolver_args = ["--dns", dns_address, "--dns-option", "timeout:1",
                                         "--dns-option", "attempts:1"]
                    wait_for_listener(dns_address, 53)
                    postgres_bind = ",".join(address("postgres", f) for f in listening)
                    pg_script = f"""
set -eu
install -d -o postgres /tmp/qualification-pg
printf '%s' '{PASSWORD}' > /tmp/qualification-password
chmod 644 /tmp/qualification-password
runuser -u postgres -- /usr/lib/postgresql/15/bin/initdb -D /tmp/qualification-pg --username=qualification --pwfile=/tmp/qualification-password --auth-host=scram-sha-256 --auth-local=trust >/dev/null
printf '%s\\n' 'hostssl all qualification 172.30.54.0/24 scram-sha-256' 'hostssl all qualification fd00:5403::/64 scram-sha-256' >> /tmp/qualification-pg/pg_hba.conf
install -o postgres -m 600 /certs/server.key /tmp/qualification-pg/server.key
exec runuser -u postgres -- /usr/lib/postgresql/15/bin/postgres -D /tmp/qualification-pg -c listen_addresses='{postgres_bind}' -c ssl=on -c ssl_cert_file=/certs/server.crt -c ssl_key_file=/tmp/qualification-pg/server.key
"""
                    start("postgres", args.test_image, "bash", "-c", pg_script)
                    vault_listeners = [{"tcp": {
                        "address": authority(address("vault", family)) + ":8200",
                        "tls_cert_file": "/certs/server.crt", "tls_key_file": "/certs/server.key",
                    }} for family in listening]
                    vault_admin = "https://" + authority(address("vault", listening[0])) + ":8200"
                    write_config(config / "vault.json", json.dumps({
                        "api_addr": vault_admin, "listener": vault_listeners,
                    }))
                    vault = start("vault", args.test_image, "vault", "server", "-dev",
                                  "-dev-no-store-token", "-dev-root-token-id=" + TOKEN,
                                  "-dev-listen-address=127.0.0.1:0", "-config=/config/vault.json")
                    otel_bind = "[::]" if len(listening) == 2 else authority(address("otel", listening[0]))
                    write_config(config / "otel.yaml", f"""
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: '{otel_bind}:4317'
exporters:
  debug:
    verbosity: detailed
service:
  pipelines:
    traces:
      receivers: [otlp]
      exporters: [debug]
""")
                    collector = start("otel", OTEL_IMAGE, "--config=/config/otel.yaml")
                    for service, port in (("postgres", 5432), ("vault", 8200), ("otel", 4317)):
                        wait_for_listener(address(service, listening[0]), port)
                    docker("exec", "-e", "VAULT_ADDR=" + vault_admin, "-e", "VAULT_TOKEN=" + TOKEN,
                           "-e", "VAULT_CACERT=/certs/ca.crt", vault,
                           "vault", "audit", "enable", "file", "file_path=/tmp/audit.json")
                    span = "nico-dependency-" + label + "-" + uuid.uuid4().hex
                    environment = {
                        "NICO_TEST_POSTGRES_URL": "postgres://qualification:" + PASSWORD + "@"
                        + authority(endpoint("postgres")) + ":5432/postgres?sslmode=verify-full&sslrootcert=/certs/ca.crt",
                        "NICO_TEST_VAULT_URL": "https://" + authority(endpoint("vault")) + ":8200",
                        "NICO_TEST_VAULT_TOKEN": TOKEN,
                        "NICO_TEST_VAULT_CA": "/certs/ca.crt",
                        "NICO_TEST_VAULT_MOUNT": "secret",
                        "NICO_TEST_OTLP_ENDPOINT": "http://" + authority(endpoint("otel")) + ":4317",
                        "NICO_TEST_OTLP_SPAN_NAME": span,
                    }
                    if expected_family:
                        environment["NICO_TEST_POSTGRES_FAMILY"] = str(expected_family)
                    env_args = [item for key, value in environment.items() for item in ("-e", key + "=" + value)]
                    commands = [
                        ("postgres", "/qualification/carbide", "postgres::tests::postgres_url_supports_query", []),
                        ("vault", "/qualification/carbide_secrets", "forge_vault::tests::credentials_round_trip_through_external_vault", ["--ignored"]),
                        ("otel", "/qualification/carbide", "logging::tests::tracing_exporter_reaches_external_collector", ["--ignored"]),
                    ]
                    network_info = json.loads(docker("network", "inspect", network).stdout)[0]
                    if ipv6_only and network_info["EnableIPv4"]:
                        raise RuntimeError("qualification network unexpectedly enables IPv4")
                    (case_output / "network.json").write_text(json.dumps(network_info, indent=2) + "\n")
                    for service, binary, test, extra in commands:
                        runner = network + "-test-" + service
                        docker("create", "--name", runner, "--network", network,
                               *resolver_args,
                               "--mount", f"type=bind,src={certs},dst=/certs,readonly",
                               *env_args, args.test_image, binary, test,
                               "--exact", "--nocapture", *extra)
                        names.append(runner)
                        result = docker("start", "-a", runner, timeout=35, check=False)
                        test_log = result.stdout + result.stderr
                        (case_output / (service + "-test.log")).write_text(test_log)
                        resolver_output = case_output / (service + "-resolv.conf")
                        docker("cp", runner + ":/etc/resolv.conf", str(resolver_output))
                        if ipv6_only and resolver_output.read_text() != (config / "resolv.conf").read_text():
                            raise RuntimeError(f"{service} did not use the direct IPv6 resolver")
                        if naming == "missing":
                            if (not result.returncode or "0 passed; 1 failed" not in result.stdout
                                    or "failed to lookup address information" not in test_log):
                                raise RuntimeError(f"{service} did not reject an unresolvable name")
                            continue
                        if result.returncode or "1 passed" not in result.stdout:
                            raise RuntimeError(f"{service} qualification failed:\n{result.stdout}{result.stderr}")
                        if (service == "postgres"
                                and "PostgreSQL exchange succeeded over address family Some(" not in test_log):
                            raise RuntimeError("PostgreSQL test did not complete a network query")
                    if naming == "missing" or ipv6_only:
                        dns_result = docker("logs", dns)
                        dns_lines = (dns_result.stdout + dns_result.stderr).splitlines()
                        for service, _, _, _ in commands:
                            if not any(f" IN {endpoint(service)}. " in line
                                       and (not ipv6_only or "fd00:5403::" in line) for line in dns_lines):
                                raise RuntimeError(f"DNS did not record the expected {service} query")
                    collector_logs = docker("logs", collector).stderr
                    if (span in collector_logs) != (naming != "missing"):
                        raise RuntimeError("collector did not record the exported qualification span")
                    audit = docker("exec", vault, "cat", "/tmp/audit.json").stdout
                    (case_output / "vault-audit.jsonl").write_text(audit)
                    peers = sorted({event["request"]["remote_address"] for line in audit.splitlines()
                                    if (event := json.loads(line)).get("request", {}).get("path", "").startswith("secret/data/machines/bmc/")})
                    if bool(peers) != (naming != "missing"):
                        raise RuntimeError("Vault did not audit the credential requests")
                    if expected_family and any((6 if ":" in peer else 4) != expected_family for peer in peers):
                        raise RuntimeError(f"unexpected Vault connection family: {peers}")
                    results.append({"case": label, "passed": True,
                                    "expected_outcome": "DNS failure" if naming == "missing" else "exchange",
                                    "vault_peers": peers, "span": span})
                except BaseException as error:
                    results.append({"case": label, "passed": False, "error": str(error)})
                    raise
                finally:
                    try:
                        (output / "results.json").write_text(json.dumps(results, indent=2) + "\n")
                    finally:
                        cleanup(names, case_output)
                docker("network", "rm", network)
                network_created = False
    finally:
        if network_created:
            active_error = sys.exc_info()[1]
            try:
                docker("network", "rm", network)
            except Exception as error:
                if active_error is None:
                    raise
                print(f"Could not remove qualification network: {error}", file=sys.stderr)
    print(f"Passed {len(results)} dependency network cases; evidence: {output}")


def main():
    def terminate(signum, _frame):
        raise SystemExit(128 + signum)

    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, terminate)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test-image", default="nico-dependency-tests")
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory")
    args = parser.parse_args()
    qualify(args)


if __name__ == "__main__":
    main()
