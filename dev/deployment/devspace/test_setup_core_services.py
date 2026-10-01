# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("setup-core-services.sh")


class SetupCoreServicesTest(unittest.TestCase):
    def test_dhcp_service_addresses(self):
        for case in ("placeholders", "configured", "ipv6-only", "api-error"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                placeholders = {
                    "nameservers": "REPLACE_WITH_NICO_DNS_VIP",
                    "ntpserver": "REPLACE_WITH_NICO_NTP_VIPS",
                    "provisioning-server-ipv4": "REPLACE_WITH_NICO_PXE_VIP",
                }
                parameters = {f"{prefix}-{key}": value
                              for prefix in ("nico", "carbide")
                              for key, value in placeholders.items()}
                # Explicit site overrides must not be replaced by local defaults.
                if case == "configured":
                    parameters = {key: "192.0.2.9" for key in parameters}
                config = {"Dhcp4": {"hooks-libraries": [{
                    "library": "/usr/lib/kea/hooks/libdhcp.so", "parameters": parameters
                }], "valid-lifetime": 3600}}
                (Path(directory) / "config.json").write_text(json.dumps({
                    "data": {"kea_config.json": json.dumps(config)}
                }))
                result = subprocess.run(
                    ["bash", "-c", '''
kubectl() {
    case "$*" in
        '-n isolated get service '* )
            [[ "$TEST_CASE" != api-error ]] || return 1
            if [[ "$TEST_CASE" == ipv6-only ]]; then
                printf '%s' '{"spec":{"clusterIPs":["fd00::1"]}}'
            else
                case "$5" in
                    nico-dns) ip=10.96.1.1 ;;
                    nico-ntp-client) ip=10.96.1.2 ;;
                    nico-pxe) ip=10.96.1.3 ;;
                    *) return 99 ;;
                esac
                printf '{"spec":{"clusterIPs":["fd00::1","%s"]}}' "$ip"
            fi
            ;;
        '-n isolated get configmap nico-dhcp-config -o json') cat "$TEST_DIRECTORY/config.json" ;;
        '-n isolated patch configmap nico-dhcp-config --type=merge -p '*)
            printf '%s' "${@: -1}" > "$TEST_DIRECTORY/patch.json" ;;
        '-n isolated rollout restart deployment/nico-dhcp') touch "$TEST_DIRECTORY/restarted" ;;
        *) return 99 ;;
    esac
}
export -f kubectl
bash "$1" isolated
''', "bash", str(SCRIPT)],
                    env={**os.environ, "TEST_CASE": case, "TEST_DIRECTORY": directory},
                    capture_output=True, text=True, timeout=10,
                )
                if case in ("ipv6-only", "api-error"):
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertFalse((Path(directory) / "restarted").exists())
                    continue
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                if case == "configured":
                    self.assertFalse((Path(directory) / "patch.json").exists())
                    self.assertFalse((Path(directory) / "restarted").exists())
                else:
                    patch = json.loads((Path(directory) / "patch.json").read_text())
                    updated = json.loads(patch["data"]["kea_config.json"])
                    expected = {f"{prefix}-{key}": ip
                                for prefix in ("nico", "carbide")
                                for key, ip in zip(placeholders, ("10.96.1.1", "10.96.1.2", "10.96.1.3"))}
                    config["Dhcp4"]["hooks-libraries"][0]["parameters"] = expected
                    self.assertEqual(updated, config)
                    self.assertTrue((Path(directory) / "restarted").exists())


if __name__ == "__main__":
    unittest.main()
