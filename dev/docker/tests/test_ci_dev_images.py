# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check configuration for main-only development DHCP cache publishing."""

from pathlib import Path
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[3]


class CiDevImagesTest(unittest.TestCase):
    def test_cache_archives_share_identity_and_only_save_from_main(self):
        jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yaml").read_text())["jobs"]
        steps = jobs["test-dev-dhcp-image"]["steps"]
        by_id = {step["id"]: step for step in steps if "id" in step}
        restore = by_id["sccache"]
        lookup = by_id["sccache-saved"]
        export = by_id["sccache-export"]
        save = next(step for step in steps if step.get("uses") == "actions/cache/save@v4")
        # Actions includes the path in the cache identity, not just the key.
        for step in (lookup, save):
            self.assertEqual(step["with"]["path"], restore["with"]["path"])
            self.assertEqual(step["with"]["key"], "${{ steps.sccache.outputs.cache-primary-key }}")
        self.assertEqual(by_id["dev-build"]["env"]["SCCACHE_SEED_DIR"], export["env"]["CACHE_EXPORT_DIR"])
        self.assertIn(restore["with"]["path"].replace("~", "${HOME}"), by_id["sccache-dir"]["run"])
        # Saving is gated through lookup and export, which PR runs skip.
        self.assertEqual(lookup["if"],
                         "github.ref == 'refs/heads/main' && steps.sccache.outputs.cache-hit != 'true'")
        self.assertEqual(export["if"],
                         "steps.sccache-saved.outcome == 'success' && steps.sccache-saved.outputs.cache-hit != 'true'")
        self.assertEqual(save["if"], "steps.sccache-export.outcome == 'success'")


if __name__ == "__main__":
    unittest.main()
