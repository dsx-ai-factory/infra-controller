# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("verify-core-services.sh")


class VerifyCoreServicesTest(unittest.TestCase):
    def test_workload_readiness(self):
        # Exercise the command, including Kubernetes failures and pods without probes.
        for case in ("healthy", "terminating", "rollout-failed", "empty", "api-error", "not-ready",
                     "restarting", "pxe-http-failed", "pxe-wrong-content"):
            with self.subTest(case=case), tempfile.TemporaryDirectory() as directory:
                result = subprocess.run(
                    ["bash", "-c", '''
kubectl() {
    printf '%s\\n' "$*" >> "$TEST_DIRECTORY/calls"
    case "$*" in
        '-n isolated get deployments,statefulsets,daemonsets -o name')
            [[ "$TEST_CASE" != api-error ]] || return 1
            [[ "$TEST_CASE" != empty ]] || return 0
            printf '%s\\n' deployment.apps/nico-api statefulset.apps/nico-dns deployment.apps/nico-pxe
            ;;
        '-n isolated rollout status '* )
            [[ "$TEST_CASE" != rollout-failed || "$*" != *nico-pxe* ]]
            ;;
        '-n isolated get pods -o json')
            [[ ! -f "$TEST_DIRECTORY/observed" ]] || export TEST_OBSERVED=1
            touch "$TEST_DIRECTORY/observed"
            "$TEST_PYTHON" -c '
import json, os
restarts = int(os.environ["TEST_CASE"] == "restarting" and "TEST_OBSERVED" in os.environ)
ready = "False" if os.environ["TEST_CASE"] == "not-ready" else "True"
items = [
    {"metadata": {"name": "nico-dns-0", "uid": "dns-uid"}, "status": {
        "phase": "Running", "conditions": [{"type": "Ready", "status": ready}],
        "containerStatuses": [{"restartCount": restarts}]}},
    {"metadata": {"name": "nico-api-migrate"}, "status": {"phase": "Succeeded"}}
]
if os.environ["TEST_CASE"] == "terminating" and "TEST_OBSERVED" not in os.environ:
    items.append({"metadata": {"uid": "old", "deletionTimestamp": "2026-01-01T00:00:00Z"},
                  "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": "False"}]}})
print(json.dumps({"items": items}))'
            ;;
        '-n isolated get service nico-pxe -o json')
            printf '%s\\n' '{"spec":{"ports":[{"name":"http","port":18080}]}}'
            ;;
        '-n isolated exec deployment/nico-api -c nico-api -- curl --fail --silent --show-error --connect-timeout 10 --max-time 30 http://nico-pxe.isolated.svc.cluster.local.:18080/public/scout-firmware-scripts/nvidia/dgxh100/cx7/metadata.toml')
            [[ "$TEST_CASE" != pxe-http-failed ]] || return 22
            if [[ "$TEST_CASE" == pxe-wrong-content ]]; then
                printf '%s\\n' 'wrong artifact'
            else
                cat "$TEST_ARTIFACT"
            fi
            ;;
        *) return 99 ;;
    esac
}
sleep() { :; }
export -f kubectl sleep
bash "$1" isolated
''', "bash", str(SCRIPT)],
                    env={**os.environ, "TEST_CASE": case, "TEST_DIRECTORY": directory,
                         "TEST_PYTHON": os.sys.executable,
                         "TEST_ARTIFACT": str(SCRIPT.parents[3] /
                                              "pxe/scout-firmware-scripts/nvidia/dgxh100/cx7/metadata.toml")},
                    capture_output=True, text=True, timeout=10,
                )
                if case in ("healthy", "terminating"):
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    calls = (Path(directory) / "calls").read_text()
                    for resource in ("deployment.apps/nico-api", "statefulset.apps/nico-dns",
                                     "deployment.apps/nico-pxe"):
                        self.assertIn(f"rollout status {resource} --timeout=300s", calls)
                    self.assertIn("http://nico-pxe.isolated.svc.cluster.local.:18080/public/", calls)
                else:
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertNotIn("No such file", result.stderr)


if __name__ == "__main__":
    unittest.main()
