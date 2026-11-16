# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Protect the --skip-vault Core values preflight: one row per rule, independent of a live cluster."""

import io
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import types
import unittest

try:
    import yaml
except ImportError:  # the checker itself reports a missing PyYAML; the sync test below then skips
    yaml = None


CHECKER = Path(__file__).resolve().parents[1] / "check-vault-free.py"
_module = runpy.run_path(str(CHECKER))
check_vault_free = _module["check_vault_free"]

# A site TOML that satisfies every rule: Postgres-only credentials wrapped by an integrated KEK.
VAULT_FREE_SECRETS = """\
      [secrets]
      backends = ["postgres"]
      writer = "postgres"

      [secrets.kms]
      active = "site"

      [secrets.kms.providers.site]
      type = "integrated"
      keys = { "site-kek-1" = { env = "NICO_SECRETS_KEK" } }

      [secrets.routing]
      "/" = "site-kek-1"
"""


def base_config(body):
    """Render nico-api.configFiles.nicoApiConfig, the custom base config that replaces the chart's generated one."""
    return "  configFiles:\n    nicoApiConfig: |\n" + body


# A custom base config must carry the cert-manager flags and the request namespace itself: the generated config's
# values are gone.
CERT_MANAGER_FLAGS = "      [certificates]\n      use_cert_manager = true\n      use_cert_manager_ufm = true\n"
CERT_MANAGER_ON = CERT_MANAGER_FLAGS + '      [certificates.cert_manager]\n      namespace = "nico-system"\n'


def values(toml_body=VAULT_FREE_SECRETS, nico_api_extra="", top_extra="", site_enabled=None):
    """Render Core values in the umbrella shape with the given site TOML body and extra YAML."""
    out = top_extra
    out += "nico-api:\n"
    out += nico_api_extra
    out += "  siteConfig:\n"
    if site_enabled is not None:
        out += f"    enabled: {site_enabled}\n"
    out += "    nicoApiSiteConfig: |\n      sitename = \"site\"\n" + toml_body
    return out


class VaultFreePreflightTest(unittest.TestCase):
    """Each case exercises one rule of the checker, or one way of satisfying it."""

    def test_rules(self):
        cases = [
            # (name, values text, expected errors, expected warnings); each expectation is a fragment
            # of one message, or a tuple of fragments of the same message.
            ("Postgres-only secrets, chart defaults elsewhere: nothing to report", values(), [], []),
            ("no [secrets] section means the Vault credential store",
             values(toml_body=""), ["has no [secrets] section"], []),
            ("siteConfig disabled renders no TOML and so no [secrets]",
             values(site_enabled="false"), ["renders no site TOML, so there is no [secrets] section"], []),
            ("backends omitted defaults to vault",
             values(VAULT_FREE_SECRETS.replace('      backends = ["postgres"]\n', "")),
             ['[secrets] backends is omitted and defaults to ["vault"]'], []),
            ("backends still list vault",
             values(VAULT_FREE_SECRETS.replace('backends = ["postgres"]', 'backends = ["postgres", "vault"]')),
             ['[secrets] backends lists "vault"'], []),
            ("writer omitted defaults to vault",
             values(VAULT_FREE_SECRETS.replace('      writer = "postgres"\n', "")),
             ['[secrets] writer is omitted and defaults to "vault"'], []),
            ("writer set to vault",
             values(VAULT_FREE_SECRETS.replace('writer = "postgres"', 'writer = "vault"')),
             ['[secrets] writer = "vault"'], []),
            ("import_from keeps the Vault client required after the import",
             values(VAULT_FREE_SECRETS.replace('writer = "postgres"\n', 'writer = "postgres"\n      import_from = "vault"\n')),
             ['[secrets] import_from = "vault"'], []),
            ("[secrets.kms] is required with [secrets]",
             values('      [secrets]\n      backends = ["postgres"]\n      writer = "postgres"\n\n      [secrets.routing]\n      "/" = "site-kek-1"\n'),
             ["[secrets] has no [secrets.kms] table"], []),
            ("[secrets.kms] must name an active provider",
             values(VAULT_FREE_SECRETS.replace('      active = "site"\n', "")),
             ["[secrets.kms] active is missing"], []),
            ("the active provider must be defined",
             values(VAULT_FREE_SECRETS.replace('active = "site"', 'active = "prod"')),
             ['[secrets.kms] active = "prod" names no table under [secrets.kms.providers]'], []),
            ("[secrets.routing] is required with [secrets]",
             values(VAULT_FREE_SECRETS.replace('\n      [secrets.routing]\n      "/" = "site-kek-1"\n', "")),
             ["[secrets] has no [secrets.routing] table"], []),
            ("[secrets.routing] needs the catch-all entry",
             values(VAULT_FREE_SECRETS.replace('"/" = "site-kek-1"', '"machines/bmc" = "site-kek-1"')),
             ['[secrets.routing] has no "/" catch-all entry'], []),
            ("an inactive transit provider still builds a Vault client",
             values(VAULT_FREE_SECRETS + '\n      [secrets.kms.providers.old]\n      type = "transit"\n      keys = ["legacy"]\n'),
             ['[secrets.kms.providers.old] type = "transit"'], []),
            ("use_cert_manager = false in the site TOML defeats the chart flag",
             values(VAULT_FREE_SECRETS + "\n      [certificates]\n      use_cert_manager = false\n"),
             [("[certificates] use_cert_manager = false", "global.certificate.useCertManager=true")], []),
            ("use_cert_manager_ufm = false in the site TOML defeats the chart flag, InfiniBand or not",
             values(VAULT_FREE_SECRETS + "\n      [certificates]\n      use_cert_manager_ufm = false\n"),
             [("[certificates] use_cert_manager_ufm = false", "global.certificate.useCertManagerUfm=true")], []),
            ("both flags true with a dedicated_vault backend is ignored by nico-api",
             values(VAULT_FREE_SECRETS + '\n      [certificates]\n      backend = "dedicated_vault"\n'
                    "      use_cert_manager = true\n      use_cert_manager_ufm = true\n"), [], []),
            ("a custom base config drops the chart's cert-manager flags and namespace unless it sets them",
             values(nico_api_extra=base_config("      [site_explorer]\n      enabled = true\n")),
             [("[certificates] use_cert_manager is set in neither nico-api.configFiles.nicoApiConfig", "defaults it to false"),
              ("[certificates] use_cert_manager_ufm is set in neither nico-api.configFiles.nicoApiConfig", "defaults it to false"),
              ("[certificates.cert_manager] namespace is set in neither", 'defaults it to "forge-system"')], []),
            ("a custom base config with the flags but no request namespace falls back to forge-system",
             values(nico_api_extra=base_config(CERT_MANAGER_FLAGS)),
             [("[certificates.cert_manager] namespace is set in neither", 'set namespace = "nico-system"')], []),
            ("a custom base config requesting certificates outside the release namespace",
             values(nico_api_extra=base_config(CERT_MANAGER_FLAGS + '      [certificates.cert_manager]\n      namespace = "forge-system"\n')),
             [('[certificates.cert_manager] namespace = "forge-system" in nico-api.configFiles.nicoApiConfig',
               "setup.sh installs Core in nico-system")], []),
            ("the site TOML may supply the request namespace a custom base config omits",
             values(toml_body=VAULT_FREE_SECRETS + '\n      [certificates.cert_manager]\n      namespace = "nico-system"\n',
                    nico_api_extra=base_config(CERT_MANAGER_FLAGS)), [], []),
            ("a custom base config with both flags true and a site TOML without [certificates] passes",
             values(nico_api_extra=base_config(CERT_MANAGER_ON)), [], []),
            ("[secrets] may live in the custom base config instead of the site TOML",
             values(toml_body="", nico_api_extra=base_config(CERT_MANAGER_ON + VAULT_FREE_SECRETS)), [], []),
            ("the site TOML overrides the custom base config key by key, as nico-api merges them",
             values(toml_body='      [secrets]\n      backends = ["postgres"]\n',
                    nico_api_extra=base_config(CERT_MANAGER_ON + VAULT_FREE_SECRETS.replace(
                        'backends = ["postgres"]', 'backends = ["postgres", "vault"]'))),
             [], []),
            ("a Vault signer in the site TOML overrides the chart's site-issuer",
             values(VAULT_FREE_SECRETS + '\n      [certificates.cert_manager]\n      issuer_name = "vault-nico-issuer"\n'),
             [('[certificates.cert_manager] issuer_name = "vault-nico-issuer"', "in the site TOML")], []),
            ("a custom cert-manager signer is allowed with a note",
             values(VAULT_FREE_SECRETS + '\n      [certificates.cert_manager]\n      issuer_name = "corp-issuer"\n'),
             [], ['[certificates.cert_manager] issuer_name = "corp-issuer"']),
            ("a namespaced Issuer kind cannot reach the site-issuer ClusterIssuer",
             values(VAULT_FREE_SECRETS + '\n      [certificates.cert_manager]\n      issuer_name = "site-issuer"\n'
                                         '      issuer_kind = "Issuer"\n'),
             ['[certificates.cert_manager] issuer_kind = "Issuer"'], []),
            ("an omitted issuer_name is site-issuer, the nico-api default, so a namespaced kind alone is rejected too",
             values(nico_api_extra=base_config(CERT_MANAGER_ON + '      issuer_kind = "Issuer"\n')),
             [('[certificates.cert_manager] issuer_kind = "Issuer" in nico-api.configFiles.nicoApiConfig',
               'with issuer_name "site-issuer" (the nico-api default)')], []),
            ("a namespaced kind with a custom signer name is that signer's business",
             values(nico_api_extra=base_config(CERT_MANAGER_ON + '      issuer_name = "corp-issuer"\n      issuer_kind = "Issuer"\n')),
             [], ['[certificates.cert_manager] issuer_name = "corp-issuer"']),
            ("the top-level issuer is overridden by setup.sh, so the shipped default is fine",
             values(top_extra="global:\n  certificate:\n    issuerRef:\n      kind: ClusterIssuer\n      name: vault-nico-issuer\n"),
             [], []),
            ("the top-level issuer naming a third issuer is overridden too, and said so",
             values(top_extra="global:\n  certificate:\n    issuerRef:\n      name: corp-issuer\n"),
             [], [("global.certificate.issuerRef.name is \"corp-issuer\"", "--set global.certificate.issuerRef.name=site-issuer")]),
            ("a subchart-level global issuer is overridden by the parent's global, so it is a warning",
             values(nico_api_extra="  global:\n    certificate:\n      issuerRef:\n        name: vault-nico-issuer\n"),
             [], [("nico-api.global.certificate.issuerRef.name", "parent chart's global values precedence")]),
            ("a subchart-level global issuer already set to site-issuer needs no note",
             values(nico_api_extra="  global:\n    certificate:\n      issuerRef:\n        name: site-issuer\n"), [], []),
            ("nvSwitchTls pinned to the Vault issuer (the NICo client leaf renders by default)",
             values(nico_api_extra="  nvSwitchTls:\n    issuerRef:\n      kind: ClusterIssuer\n      name: vault-nico-issuer\n"),
             ['nico-api.nvSwitchTls.issuerRef.name is "vault-nico-issuer"'], []),
            ("nvSwitchTls with both leaves disabled is read by no Certificate",
             values(nico_api_extra="  nvSwitchTls:\n    nicoClient:\n      enabled: false\n    switchServer:\n      enabled: false\n"
                                   "    issuerRef:\n      name: vault-nico-issuer\n"), [], []),
            ("nvSwitchTls with only the switch server leaf enabled still counts",
             values(nico_api_extra="  nvSwitchTls:\n    nicoClient:\n      enabled: false\n    switchServer:\n      enabled: true\n"
                                   "    issuerRef:\n      name: vault-nico-issuer\n"),
             ['nico-api.nvSwitchTls.issuerRef.name is "vault-nico-issuer"'], []),
            ("a disabled optional subchart's issuer is read by nothing",
             values(top_extra="nico-ntp:\n  enabled: false\n  certificate:\n    issuerRef:\n      name: vault-nico-issuer\n"), [], []),
            ("an optional subchart that is off by default is read by nothing unless enabled",
             values(top_extra="unbound:\n  certificate:\n    issuerRef:\n      name: vault-nico-issuer\n"), [], []),
            ("an optional subchart that is enabled is read",
             values(top_extra="unbound:\n  enabled: true\n  certificate:\n    issuerRef:\n      name: vault-nico-issuer\n"),
             ['unbound.certificate.issuerRef.name is "vault-nico-issuer"'], []),
            ("enabled: false on a subchart the umbrella installs unconditionally is not a gate",
             values(top_extra="nico-dns:\n  enabled: false\n  certificate:\n    issuerRef:\n      name: vault-nico-issuer\n"),
             ['nico-dns.certificate.issuerRef.name is "vault-nico-issuer"'], []),
            ("enabled: false on nico-api is not a gate either",
             values(nico_api_extra="  enabled: false\n  machineCertificates:\n    issuerRef:\n      name: vault-nico-issuer\n"),
             ['nico-api.machineCertificates.issuerRef.name is "vault-nico-issuer"'], []),
            ("an enabled flag inside a value the charts do not gate on is not a gate",
             values(nico_api_extra="  extraCertificates:\n    - name: a\n      enabled: false\n      issuerRef:\n        name: vault-nico-issuer\n"),
             ['nico-api.extraCertificates[0].issuerRef.name is "vault-nico-issuer"'], []),
            ("backends must be a list",
             values(VAULT_FREE_SECRETS.replace('backends = ["postgres"]', 'backends = "postgres"')),
             ['[secrets] backends = "postgres" is not a list'], []),
            ("a site TOML that moves the request namespace off the release namespace",
             values(VAULT_FREE_SECRETS + '\n      [certificates.cert_manager]\n      namespace = "forge-system"\n'),
             [('[certificates.cert_manager] namespace = "forge-system" in the site TOML', "setup.sh installs Core in nico-system")], []),
            ("a site TOML that restates the release namespace is fine",
             values(VAULT_FREE_SECRETS + '\n      [certificates.cert_manager]\n      namespace = "nico-system"\n'), [], []),
            ("a namespaced Issuer kind on a values-level site-issuer reference",
             values(nico_api_extra="  nvSwitchTls:\n    issuerRef:\n      kind: Issuer\n      name: site-issuer\n      group: cert-manager.io\n"),
             ['nico-api.nvSwitchTls.issuerRef.kind is "Issuer" with name "site-issuer"; the bootstrap creates site-issuer as a ClusterIssuer, so set kind to "ClusterIssuer"'], []),
            ("machineCertificates defaults its name to site-issuer, so a kind alone is judged too",
             values(nico_api_extra="  machineCertificates:\n    issuerRef:\n      kind: Issuer\n"),
             [('nico-api.machineCertificates.issuerRef.kind is "Issuer" with name "site-issuer" (the chart default)',
               'set kind to "ClusterIssuer"')], []),
            ("machineCertificates pinned to the Vault issuer",
             values(nico_api_extra="  machineCertificates:\n    issuerRef:\n      name: vault-nico-issuer\n"),
             ['nico-api.machineCertificates.issuerRef.name is "vault-nico-issuer"'], []),
            ("another subchart's own certificate issuer",
             values(top_extra="nico-dns:\n  certificate:\n    issuerRef:\n      name: vault-nico-issuer\n"),
             ['nico-dns.certificate.issuerRef.name is "vault-nico-issuer"'], []),
            ("an issuer inside a list is reported with its index",
             values(nico_api_extra="  extraCertificates:\n    - name: a\n      issuerRef:\n        name: site-issuer\n"
                                   "    - name: b\n      issuerRef:\n        name: vault-nico-issuer\n"),
             ['nico-api.extraCertificates[1].issuerRef.name is "vault-nico-issuer"'], []),
            ("the bmc-proxy switch issuer is ignored while switchTls is disabled (the chart default)",
             values(top_extra="nico-bmc-proxy:\n  switchTls:\n    issuerRef:\n      name: vault-nico-issuer\n"), [], []),
            ("the bmc-proxy switch issuer defaults to the Vault issuer when switchTls is enabled without one",
             values(top_extra="nico-bmc-proxy:\n  switchTls:\n    enabled: true\n    uris:\n      - spiffe://nico.local/bmc-proxy\n"),
             [("nico-bmc-proxy.switchTls.issuerRef.name is absent", 'chart default "vault-nico-issuer"')], []),
            ("the bmc-proxy switch issuer counts once switchTls is enabled",
             values(top_extra="nico-bmc-proxy:\n  switchTls:\n    enabled: true\n    issuerRef:\n      name: vault-nico-issuer\n"),
             ['nico-bmc-proxy.switchTls.issuerRef.name is "vault-nico-issuer"'], []),
            ("an issuer other than the Vault one is accepted anywhere",
             values(nico_api_extra="  nvSwitchTls:\n    issuerRef:\n      name: nvswitch-issuer\n"), [], []),
            ("vaultClusterInfo is rejected by the chart without Vault",
             values(nico_api_extra="  vaultClusterInfo:\n    VAULT_SERVICE: https://vault.example.com\n"),
             ["nico-api.vaultClusterInfo is set"], []),
            ("an empty vaultClusterInfo is the chart default",
             values(nico_api_extra="  vaultClusterInfo: {}\n"), [], []),
            ("explicit false cert-manager flags are overridden, and said so",
             values(top_extra="global:\n  certificate:\n    useCertManager: false\n    useCertManagerUfm: false\n"),
             [], ["global.certificate.useCertManager is false", "global.certificate.useCertManagerUfm is false"]),
            ("an explicit nico-api.vault.enabled: true is overridden, and said so",
             values(nico_api_extra="  vault:\n    enabled: true\n"), [], ["nico-api.vault.enabled is true"]),
            ("an explicit nico-api.vault.enabled: false matches the override",
             values(nico_api_extra="  vault:\n    enabled: false\n"), [], []),
            ("extraEnv that overrides [certificates] at runtime",
             values(nico_api_extra="  extraEnv:\n    - name: CARBIDE_API_CERTIFICATES\n      value: '{use_cert_manager=false}'\n"),
             [("nico-api.extraEnv[0] sets CARBIDE_API_CERTIFICATES", "override both config files at runtime")], []),
            ("extraEnv that overrides a nested [secrets] key at runtime",
             values(nico_api_extra="  extraEnv:\n    - name: RUST_LOG\n      value: info\n    - name: CARBIDE_API_SECRETS__BACKENDS\n      value: '[\"vault\"]'\n"),
             [("nico-api.extraEnv[1] sets CARBIDE_API_SECRETS__BACKENDS", "put the setting in the site TOML")], []),
            ("extraEnv that selects the credential store",
             values(nico_api_extra="  extraEnv:\n    - name: CARBIDE_CREDENTIAL_STORE\n      value: vault\n"),
             ["nico-api.extraEnv[0] sets CARBIDE_CREDENTIAL_STORE, which selects the credential store"], []),
            ("extraEnv that hands nico-api a Vault address keeps it a Vault consumer",
             values(nico_api_extra="  extraEnv:\n    - name: VAULT_ADDR\n      value: https://vault.example.com:8200\n"),
             [("nico-api.extraEnv[0] sets VAULT_ADDR", "counts as a Vault consumer")], []),
            ("extraEnv entries that touch neither the config nor Vault are fine",
             values(nico_api_extra="  extraEnv:\n    - name: RUST_LOG\n      value: debug\n    - name: CARBIDE_API_DATABASE_URL\n"
                                   "      valueFrom:\n        secretKeyRef:\n          name: db\n          key: url\n"), [], []),
            ("no service account token means no Kubernetes client for the cert-manager provider",
             values(nico_api_extra="  automountServiceAccountToken: false\n"),
             [("nico-api.automountServiceAccountToken is false", "set it to true")], []),
            ("an explicit automountServiceAccountToken: true is the chart default",
             values(nico_api_extra="  automountServiceAccountToken: true\n"), [], []),
            ("every finding is reported, not only the first",
             values(toml_body="", nico_api_extra="  vaultClusterInfo:\n    VAULT_SERVICE: x\n  vault:\n    enabled: true\n"),
             ["has no [secrets] section", "nico-api.vaultClusterInfo is set"], ["nico-api.vault.enabled is true"]),
        ]
        for name, text, want_errors, want_warnings in cases:
            with self.subTest(name):
                errors, warnings = check_vault_free(io.StringIO(text))
                self.assertEqual(len(errors), len(want_errors), errors)
                for want, error in zip(want_errors, errors):
                    for fragment in ((want,) if isinstance(want, str) else want):
                        self.assertIn(fragment, error)
                self.assertEqual(len(warnings), len(want_warnings), warnings)
                for want, warning in zip(want_warnings, warnings):
                    for fragment in ((want,) if isinstance(want, str) else want):
                        self.assertIn(fragment, warning)

    def test_missing_parser_is_an_error(self):
        """The [secrets] section must be read in this mode, so an absent TOML parser is an error, not a warning."""
        saved = {name: sys.modules.get(name) for name in ("tomllib", "tomli")}
        sys.modules["tomllib"] = None
        sys.modules["tomli"] = None
        try:
            errors, warnings = check_vault_free(io.StringIO(values()))
        finally:
            for name, module in saved.items():
                if module is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = module
        self.assertEqual(warnings, [])
        self.assertEqual(len(errors), 1, errors)
        self.assertIn("python3 3.11 or later, or the tomli package", errors[0])

    def test_tomli_fallback_parses_the_config(self):
        """Without tomllib (Python before 3.11), the tomli package is used, so the documented fallback must work."""
        try:
            import tomllib as real_tomllib
        except ImportError:  # Python 3.10 with tomli: the fallback module is the real parser
            import tomli as real_tomllib

        fake_tomli = types.ModuleType("tomli")
        fake_tomli.loads = real_tomllib.loads
        fake_tomli.TOMLDecodeError = real_tomllib.TOMLDecodeError
        saved = {name: sys.modules.get(name) for name in ("tomllib", "tomli")}
        sys.modules["tomllib"] = None
        sys.modules["tomli"] = fake_tomli
        try:
            errors, warnings = check_vault_free(io.StringIO(values()))
            # The fallback parses, so a real finding is still reported through it.
            fallback_errors, _ = check_vault_free(io.StringIO(values(toml_body="")))
        finally:
            for name, module in saved.items():
                if module is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = module
        self.assertEqual((errors, warnings), ([], []))
        self.assertEqual(len(fallback_errors), 1, fallback_errors)
        self.assertIn("has no [secrets] section", fallback_errors[0])

    def test_optional_subchart_table_matches_the_umbrella_chart(self):
        """The checker's table of optional subcharts and their defaults must follow helm/Chart.yaml and helm/values.yaml."""
        repo = CHECKER.parents[1]
        chart_yaml = repo / "helm" / "Chart.yaml"
        values_yaml = repo / "helm" / "values.yaml"
        if yaml is None or not chart_yaml.is_file() or not values_yaml.is_file():
            self.skipTest("the umbrella chart is not checked out beside helm-prereqs")
        chart = yaml.safe_load(chart_yaml.read_text(encoding="utf-8"))
        conditions = {}
        for dependency in chart.get("dependencies", []):
            condition = dependency.get("condition")
            if condition:
                self.assertEqual(condition, f"{dependency['name']}.enabled", dependency)
                conditions[dependency["name"]] = True
        table = _module["OPTIONAL_SUBCHART_DEFAULTS"]
        self.assertEqual(set(table), set(conditions), "optional subcharts changed in helm/Chart.yaml")
        defaults = yaml.safe_load(values_yaml.read_text(encoding="utf-8"))
        for name, default in table.items():
            self.assertEqual(bool(defaults.get(name, {}).get("enabled", False)), default, name)

    def test_command_line_output(self):
        """preflight.sh parses the script's stdout line by line: ERROR: and WARNING: prefixes, exit 0 on findings,
        and a non-zero exit with one message when the file is unusable."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "core.yaml"
            path.write_text(values(toml_body="", nico_api_extra="  vault:\n    enabled: true\n"), encoding="utf-8")
            result = subprocess.run([sys.executable, str(CHECKER), str(path)], capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            lines = result.stdout.splitlines()
            self.assertEqual([line.split(":")[0] for line in lines], ["ERROR", "WARNING"], lines)
            self.assertIn("has no [secrets] section", lines[0])
            self.assertIn("nico-api.vault.enabled is true", lines[1])
            path.write_text(values(), encoding="utf-8")
            result = subprocess.run([sys.executable, str(CHECKER), str(path)], capture_output=True, text=True, check=False)
            self.assertEqual((result.returncode, result.stdout), (0, ""), result.stderr)
            result = subprocess.run([sys.executable, str(CHECKER), str(Path(tmp) / "missing.yaml")],
                                    capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 1)
            self.assertIn("Cannot check the Vault-free Core values", result.stderr)
            result = subprocess.run([sys.executable, str(CHECKER)], capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 1)
            self.assertIn("usage:", result.stderr)

    def test_malformed_values_raise(self):
        """Unusable input raises, so the preflight reports one error instead of a false pass."""
        cases = [
            ("site TOML that does not parse", values("      [secrets\n"), "is not TOML"),
            ("custom base config that does not parse", values(nico_api_extra=base_config("      [secrets\n")),
             "nico-api.configFiles.nicoApiConfig is not TOML"),
            ("site TOML that is not a string", "nico-api:\n  siteConfig:\n    nicoApiSiteConfig:\n      - a\n",
             "nicoApiSiteConfig must be a string"),
            ("nico-api is not a mapping", "nico-api: []\n", "nico-api must be a mapping"),
            ("extraEnv is not a list", values(nico_api_extra="  extraEnv:\n    name: X\n"), "nico-api.extraEnv must be a list"),
            ("an extraEnv entry has no name", values(nico_api_extra="  extraEnv:\n    - value: x\n"),
             "nico-api.extraEnv[0].name must be a string"),
            ("Core values are not a mapping", "- just\n- a list\n", "Core values must be a YAML mapping"),
        ]
        for name, text, fragment in cases:
            with self.subTest(name):
                with self.assertRaises(ValueError) as raised:
                    check_vault_free(io.StringIO(text))
                self.assertIn(fragment, str(raised.exception))


if __name__ == "__main__":
    unittest.main()
