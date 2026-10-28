#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Execute guest cleanup and DHCP hooks with temporary paths and stubbed commands."""

import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import unittest

import lab_manager


GUEST = Path(__file__).resolve().parents[1] / "lab/guest"


class GatewayBootstrapTests(unittest.TestCase):
    def test_builder_always_attempts_shutdown_without_publishing_failed_builds(self):
        cases = ("console", "mkdir", "modprobe", "mount", "apk",
                 "unwritable-marker", "sync", "success")
        for failure in cases:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                binary = root / "bin"
                binary.mkdir()
                (root / "etc/apk").mkdir(parents=True)
                build = root / "build"
                console = root / ("missing/console" if failure == "console" else "console")
                script = (GUEST / "fetch-gateway-packages.sh").read_text()
                script = script.replace("/mnt/build", str(build)).replace("/dev/hvc0", str(console))
                script = script.replace("/etc/", str(root / "etc") + "/")
                if failure == "unwritable-marker":
                    (build / "failed").mkdir(parents=True)
                for name in ("mkdir", "modprobe", "mount", "ip", "dhcpcd", "cp",
                             "apk", "cat", "touch", "sync", "poweroff"):
                    content = '#!/bin/sh\nprintf "%s\\n" "${0##*/}" >> "$BOOT_TEST/events"\n'
                    content += f'[ "$BOOT_FAIL" != {name} ] || exit 41\n'
                    if name in ("mkdir", "touch"):
                        content += f'exec {shlex.quote(shutil.which(name))} "$@"\n'
                    elif name == "cat":
                        content += 'printf "fixture-package\\n"\n'
                    path = binary / name
                    path.write_text(content)
                    path.chmod(0o755)
                result = subprocess.run(["sh", "-c", script], capture_output=True, text=True,
                                        env=dict(os.environ, PATH=str(binary), BOOT_TEST=str(root),
                                                 BOOT_FAIL="apk" if failure == "unwritable-marker" else failure),
                                        executable=shutil.which("sh"), timeout=5)
                events = (root / "events").read_text().splitlines() if (root / "events").exists() else []
                self.assertEqual(events[-2:], ["sync", "poweroff"], result.stderr)
                succeeded = failure in ("success", "sync")
                self.assertEqual(result.returncode == 0, succeeded, result.stderr)
                self.assertEqual((build / "success").exists(), succeeded)
                if failure in ("modprobe", "mount", "apk"):
                    self.assertEqual((build / "failed").read_text(), "41\n")

    def test_dhcp_hook_marks_only_configured_wan_ipv4_acquisitions(self):
        source = (GUEST / "networking-gateway.sh").read_text()
        hook = source.split("<<'HOOK'\n", 1)[1].split("\nHOOK\n", 1)[0]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            lab = lab_manager.Lab(root / "state", 12)
            self.addCleanup(shutil.rmtree, lab.runtime, ignore_errors=True)
            lab.directory.mkdir(parents=True)
            wan = root / "wan_if"
            wan.write_text("eth1\n")
            console = root / "console"
            hook = hook.replace("/etc/vxlab/wan_if", str(wan)).replace("/dev/hvc0", str(console))
            cases = (("BOUND", "eth1", "192.0.2.2", True),
                     ("RENEW", "eth1", "192.0.2.3", True),
                     ("BOUND", "eth0", "10.12.0.1", False),
                     ("RELEASE", "eth1", "192.0.2.2", False),
                     ("BOUND", "eth1", "", False))
            for reason, interface, address, emitted in cases:
                with self.subTest(reason=reason, interface=interface, address=address):
                    console.unlink(missing_ok=True)
                    subprocess.run(["sh", "-eu", "-c", hook], check=True, capture_output=True,
                                   env=dict(os.environ, reason=reason, interface=interface,
                                            new_ip_address=address), timeout=5)
                    self.assertEqual(console.exists(), emitted)
                    if emitted:
                        self.assertEqual(console.read_text(), f"VFLAB_WAN_IPV4: leased {address} for DHCP\n")
                    serial = "OpenRC started\ngateway login: " + (console.read_text() if emitted else "")
                    (lab.directory / "serial.log").write_text(serial)
                    self.assertEqual(lab.gateway_ip(), address if emitted else None)


if __name__ == "__main__":
    unittest.main()
