# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import base64
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "stack/vault-bootstrap.py"
vault = load_script('vault_bootstrap', SCRIPT)


def streamed_init(payload, code=0, error=None):
    def run(*args, **kwargs):
        kwargs["stdout"].write(payload)
        kwargs["stdout"].flush()
        if error:
            raise error
        return result(code=code)
    return run


def result(output="", error="", code=0):
    return subprocess.CompletedProcess([], code, output, error)


class VaultBootstrapTest(unittest.TestCase):
    def setUp(self):
        self.bootstrap = vault.Bootstrap("kind-test", "nico-system")

    def test_mode_guards_retained_storage_and_live_configuration(self):
        for installed, pvc, selected, fails in (
            (None, False, "ephemeral", False),
            (None, True, "ephemeral", True),
            ("ephemeral", False, "persistent", True),
            ("persistent", True, "ephemeral", True),
            ("persistent", True, "persistent", False),
        ):
            with self.subTest(installed=installed, pvc=pvc, selected=selected):
                env = [{"name": "VAULT_DEV_ROOT_TOKEN_ID"}] if installed == "ephemeral" else []
                sts = {"spec": {"template": {"spec": {"containers": [{"name": "vault", "env": env}]}}}}
                def get(kind, name):
                    return (sts if installed else None) if kind == "statefulset" else (pvc if kind == "pvc" else None)
                with patch.object(self.bootstrap, "get", side_effect=get):
                    if fails:
                        with self.assertRaises(vault.VaultError):
                            self.bootstrap.check_mode(selected)
                    else:
                        self.bootstrap.check_mode(selected)

    def test_ownership_checks_pvc_identity_before_adoption(self):
        self.bootstrap.pvc_uid = "original"
        for owner, pvc, fails in ((vault.OWNER, "original", False),
                                  ("helm", "original", True), (vault.OWNER, "replacement", True)):
            with self.subTest(owner=owner, pvc=pvc):
                secret = {"metadata": {"labels": {vault.LABEL: owner},
                                       "annotations": {vault.PVC_ANNOTATION: pvc}}}
                with patch.object(self.bootstrap, "get", return_value=secret):
                    if fails:
                        with self.assertRaises(vault.VaultError):
                            self.bootstrap.owned_secret(vault.KEYS)
                    else:
                        self.assertIs(self.bootstrap.owned_secret(vault.KEYS), secret)

    def test_missing_or_stale_keys_never_reinitialize(self):
        for initialized, keys in ((True, None), (False, {"data": {}})):
            with self.subTest(initialized=initialized), \
                 patch.object(self.bootstrap, "check_mode"), \
                 patch.object(self.bootstrap, "get", return_value={"metadata": {"uid": "pvc"}, "status": {"phase": "Bound"}}), \
                 patch.object(self.bootstrap, "owned_secret", side_effect=[keys, None]), \
                 patch.object(self.bootstrap, "status", return_value={"initialized": initialized, "sealed": True}), \
                 patch.object(self.bootstrap, "vault") as command:
                with self.assertRaises(vault.VaultError):
                    self.bootstrap.bootstrap()
                command.assert_not_called()

    def test_failed_secret_persistence_keeps_private_recovery_copy(self):
        keys = json.dumps({"root_token": "test-token", "unseal_keys_b64": ["test-key"]})
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(vault.tempfile, "tempdir", directory), \
             patch.object(self.bootstrap, "check_mode"), \
             patch.object(self.bootstrap, "get", return_value={"metadata": {"uid": "pvc"}, "status": {"phase": "Bound"}}), \
             patch.object(self.bootstrap, "owned_secret", return_value=None), \
             patch.object(self.bootstrap, "status", return_value={"initialized": False, "sealed": True}), \
             patch.object(self.bootstrap, "vault", side_effect=streamed_init(keys)), \
             patch.object(self.bootstrap, "create_secret", side_effect=vault.VaultError("unavailable")):
            with self.assertRaisesRegex(vault.VaultError, "Initialization keys need recovery"):
                self.bootstrap.bootstrap()
            files = list(Path(directory).iterdir())
            self.assertEqual(len(files), 1)
            self.assertEqual(files[0].stat().st_mode & 0o777, 0o600)
            self.assertEqual(files[0].read_text(), keys)

    def test_initialization_transport_errors_preserve_received_keys_without_empty_files(self):
        keys = json.dumps({"root_token": "test-token", "unseal_keys_b64": ["test-key"]})
        for response in (result(keys, code=1),
                         subprocess.TimeoutExpired("kubectl", 90, output=keys.encode()),
                         KeyboardInterrupt(), OSError("transport interrupted"), result(code=1)):
            with self.subTest(response=response), tempfile.TemporaryDirectory() as directory, \
                 patch.object(vault.tempfile, "tempdir", directory), \
                 patch.object(self.bootstrap, "check_mode"), \
                 patch.object(self.bootstrap, "get", return_value={"metadata": {"uid": "pvc"}, "status": {"phase": "Bound"}}), \
                 patch.object(self.bootstrap, "owned_secret", return_value=None), \
                 patch.object(self.bootstrap, "status", return_value={"initialized": False, "sealed": True}), \
                 patch.object(self.bootstrap, "vault") as command, \
                 patch.object(self.bootstrap, "create_secret") as create, \
                 patch.object(sys, "stderr", new_callable=io.StringIO) as stderr:
                def initialize(*args, **kwargs):
                    recovery = next(Path(directory).iterdir())
                    self.assertEqual(stderr.getvalue(), "Vault initialization recovery file: " + str(recovery) + "\n")
                    if isinstance(response, BaseException):
                        return streamed_init(keys, error=response)(*args, **kwargs)
                    return streamed_init(response.stdout, code=response.returncode)(*args, **kwargs)
                command.side_effect = initialize
                with self.assertRaises(vault.VaultError):
                    self.bootstrap.bootstrap()
                create.assert_not_called()
                files = list(Path(directory).iterdir())
                if isinstance(response, BaseException) or response.stdout:
                    self.assertEqual(len(files), 1)
                    self.assertEqual(files[0].read_text(), keys)
                    self.assertEqual(files[0].stat().st_mode & 0o777, 0o600)
                else:
                    self.assertEqual(files, [])

    def test_broken_recovery_log_aborts_before_init_without_leaving_a_file(self):
        with tempfile.TemporaryDirectory() as directory, \
             patch.object(vault.tempfile, "tempdir", directory), \
             patch.object(self.bootstrap, "check_mode"), \
             patch.object(self.bootstrap, "get", return_value={"metadata": {"uid": "pvc"}, "status": {"phase": "Bound"}}), \
             patch.object(self.bootstrap, "owned_secret", return_value=None), \
             patch.object(self.bootstrap, "status", return_value={"initialized": False, "sealed": True}), \
             patch.object(self.bootstrap, "vault") as command, \
             patch.object(sys.stderr, "write", side_effect=BrokenPipeError):
            with self.assertRaisesRegex(vault.VaultError, "No initialization keys returned"):
                self.bootstrap.bootstrap()
            command.assert_not_called()
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_configure_preserves_ca_and_rotated_credentials_but_propagates_other_failures(self):
        for fixture_error, fails in (("check-and-set parameter did not match the current version", False),
                                     ("permission denied", True)):
            with self.subTest(error=fixture_error):
                calls = []
                def command(*args, **kwargs):
                    calls.append((args, kwargs))
                    if args[:2] == ("secrets", "list"):
                        return result(json.dumps({"secrets/": {"type": "kv", "options": {"version": "2"}},
                                                  "certs/": {"type": "pki"}}))
                    if args[:2] == ("kv", "put"):
                        self.assertEqual(args[2], "-cas=0")
                        return result(error=fixture_error, code=2)
                    return result("existing CA")
                with patch.object(self.bootstrap, "vault", side_effect=command):
                    if fails:
                        with self.assertRaises(vault.VaultError):
                            self.bootstrap.configure()
                    else:
                        self.bootstrap.configure()
                self.assertFalse(any("certs/root/generate/internal" in args for args, _ in calls))
                self.assertFalse(any(args[:2] == ("secrets", "enable") for args, _ in calls))

    def test_fresh_issuer_is_created_only_for_an_empty_list(self):
        for response, created, fails in ((result("{}\n", code=2), True, False),
                                         (result(error="permission denied", code=2), False, True)):
            with self.subTest(created=created), patch.object(self.bootstrap, "vault") as command:
                def run(*args, **kwargs):
                    if args[:2] == ("secrets", "list"):
                        return result("{}")
                    if args[0] == "list":
                        return response
                    return result()
                command.side_effect = run
                if fails:
                    with self.assertRaises(vault.VaultError):
                        self.bootstrap.configure()
                else:
                    self.bootstrap.configure()
                command.assert_any_call("secrets", "tune", "-max-lease-ttl=87600h", "certs")
                self.assertEqual(any("certs/root/generate/internal" in call.args
                                     for call in command.call_args_list), created)

    def test_initialize_then_restart_round_trips_the_secret_write_contract(self):
        keys = {"root_token": "test-token", "unseal_keys_b64": ["test-key"]}
        objects = {}
        created = []
        def get(kind, name):
            if kind == "statefulset":
                return {"spec": {"template": {"spec": {"containers": [{"name": "vault"}]}}}}
            if kind == "pvc":
                return {"metadata": {"uid": "pvc"}, "status": {"phase": "Bound"}}
            return objects.get(name)
        def kube(*args, data=None):
            self.assertEqual(args, ("create", "-f", "-"))
            obj = json.loads(data)
            self.assertEqual((obj["apiVersion"], obj["kind"], obj["type"]), ("v1", "Secret", "Opaque"))
            self.assertEqual(obj["metadata"]["namespace"], "nico-system")
            self.assertTrue(obj["immutable"])
            name = obj["metadata"]["name"]
            self.assertNotIn(name, objects)
            obj["data"] = {k: base64.b64encode(v.encode()).decode() for k, v in obj.pop("stringData").items()}
            objects[name] = obj
            created.append(name)
            return ""
        with tempfile.TemporaryDirectory() as directory, patch.object(vault.tempfile, "tempdir", directory):
            for initialized in (False, True):
                bootstrap = vault.Bootstrap("kind-test", "nico-system")
                created.clear()
                def command(*args, **kwargs):
                    if args[:2] == ("operator", "init"):
                        self.assertFalse(initialized)
                        return streamed_init(json.dumps(keys))(*args, **kwargs)
                    self.assertEqual(args, ("write", "sys/unseal", "-"))
                    self.assertEqual(bootstrap.token, "test-token")
                    self.assertEqual(json.loads(kwargs["data"]), {"key": "test-key"})
                    return result()
                with patch.object(bootstrap, "get", side_effect=get), \
                     patch.object(bootstrap, "kube", side_effect=kube), \
                     patch.object(bootstrap, "vault", side_effect=command), \
                     patch.object(bootstrap, "configure"), \
                     patch.object(bootstrap, "status", side_effect=[
                         {"initialized": initialized, "sealed": True}, {"sealed": False}]):
                    bootstrap.bootstrap()
                self.assertEqual(created, [] if initialized else [vault.KEYS, vault.TOKEN])
                self.assertEqual(json.loads(base64.b64decode(objects[vault.KEYS]["data"]["keys.json"])), keys)
                self.assertEqual(base64.b64decode(objects[vault.TOKEN]["data"]["token"]), b"test-token")
                self.assertEqual(list(Path(directory).iterdir()), [])
            objects[vault.TOKEN]["data"]["token"] = base64.b64encode(b"different").decode()
            with patch.object(bootstrap, "get", side_effect=get), \
                 patch.object(bootstrap, "status", return_value={"initialized": True, "sealed": True}), \
                 patch.object(bootstrap, "vault") as command, patch.object(bootstrap, "kube") as write:
                with self.assertRaisesRegex(vault.VaultError, "Core token differs"):
                    bootstrap.bootstrap()
                command.assert_not_called()
                write.assert_not_called()

    def test_init_output_reaches_file_before_transport_returns(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            executable = root / "kubectl"
            executable.write_text(f"#!{sys.executable}\nimport os,time\nos.write(1, b'captured-init-output')\ntime.sleep(30)\n")
            executable.chmod(0o755)
            output = root / "recovery"
            code = f"""import importlib.util
s=importlib.util.spec_from_file_location('bootstrap', {str(SCRIPT)!r})
m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
with open({str(output)!r}, 'w') as f:
 m.Bootstrap('kind-test', 'nico-system').vault('operator', 'init', stdout=f)
"""
            process = subprocess.Popen([sys.executable, "-c", code], start_new_session=True,
                                       env={**os.environ, "PATH": str(root)}, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline and (not output.exists() or not output.stat().st_size):
                    time.sleep(0.02)
                self.assertIsNone(process.poll(), "Transport must still be running")
                self.assertEqual(output.read_text(), "captured-init-output")
            finally:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                process.communicate(timeout=5)
            self.assertEqual(output.read_text(), "captured-init-output")

    def test_transport_keeps_token_and_payload_out_of_argv(self):
        self.bootstrap.token = "test-token"
        with patch.object(vault.subprocess, "run", return_value=result()) as run:
            self.bootstrap.vault("write", "sys/unseal", "-", data='{"key":"test-key"}')
        args, kwargs = run.call_args
        self.assertNotIn("test-token", " ".join(args[0]))
        self.assertNotIn("test-key", " ".join(args[0]))
        self.assertEqual(kwargs["input"], 'test-token\n{"key":"test-key"}')
        self.assertEqual(kwargs["stdout"], subprocess.PIPE)
        self.assertEqual(kwargs["stderr"], subprocess.PIPE)


if __name__ == "__main__":
    unittest.main()
