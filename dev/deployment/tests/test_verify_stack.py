# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import importlib.util
import unittest
from pathlib import Path

VERIFY = load_script('verify_stack', Path(__file__).resolve().parents[1] / 'verify-stack.py')


class InventoryTest(unittest.TestCase):
    def test_only_hosts_count_and_every_host_needs_a_unique_identity(self):
        host = {"device_kind": "machine", "machine_id": "host-a"}
        other = {"device_kind": "switch", "machine_id": "switch-a"}
        self.assertEqual(VERIFY.mat_host_ids({"machines": [host, other]}), {"host-a"})
        for devices in ([other], [host, host], [host, {"device_kind": "machine"}], [{}]):
            with self.subTest(devices=devices), self.assertRaises(VERIFY.NotReady):
                VERIFY.mat_host_ids({"machines": devices})

    def test_core_requires_exact_host_set_and_accepted_states(self):
        host = {"machine_type": 2, "id": "host-a", "state": "Ready"}
        dpu = {"machine_type": 1, "id": "dpu-a", "state": "Ready"}
        self.assertEqual(VERIFY.core_hosts({"machines": [host, dpu]}, {"host-a"}), [host])
        for machines in ([], [host, host], [{**host, "id": "other"}],
                         [{**host, "state": "Initializing"}]):
            with self.subTest(machines=machines), self.assertRaises(VERIFY.NotReady):
                VERIFY.core_hosts({"machines": machines}, {"host-a"})
        assigned = {**host, "state": "Assigned/Ready"}
        self.assertEqual(VERIFY.core_hosts({"machines": [assigned]}, {"host-a"}), [assigned])

    def test_fresh_inventory_requires_a_self_contained_successful_final_page(self):
        import json
        final = {"Activity": "UpdateMachinesInDB", "Site ID": "site-a",
                 "msg": "completed activity", "inventory_page": 2,
                 "inventory_total_pages": 2, "inventory_total_items": 38,
                 "inventory_status": "INVENTORY_STATUS_SUCCESS"}
        received = {"Activity": "UpdateMachinesInDB", "Site ID": "site-a",
                    "msg": "Received Machine inventory page: 2 of 2, page size: 13, total count: 38"}
        first = {**final, "inventory_page": 1}
        received_first = {**received, "msg": "Received Machine inventory page: 1 of 2, page size: 25, total count: 38"}
        cases = [
            ("reordered pages and completions", [received, received_first, first, final], True),
            ("later activity does not erase completed receipt", [final, {**first, "msg": "starting activity"}], True),
            ("other page cannot complete a failed final page", [received, first], False),
            ("retries without success", [received, {**final, "msg": "starting activity"}], False),
            ("legacy uncorrelated completion", [received, {"Activity": "UpdateMachinesInDB", "Site ID": "site-a", "msg": "completed activity"}], False),
            ("empty inventory", [{**final, "inventory_total_items": 0}], False),
            ("different inventory size", [{**final, "inventory_total_items": 39}], False),
            ("failed collection", [{**final, "inventory_status": "INVENTORY_STATUS_FAILED"}], False),
            ("other site", [{**final, "Site ID": "other-site"}], False),
            ("other activity", [{**final, "Activity": "UpdateExpectedMachinesInDB"}], False),
            ("malformed pagination", [{**final, "inventory_page": "2"}], False),
            ("missing pagination", [{k: v for k, v in final.items() if k != "inventory_total_pages"}], False),
            ("no receipt", [], False),
        ]
        for name, events, expected in cases:
            with self.subTest(name=name):
                logs = "not json\n[]\n{}\n" + "\n".join(json.dumps(e) for e in events)
                self.assertEqual(VERIFY.fresh_inventory(logs, "site-a", 38), expected)


class CommandTest(unittest.TestCase):
    def invoke(self, mode="healthy", rest=False, *extra, command_delay=0):
        import os
        import subprocess
        import sys
        import tempfile
        import json
        with tempfile.TemporaryDirectory(prefix="stack check ") as temporary:
            root = Path(temporary)
            fake = root / "command"
            fake.write_text(f"#!{sys.executable}\n" + r'''
import base64, json, os, sys, time
from pathlib import Path
name, args = Path(sys.argv[0]).name, sys.argv[1:]
mode = os.environ["TEST_MODE"]
time.sleep(float(os.environ["TEST_COMMAND_DELAY"]))
context = os.environ["TEST_CONTEXT"]
with open(os.environ["TEST_CALLS"], "a") as log:
    log.write(json.dumps([name, args]) + "\n")
if name == "kubectl":
    if args == ["config", "current-context"]:
        print(context)
        sys.exit()
    assert args[:3] == ["--context", context, "--request-timeout=30s"], args
    assert args[3:5] in (["-n", "core-test"], ["-n", "rest-test"]), args
    args = args[5:]
    if args[:2] == ["get", "service"]:
        print(json.dumps({"spec": {"clusterIP": "fd00::10"}}))
    elif args[0] == "exec":
        assert args[-1] == "https://[fd00::10]:1266/machines/status", args
        if mode == "stalled":
            time.sleep(30)
        print(json.dumps({"machines": [{"device_kind": "machine", "machine_id": "host-a"},
                                      {"device_kind": "switch", "machine_id": "switch-a"}]}))
    elif args[:2] == ["get", "configmap"]:
        print(json.dumps({"data": {"CLUSTER_ID": "stale" if mode == "secret-identity" else "site-a", "TEMPORAL_SUBSCRIBE_NAMESPACE": "stale" if mode == "secret-identity" else "site-a"}}))
    elif args[:2] == ["get", "statefulset"]:
        env = []
        if mode in ("secret-identity", "wrong-identity-source"):
            env = [{"name": key, "valueFrom": {"secretKeyRef": {
                "name": "other-secret" if mode == "wrong-identity-source" else "site-registration", "key": "site-uuid"}}}
                for key in ("CLUSTER_ID", "TEMPORAL_SUBSCRIBE_NAMESPACE")]
        print(json.dumps({"spec": {"template": {"spec": {"containers": [{"name": "site-agent", "env": env}]}}}}))
    elif args[:2] == ["get", "secret"]:
        print(json.dumps({"data": {"site-uuid": base64.b64encode(
            ("other-site" if mode == "wrong-registration" else "site-a").encode()).decode()}}))
    elif args[0] == "logs":
        assert args[-1].startswith("--since-time="), args
        if mode != "stale":
            for msg in ["starting activity", "Received Machine inventory page: 1 of 1, page size: 100, total count: 1", "completed activity"]:
                event = {"Activity": "UpdateMachinesInDB", "Site ID": "site-a", "msg": msg}
                event.update(inventory_page=1, inventory_total_pages=1,
                             inventory_total_items=2 if mode == "wrong-receipt-total" else 1,
                             inventory_status="INVENTORY_STATUS_SUCCESS")
                print(json.dumps(event))
    else:
        sys.exit("unexpected kubectl command")
elif name == "bash":
    assert args[0].endswith("/deployment/core-admin.sh"), args
    assert os.environ["LOCAL_DEV_KUBE_CONTEXT"] == context
    assert os.environ["LOCAL_DEV_NAMESPACE"] == "core-test"
    assert os.environ["FORGE_CLIENT_REQUEST_TIMEOUT_SECS"] == "15"
    if args[1:] == ["-f", "json", "machine", "show"]:
        marker = Path(os.environ["TEST_CALLS"] + ".retry")
        fail = mode == "retry" and not marker.exists()
        marker.touch()
        if fail:
            sys.exit(1)
        print(json.dumps({"machines": [{"machine_type": 2, "id": "host-a",
            "state": "Assigned/Ready" if mode == "assigned" else "Ready"}]}))
    elif args[1:] == ["scout-stream", "ping", "host-a"]:
        marker = Path(os.environ["TEST_CALLS"] + ".scout-retry")
        fail = mode == "scout-retry" and not marker.exists()
        marker.touch()
        if mode == "scout-down" or fail:
            sys.exit(1)
    else:
        sys.exit("unexpected admin command")
elif name == "curl":
    if "--config" in args:
        assert sys.stdin.read() == 'header = "Authorization: Bearer fixture-token"\n'
    if any("openid-connect" in a for a in args):
        print(json.dumps({"access_token": "fixture-token"}))
    elif any("/site/" in a for a in args):
        print(json.dumps({"id": "site-a", "isOnline": mode != "offline", "status": "Registered"}))
    else:
        assert "--dump-header" in args
        print('HTTP/1.1 200 OK\r\nX-Pagination: {"total": ' + ("101" if mode == "truncated" else "1") + '}\r\n\r\n', end="")
        marker = Path(os.environ["TEST_CALLS"] + ".rest-retry")
        pending = mode == "rest-retry" and not marker.exists()
        marker.touch()
        print(json.dumps([{"controllerMachineId": "wrong" if mode == "wrong-rest" else "host-a",
                           "siteId": "site-a", "isUsableByTenant": mode != "unusable" and not pending}]))
else:
    sys.exit("unexpected executable")
''')
            fake.chmod(0o755)
            for command in ("kubectl", "bash", "curl"):
                (root / command).symlink_to(fake)
            calls = root / "calls"
            driver = root / "verify.py"
            driver.write_text(r'''import importlib.util, sys, types
spec = importlib.util.spec_from_file_location("verify_stack", sys.argv.pop(1))
verify = importlib.util.module_from_spec(spec)
spec.loader.exec_module(verify)
# Advance only the verifier's retry clock. Real subprocess deadlines remain
# active, including the stalled-command case.
clock = [0.0]
def sleep(seconds):
    clock[0] += seconds
verify.time = types.SimpleNamespace(monotonic=lambda: clock[0], sleep=sleep)
sys.exit(verify.main())
''')
            entrypoint = [str(driver), str(VERIFY.__file__)]
            result = subprocess.run(
                [sys.executable, "-B", *entrypoint, "--core-namespace", "core-test",
                 "--rest-namespace", "rest-test", "--timeout", "15" if mode in ("retry", "rest-retry", "scout-retry", "healthy", "assigned") else "3",
                 *(["--rest"] if rest else []), *extra],
                env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                     "TEST_MODE": mode, "TEST_CALLS": str(calls),
                     "TEST_COMMAND_DELAY": str(command_delay),
                     "TEST_CONTEXT": extra[extra.index("--context") + 1] if "--context" in extra else "kind-selected"},
                capture_output=True, text=True, timeout=25)
            records = [json.loads(line) for line in calls.read_text().splitlines()] if calls.exists() else []
            return result, records

    def test_core_only_and_rest_check_the_selected_context_and_scout(self):
        for rest in (False, True):
            with self.subTest(rest=rest):
                result, calls = self.invoke(rest=rest)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("1 matching MAT/Core hosts", result.stdout)
                self.assertTrue(any("scout-stream" in args for _, args in calls))
                self.assertEqual(any(name == "curl" for name, _ in calls), rest)

    def test_secret_identity_checks_both_environment_bindings(self):
        for mode, expected in (("secret-identity", 0), ("wrong-identity-source", 1)):
            with self.subTest(mode=mode):
                result, _ = self.invoke(mode, rest=True)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                if expected:
                    self.assertIn("unsupported site-agent identity source", result.stdout)

    def test_fixture_command_latency_does_not_consume_the_retry_deadline(self):
        result, _ = self.invoke("wrong-rest", rest=True, command_delay=0.3)
        self.assertEqual(result.returncode, 1)
        self.assertIn("exact usable", result.stdout + result.stderr)

    def test_assigned_hosts_do_not_require_scout(self):
        result, calls = self.invoke("assigned", rest=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any("scout-stream" in args for _, args in calls))

    def test_failures_do_not_become_readiness(self):
        for mode, reason in (("stale", "Rebuild the REST workflow image"),
                             ("wrong-receipt-total", "completion with 1 hosts"),
                             ("offline", "Registered"),
                             ("wrong-rest", "exact usable"), ("unusable", "exact usable"),
                             ("truncated", "inventory total"),
                             ("wrong-registration", "disagree"), ("scout-down", "scout-stream"),
                             ("stalled", "timed out")):
            with self.subTest(mode=mode):
                result, _ = self.invoke(mode, rest=True)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn(reason, result.stdout + result.stderr)
                self.assertNotIn("Stack ready:", result.stdout)

    def test_core_rest_and_scout_recover_before_readiness(self):
        for mode in ("retry", "rest-retry", "scout-retry"):
            with self.subTest(mode=mode):
                result, calls = self.invoke(mode, rest=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(sum("show" in args for _, args in calls), 2)
                if mode == "retry":
                    self.assertIn("Core machine show failed (exit 1)", result.stdout)
                if mode == "scout-retry":
                    self.assertEqual(sum("scout-stream" in args for _, args in calls), 2)

    def test_context_override_preserves_non_kind_support(self):
        result, calls = self.invoke("healthy", False, "--context", "kind-selected")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(args == ["config", "current-context"] for _, args in calls))
        result, calls = self.invoke("healthy", False, "--context", "k3s-local")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(any(args == ["config", "current-context"] for _, args in calls))


class FrontendTest(unittest.TestCase):
    def test_tilt_rejects_an_unconfigured_readiness_pod_before_deploying(self):
        source = (Path(VERIFY.__file__).parent / "tilt/Tiltfile").read_text()
        block = source[source.index("machine_a_tron_dhcp_pod ="):source.index("\ndef merged(")]
        def fail(message):
            raise ValueError(message)
        for pod_name in ("mat-test", "missing"):
            with self.subTest(pod_name=pod_name):
                scope = {"bmc_lab": False, "development": {"machineATronDhcp": {"podName": pod_name}},
                         "all_values": {"nico-machine-a-tron": {"pods": {
                             "mat-test": {"machines": {"compute": {"hostCount": 1}}}}}},
                         "fail": fail}
                if pod_name == "missing":
                    with self.assertRaisesRegex(ValueError, "configured pod: missing"):
                        exec(block, scope)
                else:
                    exec(block, scope)
                    self.assertEqual(scope["machine_a_tron_dhcp_pod"], "mat-test")

    def test_tilt_gates_selected_profile_and_context(self):
        import os
        source = (Path(VERIFY.__file__).parent / "tilt/Tiltfile").read_text()
        block = source[source.index("stack_verifier ="):source.index("\nenabled_resources =")]
        for rest, mcp in ((False, False), (True, False), (True, True)):
            with self.subTest(rest=rest, mcp=mcp):
                resources = []
                scope = {"os": os, "repo_root": "/repo with spaces", "enable_rest": rest, "enable_mcp": mcp,
                         "cluster_context": "kind-selected", "core_namespace": "core-test",
                         "rest_namespace": "rest-test", "machine_a_tron_dhcp_pod": "mat-test",
                         "local_resource": lambda *a, **kw: resources.append((a, kw))}
                exec(block, scope)
                (name, command), options = resources[0]
                self.assertEqual(name, "stack-ready")
                self.assertEqual(command[:4], ["python3", "/repo with spaces/dev/deployment/verify-stack.py",
                                               "--context", "kind-selected"])
                self.assertIn("nico-machine-a-tron-mat-test-bmc-mock", command)
                self.assertEqual("--rest" in command, rest)
                self.assertEqual("nico-rest-site-agent" in options["resource_deps"], rest)
                self.assertEqual("nico-mcp" in options["resource_deps"], mcp)
                self.assertIn("'stack-ready'", source[source.index("enabled_resources ="):])

    def test_legacy_setup_delegates_and_propagates_failure(self):
        import os
        import subprocess
        source = (Path(VERIFY.__file__).parent / "devspace/setup-rest-integration.sh").read_text()
        start = source.index('python3 "${SCRIPT_DIR}/../verify-stack.py"')
        end = source.index('\nif [[ "${DSX_GATEWAY_ENABLED}"', start)
        for status in (0, 42):
            with self.subTest(status=status):
                result = subprocess.run(["bash", "-c", """set -euo pipefail
python3() { printf '<%s>\\n' "$@"; return "$TEST_STATUS"; }
""" + source[start:end] + "\necho verified"],
                    env={**os.environ, "SCRIPT_DIR": "/repo with spaces/dev/deployment/devspace",
                         "CORE_NAMESPACE": "core-test", "REST_NAMESPACE": "rest-test",
                         "MACHINE_A_TRON_BMC_SERVICE": "mat-control", "API_FORWARD_PORT": "18389",
                         "KEYCLOAK_FORWARD_PORT": "18083", "TEST_STATUS": str(status)},
                    capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertIn("<--rest>", result.stdout)
                self.assertIn("<http://localhost:18389>", result.stdout)
                self.assertIn("<http://localhost:18083>", result.stdout)
                self.assertEqual("verified" in result.stdout, status == 0)


if __name__ == "__main__":
    unittest.main()
