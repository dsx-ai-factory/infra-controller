# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise IPv6 target selection with rendered Services and Prometheus relabeling."""

import copy
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
CHART = ROOT / "helm/charts/nico-machine-a-tron"
METRICS_LABEL = "app.kubernetes.io/metrics"
SERVICE_LABEL = "__meta_kubernetes_service_label_app_kubernetes_io_metrics"
SERVICE_NAME = "__meta_kubernetes_service_name"
PORT_NAME = "__meta_kubernetes_endpointslice_port_name"
ADDRESS_TYPE = "__meta_kubernetes_endpointslice_address_type"


def render_probe(*settings):
    command = ["helm", "template", "probe-test", str(CHART)]
    for setting in settings:
        command.extend(["--set", f"nico-site-health-probe.{setting}"])
    output = subprocess.run(command, check=True, capture_output=True, text=True).stdout
    return [document for document in yaml.safe_load_all(output) if document]


class IPv6ScrapingTest(unittest.TestCase):
    def test_probe_target_selection(self):
        with (ROOT / "helm-prereqs/observability/values-nico-ipv6-scraping.yaml").open() as stream:
            spec = yaml.safe_load(stream)["prometheus"]["prometheusSpec"]
        job = copy.deepcopy(next(config for config in spec["additionalScrapeConfigs"]
                                 if config["job_name"] == "nico-ipv6"))
        probe_labels = []
        monitors = []
        for name, settings in [
            ("nico-site-health-probe", []),
            ("custom-probe", ["nameOverride=custom-probe"]),
        ]:
            documents = render_probe("enabled=true", "serviceMonitor.enabled=true", *settings)
            service = next(document for document in documents
                           if document["kind"] == "Service" and document["metadata"]["name"] == f"{name}-metrics")
            monitors.append(next(document for document in documents
                                 if document["kind"] == "ServiceMonitor" and document["metadata"]["name"] == name))
            probe_labels.append({
                SERVICE_LABEL: service["metadata"]["labels"][METRICS_LABEL],
                SERVICE_NAME: service["metadata"]["name"],
                PORT_NAME: service["spec"]["ports"][0]["name"],
                ADDRESS_TYPE: "IPv6",
            })

        default, renamed = probe_labels
        api = {**default, SERVICE_LABEL: "nico-api", SERVICE_NAME: "nico-api-metrics", PORT_NAME: "http"}
        cases = [
            ("probe", default, "[2001:db8::1]:9009", True),
            ("ipv4 sibling", {**default, ADDRESS_TYPE: "IPv4"}, "192.0.2.1:9009", False),
            ("renamed probe", renamed, "[2001:db8::2]:9009", True),
            ("Service without metrics label", {**default, SERVICE_LABEL: ""}, "[2001:db8::3]:9009", False),
            ("Unbound DNS port", {**default, SERVICE_LABEL: "nico-unbound",
                                  SERVICE_NAME: "nico-unbound", PORT_NAME: "dns-tcp"}, "[2001:db8::5]:53", False),
            ("API metrics", api, "[2001:db8::4]:9009", True),
            ("API profiler", {**api, SERVICE_NAME: "nico-api-profiler"}, "[2001:db8::4]:9009", False),
            ("API per-object metrics", {**api, SERVICE_LABEL: "nico-api-object-metrics",
                                        SERVICE_NAME: "nico-api-object-metrics"}, "[2001:db8::4]:9009", False),
        ]
        # Substitute discovery inputs only; promtool applies the production relabel rules.
        del job["kubernetes_sd_configs"]
        job["static_configs"] = [
            {"targets": [address], "labels": {**labels, "__meta_test_case": name}}
            for name, labels, address, _ in cases
        ]
        with tempfile.TemporaryDirectory() as directory:
            config = Path(directory) / "prometheus.yaml"
            config.write_text(yaml.safe_dump({"scrape_configs": [job]}))
            result = subprocess.run(
                [os.environ.get("PROMTOOL", "promtool"), "check", "service-discovery",
                 "--timeout=1s", str(config), "nico-ipv6"],
                check=True, capture_output=True, text=True,
            )
        targets = {target["discoveredLabels"]["__meta_test_case"]: target["labels"]
                   for target in json.loads(result.stdout)}
        self.assertEqual(set(targets), {name for name, _, _, _ in cases})
        for name, _, _, keep in cases:
            with self.subTest(target=name):
                self.assertEqual(bool(targets[name]), keep)
        # The IPv6 replacement job must not run alongside the ordinary monitor.
        exclusion = next(expression for expression in spec["serviceMonitorSelector"]["matchExpressions"]
                         if expression["key"] == METRICS_LABEL)
        self.assertEqual(exclusion["operator"], "NotIn")
        for monitor in monitors:
            self.assertIn(monitor["metadata"]["labels"][METRICS_LABEL], exclusion["values"])


if __name__ == "__main__":
    unittest.main()
