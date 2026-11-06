# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Exercise the loader's storage setup without mounting on the test host."""

import os
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "common_files/scout-loader-rclocal"


class ScoutLoaderStorageTest(unittest.TestCase):
    def run_loader(self, failure="", attempts=1):
        source = SCRIPT.read_text()
        function = re.search(r"^prepare_squashfs_rootfs\(\) \{\n.*?^\}", source, re.M | re.S)
        self.assertIsNotNone(function)
        image_settings = []
        for name in ("ROOTFS_IMAGE_DIR", "ROOTFS_IMAGE_FILE"):
            assignment = re.search(rf"^{name}=.*$", source, re.M)
            self.assertIsNotNone(assignment)
            image_settings.append(assignment[0])
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # Retain production path bindings, redirecting them into the fixture.
            # Run the production function, replacing privileged commands only.
            # curl models a root image too large for the surrounding /run mount.
            harness = "\n".join(image_settings) + r'''
ROOTFS_IMAGE_DIR="$TEST_DIRECTORY$ROOTFS_IMAGE_DIR"
ROOTFS_IMAGE_FILE="$TEST_DIRECTORY$ROOTFS_IMAGE_FILE"
printf '%s\n' "$ROOTFS_IMAGE_DIR" "$ROOTFS_IMAGE_FILE" > "$TEST_DIRECTORY/paths"
ROOTFS_DIGEST_FILE="$TEST_DIRECTORY/digest"
CURL_ERR="$TEST_DIRECTORY/curl.err"
MOUNT_ERR="$TEST_DIRECTORY/mount.err"
SQUASHFS_LOWER="$TEST_DIRECTORY/lower"
OVERLAY_ROOT="$TEST_DIRECTORY/overlay"
OVERLAY_UPPER="$OVERLAY_ROOT/upper"
OVERLAY_WORK="$OVERLAY_ROOT/work"
PCR_HASH_CMD=""
record() { printf '%s\n' "$*" >> "$TEST_DIRECTORY/calls"; }
cleanup_squashfs_mounts() { record cleanup; }
ensure_loop_devices() { :; }
modprobe() { :; }
mkdir() {
    record "mkdir $*"
    [[ "$TEST_FAILURE" != mkdir ]]
}
mountpoint() {
    [[ "$2" == "$TEST_DIRECTORY/run" ]] ||
        { [[ "$2" == "$ROOTFS_IMAGE_DIR" ]] && [[ -f "$TEST_DIRECTORY/mounted" ]]; }
}
mount() {
    record "mount $*"
    if [[ "${!#}" == "$ROOTFS_IMAGE_DIR" ]]; then
        [[ "$TEST_FAILURE" != mount ]] || { printf 'mount denied\n' >&2; return 7; }
        touch "$TEST_DIRECTORY/mounted"
    fi
}
curl() {
    record "curl $*"
    [[ -f "$TEST_DIRECTORY/mounted" && "${3%/*}" == "$ROOTFS_IMAGE_DIR" ]] || return 23
    [[ "$TEST_FAILURE" != download ]]
}
'''
            result = subprocess.run(
                ["bash", "-c", harness + function[0] + "\n" +
                 "for ((i=0; i<TEST_ATTEMPTS; i++)); do prepare_squashfs_rootfs https://example.test/scout.squashfs || exit $?; done"],
                env={**os.environ, "TEST_DIRECTORY": directory, "TEST_FAILURE": failure,
                     "TEST_ATTEMPTS": str(attempts)},
                capture_output=True, text=True, timeout=10,
            )
            calls = (root / "calls").read_text().splitlines()
            mount_error = (root / "mount.err").read_text() if (root / "mount.err").exists() else ""
            image_dir, image_file = (root / "paths").read_text().splitlines()
            return result, calls, mount_error, (image_dir, image_file)

    def test_download_uses_independent_storage_and_retry_reuses_mount(self):
        result, calls, _, (image_dir, image_file) = self.run_loader(attempts=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        storage_mounts = [c for c in calls if c.startswith("mount ") and c.endswith(image_dir)]
        downloads = [c for c in calls if c.startswith("curl ")]
        self.assertEqual(len(storage_mounts), 1, calls)
        self.assertEqual(len(downloads), 2, calls)
        self.assertLess(calls.index(storage_mounts[0]), calls.index(downloads[0]))
        self.assertIn("-t tmpfs", storage_mounts[0])
        mount_args = shlex.split(storage_mounts[0])
        options = set(mount_args[mount_args.index("-o") + 1].split(","))
        self.assertTrue({"mode=0700", "nodev", "nosuid", "noexec"} <= options)
        size_options = [option for option in options if option.startswith("size=")]
        self.assertIn(size_options, ([], ["size=50%"]))
        self.assertEqual(Path(image_file).parent, Path(image_dir))
        self.assertTrue(all(shlex.split(c)[3] == image_file for c in downloads))

    def test_storage_setup_failure_prevents_download(self):
        for failure in ("mkdir", "mount"):
            with self.subTest(failure=failure):
                result, calls, error, _ = self.run_loader(failure=failure)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(any(c.startswith("curl ") for c in calls), calls)
                if failure == "mount":
                    self.assertEqual(error, "mount denied\n")

    def test_failed_download_does_not_mount_an_incomplete_image(self):
        result, calls, _, _ = self.run_loader(failure="download")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len([c for c in calls if c.startswith("curl ")]), 1)
        self.assertFalse(any("-t squashfs" in c or "-t overlay" in c for c in calls), calls)


if __name__ == "__main__":
    unittest.main()
