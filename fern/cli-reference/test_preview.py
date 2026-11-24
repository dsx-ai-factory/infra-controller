# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import unittest
from fnmatch import fnmatchcase
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

import yaml

from preview import OUTPUT, ROOT, check_coverage, render


class SidebarTests(unittest.TestCase):
    def test_four_ordered_domain_folders_with_discoverable_command_pages(self):
        domains = ["Hardware", "Network", "Tenant", "Admin"]
        with TemporaryDirectory() as tmp:
            site = Path(tmp) / "site"
            docs = site / "docs"
            docs.mkdir(parents=True)
            navigation = docs / "index.yml"
            navigation.write_text(
                "navigation:\n- tab: documentation\n  layout:\n  - section: Reference\n"
                "    contents:\n    - section: Development\n      contents: []\n"
            )
            raw = Path(tmp) / "raw"
            guide = docs / "guide.md"
            guide.write_text(
                "[Inspect VPCs][show-vpcs]\n\n"
                "[show-vpcs]: https://github.com/dsx-ai-factory/infra-controller/blob/main/"
                "docs/manuals/nico-admin-cli/commands/network/vpc-show.md#options\n"
            )
            commands = raw / "commands/network"
            commands.mkdir(parents=True)
            (raw / "README.md").write_text("# CLI reference\n")
            for domain in domains:
                (raw / f"{domain.lower()}.md").write_text(f"# {domain}\n")
            (commands / "vpc.md").write_text(
                "# `nico-admin-cli vpc`\n\n## Subcommands\n\n"
                "| Command | Description |\n| --- | --- |\n"
                "| [`show`](./vpc-show.md) | Show VPCs |\n"
            )
            (commands / "vpc-show.md").write_text("# `nico-admin-cli vpc show`\n")

            render(site, raw)

            reference = yaml.safe_load(navigation.read_text())["navigation"][0]["layout"][0]
            cli = reference["contents"][0]
            self.assertEqual(cli["section"], "CLI reference")
            self.assertTrue((docs / cli["path"]).is_file())
            folders = cli["contents"]
            self.assertEqual([folder["title"] for folder in folders], domains)
            for folder in folders:
                self.assertTrue((docs / folder["folder"] / "index.md").is_file())
                self.assertEqual(folder["title-source"], "frontmatter")
            vpc = docs / folders[1]["folder"] / "vpc"
            self.assertIn("[show-vpcs]: manuals/cli-reference/network/vpc/show.md#options", guide.read_text())
            for path, title in [(vpc / "index.md", "vpc"), (vpc / "show.md", "show")]:
                metadata = yaml.safe_load(path.read_text().split("---", 2)[1])
                self.assertEqual(metadata["title"], title)
                self.assertNotIn("position", metadata)


class WorkflowTests(unittest.TestCase):
    def test_pr_and_publish_triggers_match_the_required_core_gate(self):
        config = ROOT / ".github"
        inputs = yaml.safe_load((config / "ci/fern-docs-paths.yml").read_text())["fern_docs"]
        for change in ["crates/admin-cli/src/vpc/mod.rs", "crates/rpc/proto/forge.proto", "Cargo.lock"]:
            self.assertTrue(any(fnmatchcase(change, pattern) for pattern in inputs), change)
        for file, event in [
            ("fern-docs-preview-build.yml", "pull_request"),
            ("publish-fern-docs.yml", "push"),
        ]:
            with self.subTest(workflow=file):
                workflow = yaml.load((config / "workflows" / file).read_text(), Loader=yaml.BaseLoader)
                self.assertCountEqual(workflow["on"][event]["paths"], inputs)

    def test_missing_nested_command_fails_coverage(self):
        help_pages = {
            (): "Commands:\n  vpc   Manage VPCs\n  help  Print help\n",
            ("vpc",): "Commands:\n  show  List VPCs\n",
            ("vpc", "show"): "Usage: nico-admin-cli vpc show\n",
        }
        with TemporaryDirectory() as tmp:
            site = Path(tmp)
            page = site / OUTPUT / "network/vpc/index.md"
            page.parent.mkdir(parents=True)
            page.write_text('---\nheadline: "nico-admin-cli vpc"\n---\n')
            with patch("preview.subprocess.check_output", side_effect=lambda args, text: help_pages[tuple(args[1:-1])]):
                with self.assertRaisesRegex(ValueError, "missing .*nico-admin-cli vpc show"):
                    check_coverage("nico-admin-cli", site)


if __name__ == "__main__":
    unittest.main()
