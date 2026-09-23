# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Protect Core VIP preflight independently of site YAML presentation and live services."""

import io
import json
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest


CHECKER = Path(__file__).resolve().parents[1] / "check-external-service-vips.py"
check_vips = runpy.run_path(str(CHECKER))["check_vips"]


class ExternalServiceVipsTest(unittest.TestCase):
    """Validate configured VIPs and require a DHCPv6 relay VIP without blocking automatic allocation."""

    def test_yaml_formatting_and_service_enablement(self):
        """Match chart enablement and Service types so preflight checks only required VIPs."""
        cases = [
            # A deeper block must not hide an enabled Service from preflight.
            ("four-space indentation", """\
nico-api:
    externalService:
        enabled: true
        annotations:
            metallb.universe.tf/loadBalancerIPs: ""
""", ["nico-api.externalService"]),
            # YAML Boolean spelling must have the same meaning here as in Helm.
            ("capitalized Boolean", """\
nico-api:
  externalService:
    enabled: True
    annotations:
      metallb.universe.tf/loadBalancerIPs: ""
""", ["nico-api.externalService"]),
            # Neither a disabled chart, a disabled Service, nor a missing v6 gate creates a VIP.
            ("disabled resources", """\
nico-api:
  enabled: False
  externalService: {enabled: true, annotations: {metallb.io/loadBalancerIPs: ""}}
nico-dhcp:
  externalService: {enabled: false, annotations: {metallb.io/loadBalancerIPs: ""}}
  v6ExternalService: {enabled: true}
""", []),
            # NodePort must ignore even an explicit blank MetalLB annotation.
            ("NodePort without VIP", """\
nico-api:
  externalService:
    enabled: true
    type: NodePort
    annotations: {metallb.io/loadBalancerIPs: ""}
""", []),
            # The API template falls back to LoadBalancer even for an explicitly empty type.
            ("empty type fallback", "nico-api: {externalService: {enabled: true, type: '', annotations: {metallb.io/loadBalancerIPs: ''}}}\n",
             ["nico-api.externalService"]),
            # DHCPv6 still needs its own VIP; its fixed Service type ignores this override.
            ("independent DHCPv6 VIP", """\
nico-dhcp:
  dhcp: {v6Enabled: True}
  externalService:
    enabled: true
    annotations: {metallb.universe.tf/loadBalancerIPs: "192.0.2.67"}
  v6ExternalService:
    enabled: true
    type: NodePort
    annotations: {metallb.universe.tf/loadBalancerIPs: " "}
""", ["nico-dhcp.v6ExternalService"]),
            # Existing per-pod Services may mix explicit and automatically allocated VIPs.
            ("per-pod automatic allocation", """\
nico-dns:
  externalService:
    enabled: true
    perPodAnnotations:
      - metallb.universe.tf/loadBalancerIPs: "192.0.2.53"
      - {}
""", []),
            # Omission preserves automatic allocation for existing external Services.
            ("missing annotation", "nico-api: {externalService: {enabled: true}}\n", []),
            # DHCPv6 keeps its separate explicit relay VIP requirement.
            ("missing DHCPv6 annotation", "nico-dhcp: {dhcp: {v6Enabled: true}, v6ExternalService: {enabled: true}}\n",
             ["nico-dhcp.v6ExternalService"]),
            # Null optional maps and null/empty annotation lists retain their omission behavior.
            ("empty optional values", """\
nico-api: {externalService: null, v6ExternalService: null}
nico-dhcp: {dhcp: null, v6ExternalService: {enabled: true}}
nico-dns: {externalService: {enabled: true, perPodAnnotations: null}}
nico-ntp: {externalService: {enabled: true, perPodAnnotations: []}}
""", []),
            # Unused per-pod values must not mask an empty VIP on a single-Service chart.
            ("unused per-pod annotations", """\
nico-api:
  externalService:
    enabled: true
    annotations: {metallb.universe.tf/loadBalancerIPs: ""}
    perPodAnnotations:
      - metallb.universe.tf/loadBalancerIPs: "192.0.2.1"
""", ["nico-api.externalService"]),
            # The current MetalLB annotation spelling remains valid alongside legacy site files.
            ("MetalLB annotation alias", """\
nico-api:
  externalService:
    enabled: true
    annotations: {metallb.io/loadBalancerIPs: "192.0.2.1"}
""", []),
        ]
        # Parse the supplied YAML text so these cases exercise actual loader semantics.
        for name, values, expected in cases:
            with self.subTest(name=name):
                errors, warnings, pool_errors = check_vips(io.StringIO(values))
                self.assertEqual(errors, [f"{service} needs loadBalancerIPs from your MetalLB pool"
                                          for service in expected])
                self.assertEqual(warnings, [])
                self.assertEqual(pool_errors, [])

    def test_falsy_invalid_value_types(self):
        """Reject wrong types before defaults can hide malformed Service configuration."""
        cases = [
            # An empty list must not become an omitted Service mapping.
            ("Service list", "nico-api: {externalService: []}\n",
             "nico-api.externalService must be a mapping"),
            # An empty string must not silently disable the DHCPv6 gate.
            ("DHCP string", "nico-dhcp: {dhcp: '', v6ExternalService: {enabled: true}}\n",
             "nico-dhcp.dhcp must be a mapping"),
            # An empty mapping must not become an omitted per-pod annotation list.
            ("annotation mapping", "nico-dns: {externalService: {enabled: true, perPodAnnotations: {}}}\n",
             "nico-dns.externalService.perPodAnnotations must be a list"),
        ]
        # Parse real YAML and require the specific field diagnostic at each boundary.
        for name, values, expected in cases:
            with self.subTest(name=name):
                with self.assertRaises(ValueError) as raised:
                    check_vips(io.StringIO(values))
                self.assertEqual(str(raised.exception), expected)

    def test_ip_addresses_and_pool_membership(self):
        """Reject unusable relay VIPs while accepting either family in CIDR and range pools."""
        cases = [
            # IPv6-only CIDR pools are valid without a companion IPv4 pool.
            ("IPv6 CIDR", "2001:db8::67", ["2001:db8::/64"], None),
            # The inclusive upper endpoint exercises range membership instead of CIDR masking.
            ("IPv6 range", "2001:db8::ff", ["2001:db8::10-2001:db8::ff"], None),
            # A syntactically valid IPv4 VIP still cannot serve the IPv6-only relay Service.
            ("wrong family", "192.0.2.67", ["192.0.2.0/24"], "must be an IPv6 address"),
            # Nonempty text must not bypass validation merely because it contains no IPv4 digits.
            ("malformed IPv6", "2001:db8::gg", ["2001:db8::/64"], "not a valid IP address"),
            # A well-formed address outside the configured IPv6 pool cannot be allocated.
            ("outside pool", "2001:db8:1::67", ["2001:db8::/64"], "not within any MetalLB IPAddressPool"),
            # Existing IPv4 pools do not provide addresses to the new IPv6 relay Service.
            ("no IPv6 pool", "2001:db8::67", ["192.0.2.0/24"], "not within any MetalLB IPAddressPool"),
            # Unavailable rendered pools retain format-only checks instead of inventing a pool error.
            ("unavailable pools", "2001:db8::67", None, None),
        ]
        # Supply parsed pool resources exactly as the preflight render feeds the checker.
        for name, vip, blocks, expected_error in cases:
            with self.subTest(name=name):
                values = {"nico-dhcp": {
                    "dhcp": {"v6Enabled": True},
                    "v6ExternalService": {"enabled": True, "annotations": {
                        "metallb.io/loadBalancerIPs": vip,
                    }},
                }}
                pools = io.StringIO(json.dumps({"kind": "IPAddressPool", "spec": {"addresses": blocks}})
                                    if blocks is not None else "")
                errors, warnings, pool_errors = check_vips(io.StringIO(json.dumps(values)), pools)
                self.assertEqual(warnings, [])
                self.assertEqual(pool_errors, [])
                if expected_error is None:
                    self.assertEqual(errors, [])
                else:
                    self.assertEqual(len(errors), 1)
                    self.assertIn(expected_error, errors[0])

    def test_ipv4_only_external_services(self):
        """Reject in-pool IPv6 VIPs when the chart explicitly fixes the Service family to IPv4."""
        cases = [
            # DHCPv4 has its own IPv4 Service, separate from the DHCPv6 workload.
            ("nico-dhcp", {}),
            # Both Unbound external Services remain IPv4 even when internal IPv6 is enabled.
            ("unbound", {"ipv6": {"enabled": True}}),
        ]
        for component, config in cases:
            with self.subTest(component=component):
                # Pool membership must not override the Service's declared address family.
                values = {component: {**config, "externalService": {
                    "enabled": True,
                    "annotations": {"metallb.io/loadBalancerIPs": "2001:db8::67"},
                }}}
                pools = {"kind": "IPAddressPool", "spec": {"addresses": ["2001:db8::/64"]}}
                errors, warnings, pool_errors = check_vips(io.StringIO(json.dumps(values)), io.StringIO(json.dumps(pools)))
                self.assertEqual(errors, [f"{component}.externalService: VIP 2001:db8::67 must be an IPv4 address"])
                self.assertEqual(warnings, [])
                self.assertEqual(pool_errors, [])

    def test_invalid_pool_entries(self):
        """Report empty or invalid pools instead of silently skipping their containment checks."""
        cases = [
            # An explicit empty pool is a configuration error, unlike unavailable rendered input.
            ("empty pool", [], "has no addresses"),
            # IPv6 prefix lengths must be checked with their own address-family bounds.
            ("invalid CIDR", ["2001:db8::/129"], "is invalid"),
            # Both range endpoints must parse, including the endpoint the old shell ignored.
            ("invalid range endpoint", ["192.0.2.1-192.0.2.999"], "is invalid"),
            # Reversed ranges parse as addresses but cannot contain any VIP.
            ("reversed range", ["2001:db8::ff-2001:db8::10"], "ascending order"),
        ]
        # No Service is needed to prove that the pool configuration itself is unusable.
        for name, blocks, expected_error in cases:
            with self.subTest(name=name):
                pools = {"kind": "IPAddressPool", "spec": {"addresses": blocks}}
                errors, warnings, pool_errors = check_vips(io.StringIO("{}"), io.StringIO(json.dumps(pools)))
                self.assertEqual(errors, [])
                self.assertEqual(len(pool_errors), 1)
                self.assertIn(expected_error, pool_errors[0])
                self.assertEqual(warnings, [])

    def test_cli_pool_input_and_diagnostic_severity(self):
        """Keep stdin pool validation and normalized duplicate warnings wired to preflight's CLI protocol.

        Preserve diagnostic source and severity so preflight points operators at the right input file.
        """
        with tempfile.TemporaryDirectory() as directory:
            # Annotation aliases describe one Service; the second Service genuinely reuses its IPv6 VIP.
            values = Path(directory) / "core-values.yaml"
            values.write_text("""\
nico-api:
  externalService:
    enabled: true
    annotations:
      metallb.io/loadBalancerIPs: "192.0.2.10, 2001:0db8:0:0:0:0:0:67"
      metallb.universe.tf/loadBalancerIPs: "192.0.2.10, 2001:db8::67"
nico-dhcp:
  dhcp: {v6Enabled: true}
  v6ExternalService:
    enabled: true
    annotations: {metallb.io/loadBalancerIPs: "2001:db8::67"}
""", encoding="utf-8")
            pools = """\
kind: IPAddressPool
spec:
  addresses: ["192.0.2.10-192.0.2.20", "2001:db8:1::/64"]
---
kind: BGPPeer
spec:
  peerAddress: "2001:db8::/64"
---
kind: IPAddressPool
spec:
  addresses: []
---
kind: IPAddressPool
spec:
  addresses: ["invalid"]
"""
            # Only IPAddressPool addresses count; errors and warnings must retain distinct prefixes.
            # Pool errors also need their own prefix to identify the right configuration file.
            result = subprocess.run(
                [sys.executable, str(CHECKER), str(values), "--metallb-stdin"],
                input=pools, capture_output=True, text=True, check=False,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(result.stderr, "")
            self.assertIn("ERROR[pool]: MetalLB IPAddressPool has no addresses", result.stdout)
            self.assertIn("ERROR[pool]: MetalLB pool entry 'invalid' is invalid", result.stdout)
            self.assertIn("ERROR: nico-dhcp.v6ExternalService: VIP 2001:db8::67 is not within any MetalLB IPAddressPool", result.stdout)
            self.assertNotIn("VIP 192.0.2.10", result.stdout)
            self.assertEqual(result.stdout.count("WARNING:"), 1)
            self.assertIn("WARNING: VIP 2001:db8::67 is assigned to more than one service", result.stdout)

    def test_parser_failures_exit_nonzero(self):
        """Keep missing dependencies and malformed YAML distinguishable from a clean preflight result."""
        with tempfile.TemporaryDirectory() as directory:
            values = Path(directory) / "core-values.yaml"
            cases = [
                # Invalid YAML must fail instead of returning an empty missing-VIP list.
                ("invalid YAML", "nico-api: [\n", [], None, "Cannot check external Service VIPs"),
                # Disabling site packages models Python installations without PyYAML.
                ("missing PyYAML", "{}", ["-I", "-S"], None, "requires PyYAML"),
                # A malformed render must not be mistaken for unavailable pool configuration.
                ("invalid MetalLB YAML", "{}", [], "kind: [\n", "Cannot check external Service VIPs"),
            ]
            # Exercise the helper's process status, which preflight uses to record parser errors.
            for name, contents, flags, pools, diagnostic in cases:
                with self.subTest(name=name):
                    values.write_text(contents, encoding="utf-8")
                    result = subprocess.run(
                        [sys.executable, *flags, str(CHECKER), str(values),
                         *(["--metallb-stdin"] if pools is not None else [])],
                        input=pools, capture_output=True, text=True, check=False,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertIn(diagnostic, result.stderr)
