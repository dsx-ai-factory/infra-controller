# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Protect the --skip-vault Core values preflight: one row per rule, independent of a live cluster."""

import io
from pathlib import Path
import runpy
import sys
import unittest


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


# A custom base config must carry the cert-manager flags itself: the generated config's true values are gone.
CERT_MANAGER_ON = "      [certificates]\n      use_cert_manager = true\n      use_cert_manager_ufm = true\n"


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
            ("a custom base config drops the chart's cert-manager flags unless it sets them",
             values(nico_api_extra=base_config("      [site_explorer]\n      enabled = true\n")),
             [("[certificates] use_cert_manager is set in neither nico-api.configFiles.nicoApiConfig", "defaults it to false"),
              ("[certificates] use_cert_manager_ufm is set in neither nico-api.configFiles.nicoApiConfig", "defaults it to false")], []),
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
            ("nvSwitchTls pinned to the Vault issuer",
             values(nico_api_extra="  nvSwitchTls:\n    issuerRef:\n      kind: ClusterIssuer\n      name: vault-nico-issuer\n"),
             ['nico-api.nvSwitchTls.issuerRef.name is "vault-nico-issuer"'], []),
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

    def test_malformed_values_raise(self):
        """Unusable input raises, so the preflight reports one error instead of a false pass."""
        cases = [
            ("site TOML that does not parse", values("      [secrets\n"), "is not TOML"),
            ("custom base config that does not parse", values(nico_api_extra=base_config("      [secrets\n")),
             "nico-api.configFiles.nicoApiConfig is not TOML"),
            ("site TOML that is not a string", "nico-api:\n  siteConfig:\n    nicoApiSiteConfig:\n      - a\n",
             "nicoApiSiteConfig must be a string"),
            ("nico-api is not a mapping", "nico-api: []\n", "nico-api must be a mapping"),
            ("Core values are not a mapping", "- just\n- a list\n", "Core values must be a YAML mapping"),
        ]
        for name, text, fragment in cases:
            with self.subTest(name):
                with self.assertRaises(ValueError) as raised:
                    check_vault_free(io.StringIO(text))
                self.assertIn(fragment, str(raised.exception))


if __name__ == "__main__":
    unittest.main()
