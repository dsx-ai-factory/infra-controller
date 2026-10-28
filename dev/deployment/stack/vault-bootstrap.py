#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Initialize or unseal the optional, file-backed local Vault.

Uses the single-share initialization and Kubernetes Secret retention model from
PR #7555's setup-kind-vault.sh. The existing Tilt Core contract uses a token and
its own PKI mounts, rather than that PR's Kubernetes auth policy.
"""

import argparse
import base64
import json
import os
import subprocess
import sys
import tempfile

KEYS = "nico-vault-local-keys"
TOKEN = "nico-vault-token"
PVC = "data-nico-vault-0"
OWNER = "nico-local-vault"
LABEL = "app.kubernetes.io/managed-by"
PVC_ANNOTATION = "nico.local/vault-pvc-uid"


class VaultError(Exception):
    pass


class Bootstrap:
    def __init__(self, context, namespace):
        self.kubectl = ["kubectl", "--context", context, "--request-timeout=30s", "-n", namespace]
        self.namespace = namespace
        self.token = ""
        self.pvc_uid = ""

    def kube(self, *args, data=None):
        result = subprocess.run(self.kubectl + list(args), input=data, text=True,
                                capture_output=True, timeout=90)
        if result.returncode:
            # kubectl errors can include submitted Secret objects.
            raise VaultError("Kubernetes operation failed; check cluster access and resource ownership")
        return result.stdout

    def get(self, kind, name):
        output = self.kube("get", kind, name, "--ignore-not-found", "-o", "json")
        return json.loads(output) if output.strip() else None

    def check_mode(self, mode):
        statefulset = self.get("statefulset", "nico-vault")
        if statefulset:
            containers = statefulset["spec"]["template"]["spec"]["containers"]
            vault = next(c for c in containers if c["name"] == "vault")
            ephemeral = any(e["name"] == "VAULT_DEV_ROOT_TOKEN_ID" for e in vault.get("env", []))
            if ephemeral != (mode == "ephemeral"):
                raise VaultError("Vault mode cannot change in an existing cluster; use a new kind cluster")
        if mode == "ephemeral" and (self.get("pvc", PVC) or self.get("secret", KEYS)):
            raise VaultError("Retained Vault state exists; select persistent mode or a new kind cluster")

    def owned_secret(self, name):
        secret = self.get("secret", name)
        if secret and (secret["metadata"].get("labels", {}).get(LABEL) != OWNER or
                       secret["metadata"].get("annotations", {}).get(PVC_ANNOTATION) != self.pvc_uid):
            raise VaultError("Secret ownership or Vault PVC identity differs: " + name)
        return secret

    def create_secret(self, name, values):
        self.kube("create", "-f", "-", data=json.dumps({
            "apiVersion": "v1", "kind": "Secret", "type": "Opaque", "immutable": True,
            "metadata": {"name": name, "namespace": self.namespace,
                         "labels": {LABEL: OWNER}, "annotations": {PVC_ANNOTATION: self.pvc_uid}},
            "stringData": values,
        }))

    def vault(self, *args, data=None, allow_failure=False, stdout=None):
        # Token and JSON payload travel on stdin, never argv or Tilt output.
        command = self.kubectl + ["exec", "-i", "statefulset/nico-vault", "--", "sh", "-ec",
                                  'read -r VAULT_TOKEN; export VAULT_TOKEN; '
                                  'export VAULT_ADDR=http://127.0.0.1:8200; exec vault "$@"', "vault"]
        result = subprocess.run(command + list(args), input=self.token + "\n" + (data or ""),
                                text=True, stdout=stdout if stdout is not None else subprocess.PIPE,
                                stderr=subprocess.PIPE, timeout=90)
        if result.returncode and not allow_failure:
            raise VaultError("Vault operation failed: " + args[0])
        return result

    def status(self):
        result = self.vault("status", "-format=json", allow_failure=True)
        if result.returncode not in (0, 2):
            raise VaultError("Vault status is unavailable")
        return json.loads(result.stdout)

    def bootstrap(self):
        self.check_mode("persistent")
        pvc = self.get("pvc", PVC)
        if not pvc or pvc.get("status", {}).get("phase") != "Bound":
            raise VaultError("Persistent Vault requires its bound data PVC")
        self.pvc_uid = pvc["metadata"]["uid"]
        keys_secret = self.owned_secret(KEYS)
        token_secret = self.owned_secret(TOKEN)
        status = self.status()
        if not status["initialized"]:
            if keys_secret or token_secret:
                raise VaultError("Vault is empty but retained credentials exist; restore its original data")
            # Keep a mode-0600 recovery file if Kubernetes cannot retain the keys.
            fd, recovery = tempfile.mkstemp(prefix="nico-vault-init-", suffix=".json")
            with os.fdopen(fd, "w+") as output:
                try:
                    print("Vault initialization recovery file: " + recovery, file=sys.stderr, flush=True)
                    result = self.vault("operator", "init", "-key-shares=1", "-key-threshold=1",
                                        "-format=json", allow_failure=True, stdout=output)
                except (subprocess.TimeoutExpired, KeyboardInterrupt, OSError):
                    result = subprocess.CompletedProcess([], 1)
                finally:
                    output.flush()
                    os.fsync(output.fileno())
                output.seek(0)
                captured = output.read()
            if not captured.strip():
                os.unlink(recovery)
                raise VaultError("No initialization keys returned; inspect Vault state before retrying")
            if result.returncode:
                raise VaultError("Initialization transport failed; captured output needs recovery from " + recovery)
            try:
                keys = json.loads(captured)
                self.create_secret(KEYS, {"keys.json": captured})
            except Exception as error:
                raise VaultError("Initialization keys need recovery from " + recovery) from error
            os.unlink(recovery)
        else:
            if not keys_secret:
                raise VaultError("Initialized Vault has no retained keys; restore the keys Secret, do not reinitialize")
            keys = json.loads(base64.b64decode(keys_secret["data"]["keys.json"]))
        self.token = keys["root_token"]
        if token_secret and base64.b64decode(token_secret["data"]["token"]).decode() != self.token:
            raise VaultError("Core token differs from retained initialization token")
        if status["sealed"]:
            self.vault("write", "sys/unseal", "-", data=json.dumps({"key": keys["unseal_keys_b64"][0]}))
        if self.status()["sealed"]:
            raise VaultError("Vault remains sealed")
        self.configure()
        if not token_secret:
            self.create_secret(TOKEN, {"token": self.token})
        print("Persistent Vault is initialized, unsealed and configured")

    def configure(self):
        mounts = json.loads(self.vault("secrets", "list", "-format=json").stdout)
        for mount, kind in (("secrets", "kv"), ("certs", "pki")):
            existing = mounts.get(mount + "/")
            if existing:
                if existing["type"] != kind or (kind == "kv" and existing.get("options", {}).get("version") != "2"):
                    raise VaultError("Incompatible Vault mount: " + mount)
            else:
                options = ["-version=2"] if kind == "kv" else []
                self.vault("secrets", "enable", "-path=" + mount, *options, kind)
        self.vault("secrets", "tune", "-max-lease-ttl=87600h", "certs")
        issuers = self.vault("list", "-format=json", "certs/issuers", allow_failure=True)
        if issuers.returncode == 2 and issuers.stdout.strip() == "{}":
            self.vault("write", "certs/root/generate/internal", "common_name=local-vault-ca", "ttl=87600h")
        elif issuers.returncode:
            raise VaultError("Unable to list Vault CA issuers")
        # An existing issuer with a missing default is a configuration error,
        # not permission to create a different CA.
        self.vault("read", "-format=json", "certs/cert/ca")
        self.vault("write", "certs/roles/nico-cluster", "allow_any_name=true", "allow_bare_domains=true",
                   "allow_subdomains=true", "allow_localhost=true", "require_cn=false", "max_ttl=72h",
                   "allowed_uri_sans=spiffe://nico.local/*")
        for path in ("machines/bmc/site/root", "machines/all_dpus/site_default/uefi-metadata-items/auth",
                     "machines/all_hosts/site_default/uefi-metadata-items/auth"):
            seed = self.vault("kv", "put", "-cas=0", "secrets/" + path, "-", allow_failure=True,
                              data=json.dumps({"UsernamePassword": {"username": "root", "password": "vault-password"}}))
            if seed.returncode and "check-and-set parameter did not match" not in seed.stderr:
                raise VaultError("Unable to initialize missing Vault fixture")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True)
    parser.add_argument("--namespace", default="nico-system")
    commands = parser.add_subparsers(dest="command", required=True)
    check = commands.add_parser("check-mode", help="Reject a change to an existing Vault's storage mode")
    check.add_argument("--mode", choices=("ephemeral", "persistent"), required=True)
    commands.add_parser("bootstrap", help="Initialize or unseal persistent Vault and preserve existing secrets")
    args = parser.parse_args()
    if not args.context.startswith("kind-"):
        parser.error("Local Vault requires a kind- context")
    try:
        bootstrap = Bootstrap(args.context, args.namespace)
        if args.command == "check-mode":
            bootstrap.check_mode(args.mode)
        else:
            bootstrap.bootstrap()
    except (VaultError, OSError, ValueError, KeyError, subprocess.TimeoutExpired) as error:
        # JSON errors may contain sensitive input; report only our controlled errors.
        print(str(error) if isinstance(error, VaultError) else "Vault bootstrap failed; inspect cluster state", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
