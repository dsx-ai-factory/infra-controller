# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("core-admin.sh")


class CoreAdminTest(unittest.TestCase):
    def test_no_ready_pod_reports_namespace(self):
        with tempfile.TemporaryDirectory() as directory:
            kubectl = Path(directory) / "kubectl"
            kubectl.write_text("""#!/bin/sh
if [ "$1" = get ]; then
  printf '%s\\n' '{"items": []}'
else
  printf 'unexpected kubectl call\\n' >&2
  exit 99
fi
""")
            kubectl.chmod(0o755)
            result = subprocess.run(
                ["bash", str(SCRIPT), "machine", "list"],
                env={**os.environ, "PATH": f"{directory}:{os.environ['PATH']}",
                     "LOCAL_DEV_NAMESPACE": "fixture"},
                capture_output=True, text=True, timeout=10,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no Ready nico-api pod in namespace fixture", result.stderr)
            self.assertNotIn("unexpected kubectl call", result.stderr)


if __name__ == "__main__":
    unittest.main()
