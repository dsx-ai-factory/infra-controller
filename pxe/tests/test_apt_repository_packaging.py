# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check Scout APT publication using real Debian tools and fixture packages.

Run on Linux with dpkg-dev installed; no image build or network is required.
"""

import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import tomllib
import unittest
from urllib.parse import urlparse


REPO_ROOT = Path(__file__).resolve().parents[2]


class AptRepositoryPackagingTest(unittest.TestCase):
    def run_command(self, command, **kwargs):
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=30, **kwargs,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def build_package(self, root, package, arch):
        """Create a small real deb without compiling an architecture-specific binary."""
        package_root = root / package
        control = package_root / "DEBIAN"
        control.mkdir(parents=True)
        control.chmod(0o755)
        (control / "control").write_text(
            f"Package: {package}\nVersion: 1.0.0\nArchitecture: {arch}\n"
            "Maintainer: Fixture <fixture@example.com>\n"
            "Description: APT publication fixture\n"
        )
        deb = root / "target/debs" / f"{package}_1.0.0_{arch}.deb"
        deb.parent.mkdir(parents=True, exist_ok=True)
        self.run_command(["dpkg-deb", "--build", str(package_root), str(deb)])

    def test_repositories_survive_carrier_publication(self):
        """Both Scout sources must resolve to the matching published index and debs."""
        with (REPO_ROOT / "pxe/Makefile.toml").open("rb") as source:
            tasks = tomllib.load(source)["tasks"]
        values = (REPO_ROOT / "helm/examples/values-full.yaml").read_text()
        cases = (
            ("x86_64", "amd64", "x86-64", {"forge-scout"}),
            ("aarch64", "arm64", "aarch64", {"forge-scout", "forge-dpu"}),
        )

        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            published = directory / "published"
            published.mkdir()
            for build_arch, package_arch, carrier, packages in cases:
                with self.subTest(build_arch=build_arch, stage="build and publish"):
                    root = directory / build_arch
                    for package in sorted(packages):
                        self.build_package(root, package, package_arch)
                    env = {**os.environ, "REPO_ROOT": str(root), "PKG_VERSION": "1.0.0"}
                    for task in ("mkdir-static", f"setup-apt-repo-{package_arch}"):
                        self.run_command(["bash", "-eu", "-c", tasks[task]["script"]], env=env)

                    # Use the documented copy commands, rebasing container paths
                    # into the fixture. Publish ARM second, as on an affected site.
                    block = values.split(f"- name: boot-artifacts-{carrier}\n", 1)[1]
                    command = json.loads(block.split("command: ", 1)[1].splitlines()[0])
                    copy = shlex.split(command[2])
                    self.assertEqual(copy[:2], ["cp", "-r"])
                    self.assertEqual(copy[-1], "/forge-boot-artifacts/blobs/internal")
                    blobs = root / "pxe/static/blobs/internal"
                    self.run_command([
                        *copy[:2],
                        *(str(blobs / path.lstrip("/")) for path in copy[2:-1]),
                        str(published),
                    ])

            for build_arch, package_arch, _, packages in cases:
                with self.subTest(build_arch=build_arch, stage="consume"):
                    source = (
                        REPO_ROOT / f"pxe/mkosi.profiles/scout-oss-{build_arch}/mkosi.extra"
                        / "etc/apt/sources.list.d/forge.list"
                    )
                    _, _, url, suite, component = shlex.split(source.read_text())
                    prefix = "/public/blobs/internal/"
                    path = urlparse(url).path
                    self.assertTrue(path.startswith(prefix), path)
                    repository = published / path.removeprefix(prefix)
                    distribution = repository / "dists" / suite
                    release = (distribution / "Release").read_text()
                    self.assertIn(f"Architectures: {package_arch}\n", release)

                    index_path = f"{component}/binary-{package_arch}/Packages"
                    index = (distribution / index_path).read_bytes()
                    self.assertIn(
                        f"{hashlib.sha256(index).hexdigest()} {len(index)} {index_path}",
                        release,
                    )
                    entries = [
                        dict(line.split(": ", 1) for line in stanza.splitlines())
                        for stanza in index.decode().strip().split("\n\n")
                    ]
                    self.assertEqual({entry["Package"] for entry in entries}, packages)
                    for entry in entries:
                        self.assertEqual(entry["Architecture"], package_arch)
                        package = (repository / entry["Filename"]).read_bytes()
                        self.assertEqual(len(package), int(entry["Size"]))
                        self.assertEqual(hashlib.sha256(package).hexdigest(), entry["SHA256"])


if __name__ == "__main__":
    unittest.main()
