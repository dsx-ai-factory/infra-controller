# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Protect the machine-a-tron ServiceCIDR preflight independently of values-file layout and live clusters."""

import io
import os
from pathlib import Path
import runpy
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest


CHECKER = Path(__file__).resolve().parents[1] / "check-mat-service-cidr.py"
check_service_cidr = runpy.run_path(str(CHECKER))["check_service_cidr"]

SITE_CONFIG = """\
nico-api:
  siteConfig:
    nicoApiSiteConfig: |
      [networks.simulated-oob]
      type = "underlay"
      prefix = "10.200.0.0/18"
      gateway = "10.200.0.1"

      [networks.mat-bmc-0]
      type = "underlay"
      prefix = "10.200.64.0/20"
      gateway = "10.200.64.1"

      [networks.wide]
      type = "underlay"
      prefix = "10.200.0.0/16"
      gateway = "10.200.255.254"

      [networks.simulated-underlay]
      type = "underlay"
      prefix = "10.202.0.0/18"
      gateway = "10.202.0.1"

      [networks.nvos-inside-servicecidr]
      type = "underlay"
      prefix = "10.233.1.0/24"
      gateway = "10.233.1.1"
"""

# A fake kubectl answers each ServiceCIDR source from an environment variable and records its calls.
FAKE_KUBECTL = """\
#!/bin/sh
printf '%s\\n' "$*" >> "${KUBECTL_LOG}"
case "$*" in
  "get servicecidrs "*) out="${FAKE_SERVICECIDRS-}" ;;
  "get cm kubeadm-config "*) out="${FAKE_KUBEADM-}" ;;
  "cluster-info dump") out="${FAKE_DUMP-}" ;;
  *) exit 2 ;;
esac
[ -n "$out" ] || exit 1
printf '%s' "$out"
"""


def machines_values(relay="10.200.0.1", key="bmcDhcpRelayAddress", underlay=None, underlay_key="underlayDhcpRelayAddress"):
    """Values for one machine-a-tron pod with a BMC relay and an optional underlay relay."""
    values = f"pods:\n  mat-0:\n    machines:\n      compute:\n        hwType: dell_poweredge_r750\n        hostCount: 10\n        {key}: \"{relay}\"\n"
    if underlay is not None:
        values += f"        {underlay_key}: \"{underlay}\"\n"
    return values


class MatServiceCidrTest(unittest.TestCase):
    """Resolve every BMC relay to a declared network and refuse networks inside the ServiceCIDR."""

    def test_relay_resolution_and_overlap(self):
        """Check the resolved BMC networks, not the relay addresses, against every Service CIDR."""
        cases = [
            # The relay's own segment is the checked network.
            ("outside the ServiceCIDR", machines_values(), ["10.96.0.0/12"], [], ["10.200.0.0/18"], []),
            # A relay inside a nested declaration resolves to the most specific prefix.
            ("most specific prefix wins", machines_values("10.200.64.1"), ["10.96.0.0/12"], [],
             ["10.200.64.0/20"], []),
            # Any overlap with any Service CIDR is an error, whichever is wider.
            ("overlap", machines_values(), ["10.233.0.0/18", "10.200.0.0/16"], [], ["10.200.0.0/18"],
             ["BMC network 10.200.0.0/18 overlaps the cluster ServiceCIDR 10.200.0.0/16"]),
            # A relay outside every declared network cannot be vouched for.
            ("unresolved relay", machines_values("10.201.0.1"), ["10.96.0.0/12"], [], [],
             ["pods.mat-0.machines.compute: cannot determine the BMC network of relay 10.201.0.1; "
              "declare its [networks.*] prefix in the site config or set SCALE_BMC_PREFIXES"]),
            # Extra prefixes resolve relays the site config does not declare and are checked themselves.
            ("SCALE_BMC_PREFIXES", machines_values("10.201.0.1"), ["10.96.0.0/12"], ["10.201.0.0/18", "10.96.64.0/20"],
             ["10.96.64.0/20", "10.201.0.0/18"],
             ["BMC network 10.96.64.0/20 overlaps the cluster ServiceCIDR 10.96.0.0/12"]),
            # Rack groups use the snake_case relay key of the rendered TOML.
            ("rack groups", """\
pods:
  mat-0:
    machines:
      rack-machines: null
    racks:
      gb200:
        type: wiwynn_gb200_nvl72
        rack_profile_id: NVL72
        ids: [rack-001]
        bmc_dhcp_relay_address: 10.200.64.1
""", ["10.96.0.0/12"], [], ["10.200.64.0/20"], []),
            # The deprecated alias still names the BMC relay.
            ("deprecated alias", machines_values(key="oobDhcpRelayAddress"), ["10.96.0.0/12"], [],
             ["10.200.0.0/18"], []),
            # Switch NVOS leases are published as externalIPs too, so the underlay relay's network is checked.
            ("NVOS network checked alongside the BMC network", machines_values(underlay="10.202.0.1"),
             ["10.96.0.0/12"], [], ["10.200.0.0/18", "10.202.0.0/18"], []),
            ("NVOS network overlap", machines_values(underlay="10.233.1.1"), ["10.233.0.0/18"], [],
             ["10.200.0.0/18", "10.233.1.0/24"],
             ["NVOS network 10.233.1.0/24 overlaps the cluster ServiceCIDR 10.233.0.0/18"]),
            ("unresolved NVOS relay", machines_values(underlay="10.250.0.1"), ["10.96.0.0/12"], [],
             ["10.200.0.0/18"],
             ["pods.mat-0.machines.compute: cannot determine the NVOS network of relay 10.250.0.1; "
              "declare its [networks.*] prefix in the site config or set SCALE_BMC_PREFIXES"]),
            # The underlay relay's deprecated alias names it too; an unset underlay relay is not an error.
            ("NVOS deprecated alias", machines_values(underlay="10.202.0.1", underlay_key="adminDhcpRelayAddress"),
             ["10.96.0.0/12"], [], ["10.200.0.0/18", "10.202.0.0/18"], []),
            # Null pods and null groups disable chart defaults and carry no relay.
            ("null pods and groups", "pods:\n  default: null\n  mat-0:\n    machines:\n      rack-machines: null\n",
             ["10.96.0.0/12"], [], [], []),
            # Without the controller no Service publishes the inherited default group.
            ("Override Mode without pods", "image:\n  tag: dev\n", ["10.96.0.0/12"], [], [], []),
            # Tokens that are not CIDRs are reported, never silently dropped.
            ("invalid CIDR tokens", machines_values(), ["10.96.0.0/12", "nope"], ["10.200.0.0/33"], ["10.200.0.0/18"],
             ["SCALE_SERVICE_CIDRS entry 'nope' is not a CIDR", "SCALE_BMC_PREFIXES entry '10.200.0.0/33' is not a CIDR"]),
            # Without a Service CIDR nothing can be verified.
            ("unknown ServiceCIDR", machines_values(), [], [], ["10.200.0.0/18"],
             ['cannot determine the cluster ServiceCIDR: set SCALE_SERVICE_CIDRS="<cidr> ..."']),
        ]
        for name, values, service_cidrs, bmc_prefixes, expected_prefixes, expected_errors in cases:
            with self.subTest(name=name):
                prefixes, errors = check_service_cidr(io.StringIO(values), service_cidrs, bmc_prefixes, SITE_CONFIG)
                self.assertEqual([str(prefix) for prefix in prefixes], expected_prefixes)
                self.assertEqual(errors, expected_errors)

    def test_invalid_values(self):
        """Reject values the chart would render differently from what the check models."""
        cases = [
            ("pods list", "pods: []\n", "pods must be a mapping"),
            ("group string", "pods:\n  mat-0:\n    machines:\n      compute: yes\n",
             "pods.mat-0.machines.compute must be a mapping"),
            ("placeholder relay", machines_values("FILL_IN"),
             "pods.mat-0.machines.compute.bmcDhcpRelayAddress 'FILL_IN' is not an IP address"),
            # A group without a relay deploys with the chart default, which the check cannot vouch for.
            ("missing relay", "pods:\n  mat-0:\n    machines:\n      compute:\n        hostCount: 10\n",
             "pods.mat-0.machines.compute.bmcDhcpRelayAddress is not set; the chart substitutes its own default, "
             "which this check cannot vouch for"),
            ("DHCP relay mode", "dhcpRelay:\n  baseIP: 10.96.127.10\n" + machines_values(),
             "dhcpRelay.baseIP is set: DHCP relay mode is not covered by this check"),
            # Controller Mode publishes the inherited default group, so a partial values file cannot be vouched for.
            ("Controller Mode without pods", "image:\n  tag: dev\nmat-k8s-controller:\n  enabled: true\n",
             "mat-k8s-controller.enabled is true but pods is not set, so the chart's default machine group would "
             "deploy unchecked; pass the complete values file of the install"),
        ]
        for name, values, message in cases:
            with self.subTest(name=name):
                with self.assertRaises(ValueError) as raised:
                    check_service_cidr(io.StringIO(values), ["10.96.0.0/12"], [], SITE_CONFIG)
                self.assertEqual(str(raised.exception), message)

    def test_site_config_representations(self):
        """Read the prefixes from TOML and from Core values whatever quoting their author chose."""
        cases = [
            # A rendered site config is TOML, where single quotes also delimit strings.
            ("single-quoted TOML", "[networks.simulated-oob]\ntype = 'underlay'\nprefix = '10.200.0.0/18'\n"),
            # A double-quoted YAML scalar escapes the TOML quotes and line breaks.
            ("escaped YAML scalar",
             'nico-api:\n  siteConfig:\n    nicoApiSiteConfig: "[networks.simulated-oob]\\nprefix = \\"10.200.0.0/18\\"\\n"\n'),
        ]
        for name, site_config in cases:
            with self.subTest(name=name):
                prefixes, errors = check_service_cidr(io.StringIO(machines_values()), ["10.96.0.0/12"], [], site_config)
                self.assertEqual(([str(prefix) for prefix in prefixes], errors), (["10.200.0.0/18"], []))
        # Core values without the embedded site config are reported, not treated as an empty declaration.
        with self.assertRaises(ValueError) as raised:
            check_service_cidr(io.StringIO(machines_values()), ["10.96.0.0/12"], [], "global: {}\n")
        self.assertIn("neither TOML nor Core values", str(raised.exception))

    def test_cli_service_cidr_sources_and_exit_status(self):
        """Resolve the ServiceCIDR from the environment or kubectl and exit nonzero on any finding."""
        kubeadm = "apiServer: {}\nnetworking:\n  serviceSubnet: 10.233.0.0/18,fd85::/108\n"
        dump = '"--service-cluster-ip-range=10.96.0.0/12",\n'
        cases = [
            # SCALE_SERVICE_CIDRS wins and kubectl is never called.
            ("environment", {"SCALE_SERVICE_CIDRS": "10.96.0.0/12"}, 0, "OK: BMC/NVOS networks 10.200.0.0/18 are outside the ServiceCIDR 10.96.0.0/12", []),
            # ServiceCIDR objects are the first cluster source.
            ("ServiceCIDR objects", {"FAKE_SERVICECIDRS": "10.96.0.0/12 fd00::/108"}, 0,
             "OK: BMC/NVOS networks 10.200.0.0/18 are outside the ServiceCIDR 10.96.0.0/12 fd00::/108",
             ["get servicecidrs -o jsonpath={.items[*].spec.cidrs[*]}"]),
            # kubeadm's ClusterConfiguration is the fallback, with comma-separated families split.
            ("kubeadm-config", {"FAKE_KUBEADM": kubeadm}, 0,
             "OK: BMC/NVOS networks 10.200.0.0/18 are outside the ServiceCIDR 10.233.0.0/18 fd85::/108",
             ["get servicecidrs -o jsonpath={.items[*].spec.cidrs[*]}",
              "get cm kubeadm-config -n kube-system -o jsonpath={.data.ClusterConfiguration}"]),
            # The apiserver flag is the last resort.
            ("apiserver flag", {"FAKE_DUMP": dump}, 0,
             "OK: BMC/NVOS networks 10.200.0.0/18 are outside the ServiceCIDR 10.96.0.0/12",
             ["get servicecidrs -o jsonpath={.items[*].spec.cidrs[*]}",
              "get cm kubeadm-config -n kube-system -o jsonpath={.data.ClusterConfiguration}",
              "cluster-info dump"]),
            # An unknown ServiceCIDR is a failure, not a pass.
            ("unknown", {}, 1, 'ERROR: cannot determine the cluster ServiceCIDR: set SCALE_SERVICE_CIDRS="<cidr> ..."', None),
            # An overlap fails the preflight so a shell `&&` chain stops before helm.
            ("overlap", {"SCALE_SERVICE_CIDRS": "10.200.0.0/16"}, 1,
             "ERROR: BMC network 10.200.0.0/18 overlaps the cluster ServiceCIDR 10.200.0.0/16", []),
        ]
        with tempfile.TemporaryDirectory() as directory:
            values = Path(directory) / "mat-values.yaml"
            values.write_text(machines_values(), encoding="utf-8")
            site = Path(directory) / "nico-core-simulation.yaml"
            site.write_text(SITE_CONFIG, encoding="utf-8")
            kubectl = Path(directory) / "kubectl"
            kubectl.write_text(FAKE_KUBECTL, encoding="utf-8")
            kubectl.chmod(kubectl.stat().st_mode | stat.S_IXUSR)
            log = Path(directory) / "kubectl.log"
            # Inherited SCALE_* settings must not decide which source a case exercises.
            base_env = {key: value for key, value in os.environ.items() if not key.startswith("SCALE_")}
            base_env.update(PATH=f"{directory}{os.pathsep}{os.environ['PATH']}", KUBECTL_LOG=str(log))
            for name, extra_env, returncode, line, calls in cases:
                with self.subTest(name=name):
                    log.write_text("", encoding="utf-8")
                    env = {**base_env, **extra_env}
                    result = subprocess.run(
                        [sys.executable, str(CHECKER), str(values), "--site-config", str(site)],
                        env=env, capture_output=True, text=True, check=False,
                    )
                    self.assertEqual(result.returncode, returncode, result.stderr)
                    self.assertEqual(result.stderr, "")
                    self.assertEqual(result.stdout.strip(), line)
                    if calls is not None:
                        self.assertEqual(log.read_text(encoding="utf-8").splitlines(), calls)

    def test_parser_failures_exit_nonzero(self):
        """Keep missing dependencies and malformed input distinguishable from a clean preflight result."""
        with tempfile.TemporaryDirectory() as directory:
            values = Path(directory) / "mat-values.yaml"
            cases = [
                ("invalid YAML", "pods: [\n", [], "Cannot check the machine-a-tron BMC/NVOS networks"),
                ("missing PyYAML", "{}", ["-I", "-S"], "requires PyYAML"),
                ("placeholder relay", machines_values("FILL_IN"), [],
                 "Cannot check the machine-a-tron BMC/NVOS networks: pods.mat-0.machines.compute.bmcDhcpRelayAddress 'FILL_IN'"),
            ]
            env = {**os.environ, "SCALE_SERVICE_CIDRS": "10.96.0.0/12"}
            for name, contents, flags, diagnostic in cases:
                with self.subTest(name=name):
                    values.write_text(textwrap.dedent(contents), encoding="utf-8")
                    result = subprocess.run(
                        [sys.executable, *flags, str(CHECKER), str(values)],
                        env=env, capture_output=True, text=True, check=False,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertIn(diagnostic, result.stderr)
