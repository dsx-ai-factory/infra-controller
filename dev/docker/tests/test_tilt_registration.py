# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Verify registration precedes the Helm workload without competing writers."""

from pathlib import Path
import unittest

from tilt_test_support import TiltResult, baseline

ROOT = Path(__file__).resolve().parents[3]


class RegistrationWiringTest(unittest.TestCase):
    def test_registration_runs_before_agent_and_does_not_render_placeholder(self):
        values = baseline()
        values['development']['localConfig']['restNamespace'] = 'custom-rest'
        result = TiltResult({'rest': True}, values=values, context='kind-selected')
        registration = result.resources['rest-integration']
        workload = result.resources['nico-rest-site-agent']
        self.assertEqual(registration['command'], [str(ROOT / 'rest-api/scripts/setup-local.sh'), 'register-site'])
        self.assertEqual(registration["env"]["NAMESPACE"], "custom-rest")
        self.assertEqual(registration["env"]["LOCAL_DEV_KUBE_CONTEXT"], "kind-selected")
        self.assertNotIn("nico-rest-site-agent", registration["resource_deps"])
        self.assertIn("rest-integration", workload["resource_deps"])
        self.assertIn("siteIdentityFromRegistrationSecret=true", workload["flags"])
        self.assertIn("--wait", workload["flags"])
        self.assertNotIn("site-registration", [obj["metadata"]["name"] for obj in result.objects])


if __name__ == "__main__":
    unittest.main()
