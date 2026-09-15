# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Protect Core VIP preflight independently of site YAML presentation and live services."""

import io
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest


CHECKER = Path(__file__).resolve().parents[1] / "check-external-service-vips.py"
missing_vips = runpy.run_path(str(CHECKER))["missing_vips"]


class ExternalServiceVipsTest(unittest.TestCase):
    """Require VIPs for active LoadBalancer Services while preserving other configurations."""

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
  externalService: {enabled: true}
nico-dhcp:
  externalService: {enabled: false}
  v6ExternalService: {enabled: true}
""", []),
            # Configurable NodePort Services need no MetalLB annotation.
            ("NodePort without VIP", """\
nico-api:
  externalService:
    enabled: true
    type: NodePort
""", []),
            # The API template falls back to LoadBalancer even for an explicitly empty type.
            ("empty type fallback", "nico-api: {externalService: {enabled: true, type: ''}}\n",
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
            # One configured per-pod VIP must not mask a missing entry for another pod.
            ("per-pod VIP", """\
nico-dns:
  externalService:
    enabled: true
    perPodAnnotations:
      - metallb.universe.tf/loadBalancerIPs: "192.0.2.53"
      - {}
""", ["nico-dns.externalService"]),
            # An absent annotation is just as incomplete as an explicitly blank value.
            ("missing annotation", "nico-api: {externalService: {enabled: true}}\n",
             ["nico-api.externalService"]),
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
                self.assertEqual(missing_vips(io.StringIO(values)), expected)

    def test_parser_failures_exit_nonzero(self):
        """Keep missing dependencies and malformed YAML distinguishable from a clean preflight result."""
        with tempfile.TemporaryDirectory() as directory:
            values = Path(directory) / "core-values.yaml"
            values.write_text("nico-api: [\n", encoding="utf-8")
            cases = [
                # Invalid YAML must fail instead of returning an empty missing-VIP list.
                ("invalid YAML", [], "Cannot check external Service VIPs"),
                # Disabling site packages models Python installations without PyYAML.
                ("missing PyYAML", ["-I", "-S"], "requires PyYAML"),
            ]
            # Exercise the helper's process status, which preflight uses to record parser errors.
            for name, flags, diagnostic in cases:
                with self.subTest(name=name):
                    result = subprocess.run(
                        [sys.executable, *flags, str(CHECKER), str(values)],
                        capture_output=True, text=True, check=False,
                    )
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, "")
                    self.assertIn(diagnostic, result.stderr)
