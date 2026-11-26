# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Verify shared Helm ownership and parity with the retained Kustomize workflow."""

import json
from pathlib import Path
import subprocess
import unittest

import yaml

from tilt_test_support import TiltResult, baseline

ROOT = Path(__file__).resolve().parents[3]
KEYCLOAK = ROOT / "rest-api/deploy/kustomize/base/keycloak"


class KeycloakTest(unittest.TestCase):
    def test_tilt_forwards_inputs_and_preserves_workload_and_realm_contracts(self):
        inputs = {"image": "example.invalid/keycloak@sha256:" + "a" * 64,
                  "databaseUrl": "jdbc:postgresql://custom-db:5432/auth"}
        values = baseline()
        values['development']['keycloak'].update(inputs)
        values['development']['localConfig']['restNamespace'] = 'custom-rest'
        values['development']['localConfig']['database']['keycloak'].update(user='auth-user', password='auth-password')
        evaluated = TiltResult({'rest': True}, values=values)
        resource = evaluated.resources['keycloak']
        self.assertEqual(resource["resource_deps"], ["rest-databases-ready", "nico-local-config"])
        self.assertEqual(resource["port_forwards"], [(18082, 8080)])
        self.assertIn(str(KEYCLOAK / "nico-dev-realm.json"), resource["deps"])
        flags = [flag for flag in resource["flags"]
                 if flag != "--wait" and not flag.startswith("--timeout=")]
        output = subprocess.check_output([
            "helm", "template", resource["release_name"], resource["chart"],
            "--namespace", resource["namespace"], *flags], text=True)
        objects = list(filter(None, yaml.safe_load_all(output)))
        by_identity = {(o["kind"], o["metadata"]["name"]): o for o in objects}
        self.assertEqual(len(objects), 5)
        self.assertEqual(len(by_identity), 5)
        for obj in objects:
            self.assertEqual(obj["metadata"]["namespace"], "custom-rest")
            self.assertNotIn("helm.sh/hook", obj["metadata"].get("annotations", {}))
        secret = by_identity[("Secret", "keycloak-db-credentials")]
        self.assertEqual(secret["stringData"], {"username": "auth-user", "password": "auth-password"})
        # The two frontends retain the same public Service/Deployment contract.
        legacy = list(yaml.safe_load_all(subprocess.check_output([
            "kubectl", "kustomize", str(KEYCLOAK)], text=True)))
        for obj in legacy:
            obj["metadata"]["namespace"] = "custom-rest"
            if obj["kind"] == "Deployment":
                # Credential-triggered rollouts are protected separately below.
                obj["spec"]["template"]["metadata"]["annotations"] = by_identity[("Deployment", "keycloak")]["spec"]["template"]["metadata"]["annotations"]
                pod = obj["spec"]["template"]["spec"]
                pod.pop("imagePullSecrets")
                container = pod["containers"][0]
                container["image"] = inputs["image"]
                for env in container["env"]:
                    if env["name"] == "KC_DB_URL":
                        env["value"] = inputs["databaseUrl"]
                    if env["name"] in ("KC_DB_USERNAME", "KC_DB_PASSWORD"):
                        key = "username" if env["name"] == "KC_DB_USERNAME" else "password"
                        env.pop("value")
                        env["valueFrom"] = {"secretKeyRef": {"name": "keycloak-db-credentials", "key": key}}
            self.assertEqual(obj, by_identity[(obj["kind"], obj["metadata"]["name"])])
        imported = by_identity[("ConfigMap", "keycloak-realm")]["data"]["nico-dev-realm.json"]
        self.assertEqual(json.loads(imported), json.loads((KEYCLOAK / "nico-dev-realm.json").read_text()))
        frontend = {(obj['kind'], obj['metadata'].get('namespace'), obj['metadata']['name']) for obj in evaluated.objects}
        self.assertFalse(frontend.intersection((kind, 'custom-rest', name) for kind, name in by_identity))


    def test_database_password_change_rolls_workload_without_inline_credentials(self):
        def deployment(password):
            output = subprocess.check_output([
                "helm", "template", "keycloak", str(ROOT / "helm/nico-dev-keycloak"),
                "--set-json", "database.password=" + json.dumps(password)], text=True)
            return next(o for o in yaml.safe_load_all(output) if o["kind"] == "Deployment")
        before = deployment("original-password")
        after = deployment("replacement-password")
        before_hash = before["spec"]["template"]["metadata"]["annotations"].pop("checksum/database-credentials")
        after_hash = after["spec"]["template"]["metadata"]["annotations"].pop("checksum/database-credentials")
        self.assertNotEqual(before_hash, after_hash)
        self.assertEqual(before, after)
        self.assertNotIn("replacement-password", json.dumps(after))


if __name__ == "__main__":
    unittest.main()
