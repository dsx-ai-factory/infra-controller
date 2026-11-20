#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Verify local MAT discovery, Scout responsiveness and optional REST synchronization."""

import argparse
import base64
from datetime import datetime, timezone
from email.parser import Parser
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


class NotReady(Exception):
    """An observation that must be retried before the deadline."""


def mat_host_ids(status):
    devices = status["machines"]
    if not isinstance(devices, list) or any("device_kind" not in d for d in devices):
        raise NotReady("MAT returned invalid device status")
    hosts = [d for d in devices if d["device_kind"] == "machine"]
    ids = [host.get("machine_id") for host in hosts]
    if not ids or any(not isinstance(i, str) or not i for i in ids) or len(set(ids)) != len(ids):
        raise NotReady("MAT hosts do not all have distinct Core identities yet")
    return set(ids)


def core_hosts(status, expected):
    hosts = [m for m in status["machines"] if m["machine_type"] == 2]
    if len(hosts) != len(expected) or {m["id"] for m in hosts} != expected:
        raise NotReady("Core and MAT host identities differ")
    if any(m["state"] not in ("Ready", "Assigned/Ready") for m in hosts):
        raise NotReady("Core hosts are not all Ready or Assigned/Ready")
    return hosts


def fresh_inventory(logs, site_id, expected_total):
    # Page activities run concurrently. Only a self-contained completion record
    # can prove that the final page succeeded; adjacent log lines cannot.
    for line in logs.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if not isinstance(event, dict) or event.get("Activity") != "UpdateMachinesInDB" or event.get("Site ID") != site_id:
            continue
        if event.get("msg") != "completed activity" or event.get("inventory_status") != "INVENTORY_STATUS_SUCCESS":
            continue
        page = event.get("inventory_page")
        pages = event.get("inventory_total_pages")
        total = event.get("inventory_total_items")
        if (all(type(value) is int for value in (page, pages, total))
                and page == pages and pages > 0 and total == expected_total and total > 0):
            return True
    return False


class Verification:
    def __init__(self, args):
        self.args = args
        self.deadline = time.monotonic() + args.timeout
        self.since = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        self.context = args.context or self.run(["kubectl", "config", "current-context"], "Kubernetes context").strip()
        if not self.context:
            raise NotReady("no Kubernetes context selected")

    def run(self, command, description, env=None, input=None):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise NotReady("verification deadline reached")
        # Bound exec/log streams too; kubectl's HTTP timeout alone does not bound
        # a remote process. Kill the process group so a CLI timeout cannot orphan it.
        with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, text=True, env=env,
                              start_new_session=True) as process:
            try:
                stdout, _ = process.communicate(input, timeout=min(45, remaining))
            except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
                os.killpg(process.pid, signal.SIGKILL)
                process.communicate()
                if isinstance(error, KeyboardInterrupt):
                    raise
                raise NotReady(f"{description} timed out") from error
            if process.returncode:
                # Command output may contain authentication data; do not log it.
                raise NotReady(f"{description} failed (exit {process.returncode})")
        return stdout

    def kubectl(self, namespace, *args):
        return self.run(["kubectl", "--context", self.context, "--request-timeout=30s",
                         "-n", namespace, *args], "Kubernetes " + args[0])

    def admin(self, *args, description):
        return self.run(["bash", str(Path(__file__).with_name("core-admin.sh")), *args],
                        description, env={**os.environ,
                            "LOCAL_DEV_NAMESPACE": self.args.core_namespace,
                            "LOCAL_DEV_KUBE_CONTEXT": self.context,
                            "FORGE_CLIENT_REQUEST_TIMEOUT_SECS": "15"})

    def rest_json(self, url, token=None, form=False, expected_total=None):
        command = ["curl", "--fail", "--silent", "--max-time", "5", url]
        if expected_total is not None:
            command += ["--dump-header", "-"]
        config = ""
        if token:
            # Keep the bearer token off process command lines.
            command += ["--config", "-"]
            config = 'header = "Authorization: Bearer ' + token + '"\n'
        if form:
            command += ["--data", "client_id=nico-api", "--data", "client_secret=nico-local-secret",
                        "--data", "grant_type=password", "--data", "username=admin@example.com",
                        "--data", "password=adminpassword"]
        response = self.run(command, "local REST request", input=config)
        if expected_total is not None:
            headers, separator, response = response.partition("\n\n")
            pagination = Parser().parsestr(headers.split("\n", 1)[-1]).get("X-Pagination")
            if not separator or not pagination or json.loads(pagination).get("total") != expected_total:
                raise NotReady("REST inventory total differs from MAT/Core or pagination is missing")
        return json.loads(response)

    def check_rest(self, expected):
        namespace = self.args.rest_namespace
        config = json.loads(self.kubectl(namespace, "get", "configmap", "nico-rest-site-agent-config", "-o", "json"))
        secret = json.loads(self.kubectl(namespace, "get", "secret", "site-registration", "-o", "json"))
        workload = json.loads(self.kubectl(namespace, "get", "statefulset", "nico-rest-site-agent", "-o", "json"))
        container = next((c for c in workload["spec"]["template"]["spec"]["containers"] if c["name"] == "site-agent"), None)
        if container is None:
            raise NotReady("site-agent workload has no site-agent container")
        env = {entry["name"]: entry for entry in container.get("env", [])}
        identities = []
        for name in ("CLUSTER_ID", "TEMPORAL_SUBSCRIBE_NAMESPACE"):
            entry = env.get(name)
            if entry is None:
                identities.append(config["data"][name])
            elif entry.get("valueFrom") == {"secretKeyRef": {"name": "site-registration", "key": "site-uuid"}}:
                identities.append(base64.b64decode(secret["data"]["site-uuid"], validate=True).decode())
            elif "value" in entry:
                identities.append(entry["value"])
            else:
                raise NotReady("unsupported site-agent identity source")
        site_id = identities[0]
        if identities[1] != site_id:
            raise NotReady("site-agent identity and Temporal subscription namespace disagree")
        secret_id = base64.b64decode(secret["data"]["site-uuid"], validate=True).decode()
        if not site_id or site_id == "00000000-0000-4000-8000-000000000001" or site_id != secret_id:
            raise NotReady("site-agent identity and registration Secret disagree or contain a placeholder")
        token = self.rest_json(self.args.keycloak_url + "/realms/nico-dev/protocol/openid-connect/token", form=True)["access_token"]
        if not isinstance(token, str) or not token or any(c in token for c in '\n\r"\\'):
            raise NotReady("Keycloak did not return a valid token")
        url = self.args.api_url + "/v2/org/test-org/nico/"
        site = self.rest_json(url + "site/" + site_id, token)
        if site.get("id") != site_id or site.get("isOnline") is not True or site.get("status") != "Registered":
            raise NotReady("REST site is not Registered and online")
        # This local workflow has a single MAT pod and at most 100 hosts. Fail
        # explicitly rather than silently accepting a truncated REST page.
        if len(expected) > 100:
            raise NotReady("local REST verification supports at most 100 hosts")
        machines = self.rest_json(url + "machine?siteId=" + site_id + "&isMissingOnSite=false&pageSize=100", token, expected_total=len(expected))
        if not isinstance(machines, list) or len(machines) != len(expected) or {
            m.get("controllerMachineId") for m in machines
        } != expected or any(m.get("siteId") != site_id or m.get("isUsableByTenant") is not True for m in machines):
            raise NotReady("REST does not contain the exact usable Core/MAT host inventory")
        logs = self.kubectl(namespace, "logs", "deployment/nico-rest-site-worker", "--since-time=" + self.since)
        if not fresh_inventory(logs, site_id, len(expected)):
            raise NotReady(
                f"waiting for a fresh successful final-page completion with {len(expected)} hosts; "
                "the receipt must include matching inventory_total_items and structured inventory metadata. "
                "Rebuild the REST workflow image if completion logs lack inventory_* fields")

    def check(self):
        namespace = self.args.core_namespace
        service = json.loads(self.kubectl(namespace, "get", "service", self.args.mat_service, "-o", "json"))
        address = service["spec"]["clusterIP"]
        if not address or address == "None":
            raise NotReady("MAT control Service has no ClusterIP")
        if ":" in address:
            address = "[" + address + "]"
        status = json.loads(self.kubectl(namespace, "exec", "deployment/nico-api", "-c", "nico-api", "--",
                            "curl", "--fail", "--insecure", "--silent", "--max-time", "5",
                            "https://" + address + ":1266/machines/status"))
        expected = mat_host_ids(status)
        hosts = core_hosts(json.loads(self.admin("-f", "json", "machine", "show", description="Core machine show")), expected)
        if self.args.rest:
            self.check_rest(expected)
        # An Assigned/Ready host runs a tenant OS; only idle Ready hosts run Scout.
        for host in hosts:
            if host["state"] == "Ready":
                self.admin("scout-stream", "ping", host["id"], description="Core scout-stream ping")
        return len(hosts)


def positive_seconds(value):
    seconds = int(value)
    if not 1 <= seconds <= 3600:
        raise argparse.ArgumentTypeError("must be between 1 and 3600 seconds")
    return seconds


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", help="Kubernetes context (default: active kubectl context)")
    parser.add_argument("--core-namespace", default="nico-system")
    parser.add_argument("--mat-service", default="nico-machine-a-tron-mat-0-bmc-mock",
                        help="single MAT control Service in the Core namespace (HTTPS port 1266)")
    parser.add_argument("--rest", action="store_true", help="also require registered, fresh REST inventory")
    parser.add_argument("--rest-namespace", default="nico-rest")
    parser.add_argument("--api-url", default="http://localhost:18388")
    parser.add_argument("--keycloak-url", default="http://localhost:18082")
    parser.add_argument("--timeout", type=positive_seconds, default=900,
                        help="overall deadline in seconds, 1–3600 (default: 900)")
    args = parser.parse_args()
    try:
        verification = Verification(args)
        previous = None
        while True:
            if time.monotonic() >= verification.deadline:
                print("Stack verification failed: " + (previous or "deadline reached"), file=sys.stderr)
                return 1
            try:
                count = verification.check()
                print(f"Stack ready: {count} matching MAT/Core hosts; idle Scout pings passed" +
                      ("; registered REST site and fresh usable inventory verified" if args.rest else ""), flush=True)
                return 0
            except (NotReady, KeyError, TypeError, ValueError, AttributeError) as error:
                reason = str(error) if isinstance(error, NotReady) else "invalid readiness response shape"
                remaining = verification.deadline - time.monotonic()
                if remaining <= 0:
                    print("Stack verification failed: " + reason, file=sys.stderr)
                    return 1
                if reason != previous:
                    print("Waiting: " + reason, flush=True)
                    previous = reason
                time.sleep(min(5, remaining))
    except KeyboardInterrupt:
        return 130
    except (NotReady, OSError) as error:
        print("Stack verification failed: " + str(error), file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
