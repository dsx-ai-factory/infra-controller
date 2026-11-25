#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Verify the actual skip-path values render into an explicitly disabled Core configuration."""

import importlib.util
import pathlib
import subprocess
import tempfile
import unittest

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("skip_dpf", ROOT / "render-skip-dpf-values.py")
RENDERER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RENDERER)


class SkipDpfValuesTests(unittest.TestCase):
    def test_disabled_site_without_base_requires_explicit_configuration(self):
        with self.assertRaisesRegex(ValueError, "custom base config"):
            RENDERER.render('nico-api:\n  siteConfig:\n    enabled: false\n')

    def test_explicit_site_or_base_dpf_configuration_is_preserved_verbatim(self):
        for site, base in [('[dpf]\nenabled = true', ''), ('["dpf"]\nenabled = false', ''),
                           ('[dpf]', ''), ('[dpf.deployments.bf3]\nflavor_name = "custom"', ''),
                           ('', '[dpf]\nenabled = true')]:
            with self.subTest(site=site, base=base):
                source = yaml.safe_dump({"nico-api": {
                    "siteConfig": {"nicoApiSiteConfig": site},
                    "configFiles": {"nicoApiConfig": base},
                }})
                self.assertEqual(RENDERER.render(source), source)

    def test_disabled_site_file_uses_custom_base_without_enabling_site(self):
        source = yaml.safe_dump({"nico-api": {
            "siteConfig": {"enabled": False},
            "configFiles": {"nicoApiConfig": 'asn = 65535\n'},
        }})
        api = yaml.safe_load(RENDERER.render(source))["nico-api"]
        self.assertFalse(api["siteConfig"]["enabled"])
        self.assertEqual(RENDERER.tomllib.loads(api["configFiles"]["nicoApiConfig"]),
                         {"asn": 65535, "dpf": {"enabled": False}})

    def test_stock_skip_path_passes_disabled_config_to_helm(self):
        setup = (ROOT / "setup.sh").read_text()
        block = setup.split('_CORE_INSTALLED_THIS_RUN=false\n', 1)[1]
        block = block.split('    if ! "${INSTALL_VAULT}"; then', 1)[0]
        with tempfile.TemporaryDirectory() as directory:
            rendered = pathlib.Path(directory) / "values.yaml"
            subprocess.run(['bash', '-euc', '''
SKIP_CORE=false
INSTALL_DPF=false
SCRIPT_DIR="$1"
CORE_VALUES=""
NICO_IMAGE_REGISTRY=example.test
NICO_CORE_IMAGE_TAG=test
''' + block + '\nfi\ncp "${_CORE_VALUES_ARG}" "$2"\nrm -f "${_DPF_VALUES}"',
                            '--', str(ROOT), str(rendered)], check=True, capture_output=True, text=True)
            # setup passes umbrella values, while this render uses the API subchart directly.
            api_values = yaml.safe_load(rendered.read_text())["nico-api"]
            rendered.write_text(yaml.safe_dump(api_values))
            output = subprocess.check_output([
                'helm', 'template', 'nico-api', str(ROOT.parent / 'helm/charts/nico-api'),
                '-f', str(rendered),
            ], text=True)
            site = next(doc for doc in yaml.safe_load_all(output)
                        if doc and doc.get('kind') == 'ConfigMap'
                        and 'nico-api-site-config.toml' in doc.get('data', {}))
            config = RENDERER.tomllib.loads(site['data']['nico-api-site-config.toml'])
            self.assertEqual(config.pop('dpf'), {'enabled': False})
            original_values = yaml.safe_load((ROOT / 'values/nico-core.yaml').read_text())
            original = original_values['nico-api']['siteConfig']['nicoApiSiteConfig']
            self.assertEqual(config, RENDERER.tomllib.loads(original))


if __name__ == '__main__':
    unittest.main()
