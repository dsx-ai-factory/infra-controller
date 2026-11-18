# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Protect the KMIP KEK provider preflight independently of values-file layout and live clusters."""

import io
import os
from pathlib import Path
import re
import runpy
import stat
import subprocess
import sys
import tempfile
import unittest


CHECKER = Path(__file__).resolve().parents[1] / "check-kmip-provider.py"
_module = runpy.run_path(str(CHECKER))
check_kmip_provider = _module["check_kmip_provider"]
kubectl_secret = _module["kubectl_secret"]
chart_mount_paths = _module["chart_mount_paths"]

PROVIDER = """\
      [secrets.kms.providers.hsm]
      type = "kmip"
      endpoint = "kms.example.com:5696"
      ca_bundle = "{ca}"
      client_cert = "{cert}"
      client_key = "{key}"
      keys = {{ "site-kek-1" = "f0a1c2d3" }}
"""


def values(toml_body="", mounts=None, volumes=None, site_enabled=None):
    """Render Core values with the given site TOML body and nico-api mount lists."""
    out = "nico-api:\n"
    if mounts is not None:
        out += "  extraVolumeMounts:\n" + mounts
    if volumes is not None:
        out += "  extraVolumes:\n" + volumes
    out += "  siteConfig:\n"
    if site_enabled is not None:
        out += f"    enabled: {site_enabled}\n"
    out += "    nicoApiSiteConfig: |\n      sitename = \"site\"\n" + toml_body
    return out


def provider(ca="/etc/nico/kmip/ca.pem", cert="/etc/nico/kmip/client.pem", key="/etc/nico/kmip/client.key"):
    return PROVIDER.format(ca=ca, cert=cert, key=key)


MOUNT = """\
    - name: kmip-client
      mountPath: /etc/nico/kmip
      readOnly: true
"""
VOLUME = """\
    - name: kmip-client
      secret:
        secretName: nico-kmip-client
        defaultMode: 0400
"""
OPERATOR_VOLUME = VOLUME.replace("nico-kmip-client", "operator-kmip")
ALL_KEYS = {"ca.pem", "client.pem", "client.key"}
MANAGED = {"keys": set(), "managed_by": "helm-prereqs"}  # setup.sh's own Secret from an earlier run


class KmipProviderPreflightTest(unittest.TestCase):
    """Each case exercises one distinct branch of the mount, Secret, or managed-Secret resolution."""

    def test_mount_and_secret_resolution(self):
        cases = [
            # (name, values, managed, secrets {name: keys or None}, expected errors, expected warnings);
            # each expectation is a fragment of one message, or a tuple of fragments of the same message.
            ("no kmip provider: nothing to check", values(), False, {}, [], []),
            ("managed Secret without a kmip provider is flagged",
             values(), True, {"nico-kmip-client": None}, [], ["no [secrets.kms.providers.*] table"]),
            ("paths under the mount and a complete Secret",
             values(provider(), MOUNT, VOLUME), False, {"nico-kmip-client": ALL_KEYS}, [], []),
            ("path outside every mount",
             values(provider(ca="/etc/pki/ca.pem"), MOUNT, VOLUME), False, {"nico-kmip-client": ALL_KEYS},
             ["ca_bundle = '/etc/pki/ca.pem' is not under any nico-api.extraVolumeMounts mountPath or chart-provided mount"], []),
            ("the site CA under a chart-provided mount needs no extra volume",
             values(provider(ca="/var/run/secrets/nico-roots/ca.crt"), MOUNT, VOLUME), False, {"nico-kmip-client": ALL_KEYS}, [], []),
            ("the boot-artifact store is a chart mount only while bootArtifactContainers is set",
             values(provider(ca="/forge-boot-artifacts/blobs/internal/ca.pem"), MOUNT, VOLUME)
             .replace("  siteConfig:", "  bootArtifactContainers:\n    - name: boot\n  siteConfig:"),
             False, {"nico-kmip-client": ALL_KEYS}, [], []),
            ("without bootArtifactContainers nothing mounts the boot-artifact store",
             values(provider(ca="/forge-boot-artifacts/blobs/internal/ca.pem"), MOUNT, VOLUME), False, {"nico-kmip-client": ALL_KEYS},
             ["ca_bundle = '/forge-boot-artifacts/blobs/internal/ca.pem' is not under any nico-api.extraVolumeMounts mountPath or chart-provided mount"], []),
            ("the NVSwitch client mount is chart-provided by default",
             values(provider(ca="/var/run/secrets/nvswitch-client/ca.crt"), MOUNT, VOLUME), False, {"nico-kmip-client": ALL_KEYS}, [], []),
            ("a null nvSwitchTls.nicoClient.enabled omits the mount, as Helm's if does",
             values(provider(ca="/var/run/secrets/nvswitch-client/ca.crt"), MOUNT, VOLUME)
             .replace("  siteConfig:", "  nvSwitchTls:\n    nicoClient:\n      enabled: null\n  siteConfig:"),
             False, {"nico-kmip-client": ALL_KEYS},
             ["ca_bundle = '/var/run/secrets/nvswitch-client/ca.crt' is not under any nico-api.extraVolumeMounts mountPath or chart-provided mount"], []),
            ("a chart mount below an enclosing extra mount serves the file",
             values(provider(ca="/var/run/secrets/nico-roots/ca.crt", cert="/var/run/secrets/kmip/client.pem",
                             key="/var/run/secrets/kmip/client.key"),
                    MOUNT.replace("/etc/nico/kmip", "/var/run/secrets"),
                    VOLUME.replace("        defaultMode: 0400\n",
                                   "        items:\n          - key: client.pem\n            path: kmip/client.pem\n"
                                   "          - key: client.key\n            path: kmip/client.key\n")),
             False, {"nico-kmip-client": {"client.pem", "client.key"}}, [], []),
            ("a file name starting with two dots is still under the mount",
             values(provider(ca="/etc/nico/kmip/..ca.pem"), MOUNT, VOLUME), False,
             {"nico-kmip-client": {"..ca.pem", "client.pem", "client.key"}}, [], []),
            ("relative path cannot be mounted",
             values(provider(key="etc/nico/kmip/client.key"), MOUNT, VOLUME), False, {"nico-kmip-client": ALL_KEYS},
             ["client_key = 'etc/nico/kmip/client.key' is not an absolute path"], []),
            ("mount without a volume of that name",
             values(provider(), MOUNT, None), False, {},
             ["entry 'kmip-client' (/etc/nico/kmip) has no nico-api.extraVolumes entry"], []),
            ("Secret missing: default name suggests the installer variable",
             values(provider(), MOUNT, VOLUME), False, {"nico-kmip-client": None},
             ["Secret nico-system/nico-kmip-client, mounted by nico-api.extraVolumes for "
              "[secrets.kms.providers.hsm] ca_bundle, [secrets.kms.providers.hsm] client_cert, "
              "[secrets.kms.providers.hsm] client_key, does not exist; set NICO_KMIP_CLIENT_DIR"], []),
            ("Secret missing: operator name suggests creating it",
             values(provider(), MOUNT, OPERATOR_VOLUME), False, {"operator-kmip": None},
             [("Secret nico-system/operator-kmip", "create it before running setup.sh")], []),
            ("Secret present but one key short",
             values(provider(), MOUNT, VOLUME), False, {"nico-kmip-client": {"ca.pem", "client.pem"}},
             ["has no key 'client.key', which [secrets.kms.providers.hsm] client_key need (keys present: ca.pem, client.pem)"], []),
            ("Secret lookup failure is reported, not treated as absent",
             values(provider(), MOUNT, VOLUME), False, {"nico-kmip-client": RuntimeError("forbidden")},
             [("could not inspect Secret nico-system/nico-kmip-client", "forbidden")], []),
            ("managed Secret absent: setup.sh will create it",
             values(provider(), MOUNT, VOLUME), True, {"nico-kmip-client": None}, [], []),
            ("managed Secret from an earlier run: ownership is enough, keys are not checked",
             values(provider(), MOUNT, VOLUME), True, {"nico-kmip-client": MANAGED}, [], []),
            ("managed Secret that an operator created is refused before setup.sh would abort",
             values(provider(), MOUNT, VOLUME), True, {"nico-kmip-client": ALL_KEYS},
             ["Secret nico-system/nico-kmip-client exists but is not managed by setup.sh"], []),
            ("managed Secret cannot serve a key setup.sh does not create",
             values(provider(ca="/etc/nico/kmip/ca.crt"), MOUNT, VOLUME), True, {"nico-kmip-client": None},
             ["need key 'ca.crt' of Secret nico-kmip-client, but setup.sh creates it with the keys ca.pem, client.pem, client.key only"], []),
            ("managed Secret that nothing mounts",
             values(provider(), MOUNT, OPERATOR_VOLUME), True, {"operator-kmip": ALL_KEYS, "nico-kmip-client": None}, [],
             ["no nico-api.extraVolumes entry mounts the Secret nico-kmip-client"]),
            ("items remap the key the path reads",
             values(provider(), MOUNT, VOLUME.replace("        defaultMode: 0400\n",
                                                      "        items:\n          - key: bundle\n            path: ca.pem\n")),
             False, {"nico-kmip-client": {"bundle"}},
             ["the secret volume's items map nothing to 'client.pem'", "the secret volume's items map nothing to 'client.key'"], []),
            ("subPath mounts one key as the file itself",
             values(provider(key="/etc/nico/kmip/client.key"),
                    MOUNT + "    - name: kmip-key\n      mountPath: /etc/nico/kmip/client.key\n      subPath: tls.key\n",
                    VOLUME + OPERATOR_VOLUME.replace("kmip-client", "kmip-key")),
             False, {"nico-kmip-client": ALL_KEYS, "operator-kmip": {"tls.key"}}, [], []),
            ("every key an items projection lists must exist, not only the three the paths read",
             values(provider(), MOUNT, VOLUME.replace("        defaultMode: 0400\n",
                                                      "        items:\n          - key: ca.pem\n            path: ca.pem\n"
                                                      "          - key: client.pem\n            path: client.pem\n"
                                                      "          - key: client.key\n            path: client.key\n"
                                                      "          - key: passphrase\n            path: passphrase\n")),
             False, {"nico-kmip-client": ALL_KEYS},
             ["has no key 'passphrase', which nico-api.extraVolumes entry 'kmip-client' items need"], []),
            ("a file in a subdirectory of the mount needs items",
             values(provider(ca="/etc/nico/kmip/ca/ca.pem"), MOUNT, VOLUME), False, {"nico-kmip-client": ALL_KEYS},
             ["'ca/ca.pem' is in a subdirectory of the volume"], []),
            ("a directory subPath prefixes the paths that items map",
             values(provider(), MOUNT.replace("      readOnly: true\n", "      subPath: tls\n"),
                    VOLUME.replace("        defaultMode: 0400\n",
                                   "        items:\n          - key: bundle\n            path: tls/ca.pem\n"
                                   "          - key: cert\n            path: tls/client.pem\n"
                                   "          - key: key\n            path: tls/client.key\n")),
             False, {"nico-kmip-client": {"bundle", "cert", "key"}}, [], []),
            ("non-Secret volume is a warning",
             values(provider(), MOUNT, "    - name: kmip-client\n      hostPath:\n        path: /srv/kmip\n"), False, {},
             [], ["comes from volume 'kmip-client' of kind hostPath, not a Secret"]),
            ("siteConfig.enabled false renders no TOML",
             values(provider(), MOUNT, VOLUME, site_enabled="false"), False, {}, [], []),
            ("siteConfig.enabled null renders no TOML either",
             values(provider(), MOUNT, VOLUME, site_enabled="null"), False, {}, [], []),
        ]
        for name, text, managed, secrets, want_errors, want_warnings in cases:
            with self.subTest(name):
                asked = []

                def lookup(secret_name, secrets=secrets, asked=asked):
                    asked.append(secret_name)
                    self.assertIn(secret_name, secrets, f"unexpected Secret lookup {secret_name!r}")
                    outcome = secrets[secret_name]
                    if isinstance(outcome, Exception):
                        raise outcome
                    if isinstance(outcome, set):  # an operator's Secret: these keys, no setup.sh label
                        return {"keys": outcome, "managed_by": None}
                    return outcome

                errors, warnings = check_kmip_provider(io.StringIO(text), managed_secret=managed, lookup_secret=lookup)
                self.assertEqual(len(errors), len(want_errors), errors)
                for want, error in zip(want_errors, errors):
                    for fragment in ((want,) if isinstance(want, str) else want):
                        self.assertIn(fragment, error)
                self.assertEqual(len(warnings), len(want_warnings), warnings)
                for want, warning in zip(want_warnings, warnings):
                    for fragment in ((want,) if isinstance(want, str) else want):
                        self.assertIn(fragment, warning)
                self.assertEqual(set(asked), set(secrets), "the checker must consult exactly the Secrets the case lists")

    def test_toml_is_parsed_only_when_kmip_is_mentioned(self):
        """A site config without the word kmip never needs the TOML parser; one that mentions it is parsed fully."""
        errors, warnings = check_kmip_provider(io.StringIO(values("      [secrets\n")), lookup_secret=None)
        self.assertEqual((errors, warnings), ([], []))
        with self.assertRaises(ValueError) as raised:
            check_kmip_provider(io.StringIO(values("      # kmip\n      [secrets\n")), lookup_secret=None)
        self.assertIn("is not TOML", str(raised.exception))

    def test_missing_parser_warns_unless_the_operator_opted_in(self):
        """Without tomllib or tomli the checker cannot tell whether a provider is declared, so it warns and goes
        on; with NICO_KMIP_CLIENT_DIR set the opt-in is explicit, and a provider it cannot check is an error."""
        saved = {name: sys.modules.get(name) for name in ("tomllib", "tomli")}
        sys.modules["tomllib"] = None
        sys.modules["tomli"] = None
        try:
            for managed in (False, True):
                with self.subTest(managed_secret=managed):
                    errors, warnings = check_kmip_provider(
                        io.StringIO(values("      # no kmip here\n")), managed_secret=managed,
                        lookup_secret=lambda name: None)
                    findings, silent = (errors, warnings) if managed else (warnings, errors)
                    self.assertEqual(silent, [])
                    self.assertEqual(len(findings), 1, (errors, warnings))
                    self.assertIn("python3 3.11 or later, or the tomli package", findings[0])
                    if managed:
                        self.assertIn("NICO_KMIP_CLIENT_DIR is set", findings[0])
        finally:
            for name, module in saved.items():
                if module is None:
                    sys.modules.pop(name, None)
                else:
                    sys.modules[name] = module

    def test_malformed_values_raise(self):
        cases = [
            ("site TOML that does not parse", values("      # KMIP\n      [secrets\n"), "is not TOML"),
            ("extraVolumes is not a list", values(provider(), MOUNT, None).replace("  siteConfig:", "  extraVolumes: {}\n  siteConfig:"),
             "nico-api.extraVolumes must be a list"),
            ("Core values are not a mapping", "- just\n- a list\n", "Core values must be a YAML mapping"),
        ]
        for name, text, fragment in cases:
            with self.subTest(name):
                with self.assertRaises(ValueError) as raised:
                    check_kmip_provider(io.StringIO(text), lookup_secret=lambda name: {"keys": ALL_KEYS, "managed_by": None})
                self.assertIn(fragment, str(raised.exception))

    def test_chart_mount_paths_match_the_deployment_template(self):
        """Every literal mountPath the chart renders for nico-api must be covered by the checker's list."""
        template = (Path(__file__).resolve().parents[2] / "helm/charts/nico-api/templates/deployment.yaml").read_text()
        literal = {m.strip('"') for m in re.findall(r"mountPath:\s*(\S+)", template) if "{{" not in m}
        self.assertTrue(literal)
        known = chart_mount_paths({"bootArtifactContainers": [{"name": "boot"}]})
        for path in sorted(literal):
            self.assertTrue(any(path == chart or path.startswith(chart + "/") for chart in known),
                            f"{path} is rendered by the chart but unknown to chart_mount_paths")

    def test_kubectl_secret_lookup(self):
        """Map kubectl's outcomes onto keys and ownership, absence, and failure without a live cluster."""
        with tempfile.TemporaryDirectory() as tmp:
            kubectl = Path(tmp) / "kubectl"
            kubectl.write_text(
                "#!/bin/sh\n"
                'case "${FAKE_OUTCOME}" in\n'
                '  present) printf \'{"metadata":{"labels":{"app.kubernetes.io/managed-by":"helm-prereqs"}},"data":{"ca.pem":"","client.pem":""}}\' ;;\n'
                '  absent) echo \'Error from server (NotFound): secrets "x" not found\' >&2; exit 1 ;;\n'
                '  *) echo \'error: You must be logged in to the server\' >&2; exit 1 ;;\n'
                "esac\n")
            kubectl.chmod(kubectl.stat().st_mode | stat.S_IXUSR)
            env = dict(os.environ, PATH=f"{tmp}{os.pathsep}{os.environ.get('PATH', '')}")
            script = ("import runpy, sys; m = runpy.run_path(sys.argv[1]); "
                      "r = m['kubectl_secret']('x'); print(None if r is None else (sorted(r['keys']), r['managed_by']))")
            for outcome, expected in (("present", "(['ca.pem', 'client.pem'], 'helm-prereqs')"), ("absent", "None")):
                result = subprocess.run([sys.executable, "-c", script, str(CHECKER)], env=dict(env, FAKE_OUTCOME=outcome),
                                        capture_output=True, text=True, check=False)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.strip(), expected)
            result = subprocess.run([sys.executable, "-c", script, str(CHECKER)], env=dict(env, FAKE_OUTCOME="denied"),
                                    capture_output=True, text=True, check=False)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("RuntimeError: error: You must be logged in", result.stderr)


if __name__ == "__main__":
    unittest.main()
