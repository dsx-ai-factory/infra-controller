# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check Tilt prerequisite inputs against the shared Helm chart contract."""

from pathlib import Path
import subprocess
import unittest

import yaml

from tilt_test_support import TiltResult, baseline

ROOT = Path(__file__).resolve().parents[3]


class LocalPrerequisitesTest(unittest.TestCase):
    def test_dpf_profile_preserves_native_image_and_vault_prerequisites(self):
        for architecture, platform in [('aarch64', 'linux/arm64'), ('x86_64', 'linux/amd64')]:
            with self.subTest(architecture=architecture):
                result = TiltResult({'dpf': True}, architecture=architecture)
                self.assertEqual(result.images['dpf-sim-controller']['platform'], platform)
                self.assertIn('dpf-sim-controller', result.enabled)
                self.assertIn('dpf-crds-ready', result.resources['nico-api']['resource_deps'])
                self.assertTrue(result.env['nico_api_values']['automountServiceAccountToken'])
                self.assertTrue(result.env['nico_api_values']['dpf']['rbacCreate'])
                self.assertIn('[dpf]\nenabled = true', result.env['nico_api_values']['siteConfig']['nicoApiSiteConfig'])
                self.assertIn('auth/kubernetes/role/nico-api', result.env['vault_values']['server']['postStart'][-1])
                self.assertTrue(any(obj and obj['kind'] == 'CustomResourceDefinition' for obj in result.objects))

    def test_dpf_rejects_persistent_vault_without_kubernetes_auth(self):
        with self.assertRaisesRegex(ValueError, 'DPF profile requires ephemeral Vault'):
            TiltResult({'dpf': True, 'persistent-vault': True})

    def test_local_resources_use_shared_chart_with_preserved_database_contract(self):
        values = baseline()
        config = values['development']['localConfig']
        config.update(restNamespace='custom-rest', temporalNamespace='custom-temporal')
        config['database']['rest'].update(name='custom_db', user='custom_owner', password='custom-password')
        config['database']['temporal'].update(visibilityResourceName='custom-visibility', visibilityName='custom_visibility')
        result = TiltResult({'rest': True, 'observability': True}, values=values)
        renders = [command for command in result.commands
                   if command[:3] == ['helm', 'template', 'nico-dev-prereqs']]
        self.assertEqual(len(renders), 5)
        objects = {(obj['kind'], obj['metadata'].get('namespace'), obj['metadata']['name']): obj
                   for obj in result.objects if obj}
        database = objects[('Database', 'nico-system', 'nico-rest')]
        self.assertEqual(database['spec'], {'name': 'custom_db', 'owner': 'custom_owner',
                         'cluster': {'name': 'nico-pg-cluster'},
                         'extensions': [{'name': 'pg_trgm', 'ensure': 'present'}]})
        credentials = objects[('Secret', 'nico-system', 'nico-rest-db-user')]
        self.assertEqual(credentials['stringData'], {'username': 'custom_owner', 'password': 'custom-password'})
        self.assertEqual(credentials['metadata']['labels'], {'cnpg.io/reload': 'true'})
        for component in ('keycloak', 'temporal'):
            database_config = config['database'][component]
            credentials = objects[('Secret', 'nico-system', component + '-db-user')]
            self.assertEqual(credentials['stringData'], {
                'username': database_config['user'], 'password': database_config['password']})
            self.assertEqual(credentials['type'], 'kubernetes.io/basic-auth')
            self.assertEqual(credentials['metadata']['labels'], {'cnpg.io/reload': 'true'})
        self.assertEqual(objects[('Secret', 'custom-temporal', 'db-creds')]['stringData'],
                         {'password': config['database']['temporal']['password']})
        self.assertEqual(objects[('Database', 'nico-system', 'custom-visibility')]['spec'], {
            'name': 'custom_visibility', 'owner': config['database']['temporal']['user'],
            'cluster': {'name': 'nico-pg-cluster'}})
        wait = result.resources['rest-databases-ready']['command']
        self.assertEqual([arg for arg in wait if arg.startswith('database/')],
                         ['database/nico-rest', 'database/keycloak', 'database/temporal', 'database/custom-visibility'])
        self.assertIn(('Namespace', None, 'custom-rest'), objects)
        self.assertIn(('Namespace', None, 'custom-temporal'), objects)
        dashboards = objects[('ConfigMap', 'observability', 'grafana-dashboards-nico')]
        self.assertEqual(len(dashboards['data']), 8)
        self.assertEqual(dashboards['metadata']['labels'], {'grafana_dashboard': '1'})
        self.assertEqual(result.k8s_resources['rest-databases']['resource_deps'],
                         ['nico-postgres', 'nico-local-config'])
        # Rendering must not create a Helm release or a second owner of these objects.
        self.assertNotIn('nico-dev-prereqs', result.resources)
        for obj in (database, credentials, dashboards):
            self.assertNotIn('helm.sh/hook', obj['metadata'].get('annotations', {}))
            self.assertNotIn('meta.helm.sh/release-name', obj['metadata'].get('annotations', {}))

    def test_profile_preserves_core_contract_without_site_bootstrap_writers(self):
        for persistent in (False, True):
            with self.subTest(persistent=persistent):
                self.check_profile(persistent)

    def check_profile(self, persistent):
        ca = ROOT / "dev/certs/localhost/ca.crt"
        key = ROOT / "dev/certs/localhost/ca.key"
        ssh_key = ROOT / "crates/ssh-console/tests/fixtures/ssh_host_ed25519_key"
        result = TiltResult({'persistent-vault': persistent, 'rest': False, 'mcp': False})
        resource = result.resources['nico-core-prereqs']
        self.assertNotIn("nico-local-namespaces", resource["resource_deps"])
        # Remove only installation controls; preserve all value-affecting flags.
        render_flags = [flag for flag in resource["flags"]
                        if flag not in ("--create-namespace", "--wait")
                        and not flag.startswith("--timeout=")]
        result = subprocess.run(
            ["helm", "template", resource["release_name"], resource["chart"],
             "--namespace", resource["namespace"], *render_flags],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        resources = list(filter(None, yaml.safe_load_all(result.stdout)))
        by_name = {r["metadata"]["name"]: r for r in resources}
        expected = {
            "nico-local-ca", "nico-roots", "local-ca-issuer",
            "ssh-host-key", "nico-vault-token", "nico-vault-approle-tokens",
            "vault-cluster-info", "nico-system-nico-database-config",
        }
        if persistent:
            expected.remove("nico-vault-token")
            self.assertIn("vault-bootstrap", resource["resource_deps"])
        self.assertEqual(set(by_name), expected)
        self.assertEqual(len(resources), len(by_name))
        for resource in resources:
            self.assertEqual(resource["metadata"]["namespace"], "nico-system")
            self.assertNotIn("helm.sh/hook", resource["metadata"].get("annotations", {}))
        self.assertEqual(by_name["nico-local-ca"]["stringData"], {
            "tls.crt": ca.read_text(), "tls.key": key.read_text(),
        })
        issuer = by_name["local-ca-issuer"]
        tilt = yaml.safe_load((ROOT / "dev/deployment/stack/values.yaml").read_text())
        self.assertEqual(tilt["global"]["certificate"]["issuerRef"]["name"], issuer["metadata"]["name"])
        self.assertEqual(tilt["global"]["certificate"]["issuerRef"]["kind"], issuer["kind"])
        self.assertEqual(by_name["vault-cluster-info"]["data"], {
            "VAULT_SERVICE": "http://nico-vault.nico-system.svc.cluster.local:8200",
            "NICO_VAULT_MOUNT": "secrets", "NICO_VAULT_PKI_MOUNT": "certs",
        })
        if not persistent:
            self.assertEqual(by_name["nico-vault-token"]["stringData"]["token"],
                             tilt["development"]["vault"]["server"]["dev"]["devRootToken"])
        self.assertEqual(by_name["ssh-host-key"]["stringData"], {
            "ssh_host_ed25519_key": ssh_key.read_text(),
            "ssh_host_ed25519_key_pub": Path(str(ssh_key) + ".pub").read_text(),
        })



    def test_persistent_vault_graph_orders_bootstrap_and_public_ca_export(self):
        for persistent in (False, True):
            with self.subTest(persistent=persistent):
                result = TiltResult({'persistent-vault': persistent})
                resources, environment = result.resources, result.env
                expected = 'vault-bootstrap' if persistent else 'vault'
                self.assertEqual(resources['vault-admin-ca']['resource_deps'], [expected])
                self.assertIn(environment['vault_command'] + ['check-mode', '--mode',
                              'persistent' if persistent else 'ephemeral'], result.commands)
                self.assertEqual('vault-bootstrap' in result.enabled, persistent)
                self.assertEqual('vault-bootstrap' in resources, persistent)
                self.assertEqual(environment['vault_values']['server']['dev']['enabled'], not persistent)
                if persistent:
                    self.assertEqual(resources['vault-bootstrap']['resource_deps'], ['vault'])
                    self.assertEqual(resources['vault-bootstrap']['command'], environment['vault_command'] + ['bootstrap'])

    def test_rest_trust_rejects_incompatible_issuer_references_at_load(self):
        cases = [
            ("supported issuer", True, {"kind": "ClusterIssuer", "group": "cert-manager.io"}, False),
            ("namespaced issuer", True, {"kind": "Issuer", "group": "cert-manager.io"}, True),
            ("external issuer", True, {"kind": "ClusterIssuer", "group": "external.example"}, True),
            ("default API group", True, {"kind": "ClusterIssuer"}, False),
            ("missing required kind", True, {"group": "cert-manager.io"}, True),
            ("REST disabled", False, {"kind": "Issuer", "group": "external.example"}, False),
        ]
        for name, enabled, issuer, rejected in cases:
            with self.subTest(name=name):
                values = baseline()
                values['development']['rest']['global']['certificate']['issuerRef'] = dict(name='nico-rest-ca-issuer', **issuer)
                if rejected:
                    with self.assertRaisesRegex(ValueError, 'Local REST trust requires'):
                        TiltResult({'rest': enabled, 'mcp': False}, values=values)
                else:
                    TiltResult({'rest': enabled, 'mcp': False}, values=values)

    def test_rest_trust_preserves_certificate_contract_and_forwards_namespaces(self):
        values = baseline()
        values['development']['localConfig'].update(restNamespace='custom-rest', temporalNamespace='custom-temporal')
        values['development']['rest']['global']['certificate']['issuerRef']['name'] = 'custom-rest-ca'
        evaluated = TiltResult({'rest': True}, values=values)
        resource = evaluated.resources['nico-core-prereqs']
        self.assertIn("nico-local-namespaces", resource["resource_deps"])
        self.assertIn("cert-manager", resource["resource_deps"])
        self.assertIn(str(ROOT / "dev/deployment/stack/values.yaml"), resource["deps"])
        render_flags = [flag for flag in resource["flags"]
                        if flag not in ("--create-namespace", "--wait")
                        and not flag.startswith("--timeout=")]
        result = subprocess.run(
            ["helm", "template", resource["release_name"], resource["chart"],
             "--namespace", resource["namespace"], *render_flags],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        resources = list(filter(None, yaml.safe_load_all(result.stdout)))
        by_identity = {(r["kind"], r["metadata"].get("namespace"), r["metadata"]["name"]): r
                       for r in resources}
        self.assertEqual(len(resources), len(by_identity))
        self.assertEqual(len(resources), 14)
        for namespace in ("cert-manager", "custom-rest"):
            ca = by_identity[("Secret", namespace, "ca-signing-secret")]
            self.assertEqual(ca["type"], "kubernetes.io/tls")
            self.assertEqual(ca["stringData"], {
                "tls.crt": (ROOT / "dev/certs/localhost/ca.crt").read_text(),
                "tls.key": (ROOT / "dev/certs/localhost/ca.key").read_text(),
            })
        issuer = by_identity[("ClusterIssuer", None, "custom-rest-ca")]
        self.assertEqual(issuer["spec"], {"ca": {"secretName": "ca-signing-secret"}})
        # Preserve the entire existing TLS contract, adjusting only the explicit
        # namespace and issuer inputs. This covers every SAN and mounted Secret.
        old_certificates = yaml.safe_load_all((ROOT /
            "rest-api/deploy/kustomize/base/temporal-helm/certificates.yaml").read_text())
        for old in old_certificates:
            expected = old["spec"]
            expected["issuerRef"]["name"] = "custom-rest-ca"
            expected["dnsNames"] = [".".join("custom-temporal" if part == "temporal" else part
                                             for part in name.split("."))
                                    if ".server.temporal.local" not in name else name
                                    for name in expected["dnsNames"]]
            actual = by_identity[("Certificate", "custom-temporal", old["metadata"]["name"])]
            self.assertEqual(actual["spec"], expected)
        self.assertNotIn('ca-signing-secret', [obj['metadata']['name'] for obj in evaluated.objects])
        self.assertEqual(evaluated.resources['temporal-certificates-ready']['resource_deps'], ['nico-core-prereqs'])

    def test_rest_common_owns_secrets_and_certificate_with_forwarded_inputs(self):
        values = baseline()
        tilt = values['development']
        tilt['localConfig']['restNamespace'] = 'custom-rest'
        tilt['localConfig']['database']['rest'].update(user='custom-user', password='custom-db-value')
        tilt['localConfig']['keycloak']['clientSecret'] = 'custom-client-value'
        tilt['localConfig']['temporal']['encryptionKey'] = 'custom-encryption-value'
        evaluated = TiltResult({'rest': True}, values=values)
        resource = evaluated.resources['nico-rest-common']
        self.assertEqual(resource["pod_readiness"], "ignore")
        self.assertIn("nico-local-config", resource["resource_deps"])
        render_flags = [flag for flag in resource["flags"]
                        if flag not in ("--create-namespace", "--wait")
                        and not flag.startswith("--timeout=")]
        result = subprocess.run(
            ["helm", "template", resource["release_name"], resource["chart"],
             "--namespace", resource["namespace"], *render_flags],
            capture_output=True, text=True, timeout=30,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        resources = list(filter(None, yaml.safe_load_all(result.stdout)))
        by_name = {r["metadata"]["name"]: r for r in resources}
        names = {"db-creds", "keycloak-client-secret", "temporal-encryption-key",
                 "image-pull-secret", "temporal-client-cloud-cert"}
        self.assertEqual(set(by_name), names)
        self.assertEqual(len(resources), len(names))
        for obj in resources:
            self.assertEqual(obj["metadata"]["namespace"], "custom-rest")
            self.assertNotIn("helm.sh/hook", obj["metadata"].get("annotations", {}))
        self.assertEqual(by_name["db-creds"]["stringData"], {
            "username": "custom-user", "password": "custom-db-value"})
        self.assertEqual(by_name["keycloak-client-secret"]["stringData"], {
            "keycloak-client-secret": "custom-client-value"})
        self.assertEqual(by_name["temporal-encryption-key"]["stringData"], {
            "temporal-encryption-key": "custom-encryption-value"})
        self.assertEqual(by_name["temporal-client-cloud-cert"]["spec"]["issuerRef"],
                         tilt["rest"]["global"]["certificate"]["issuerRef"])
        # The evaluated frontend must not remain a second writer for these objects.
        rendered = {obj['metadata']['name'] for obj in evaluated.objects
                    if obj['metadata'].get('namespace') == 'custom-rest'}
        self.assertFalse(names.intersection(rendered))
        self.assertIn('nico-rest-common', evaluated.k8s_resources['nico-rest-db-migration-dev']['resource_deps'])
        self.assertEqual(evaluated.resources['rest-certificate-ready']['resource_deps'], ['nico-rest-common'])


if __name__ == "__main__":
    unittest.main()
