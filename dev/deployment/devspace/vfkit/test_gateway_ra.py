#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise WAN router renewal without changing the host network."""

import importlib.util
import os
import shlex
from pathlib import Path
import subprocess
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).parent / "lab/guest/renew-ipv6-router.sh"


class RouterRenewalTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.environment = {**os.environ, "PATH": f"{self.directory}:{os.environ['PATH']}",
                            "RA_TEST_DIR": str(self.directory)}
        self.executable("ip", '''
printf '%s\\n' "$*" >> "$RA_TEST_DIR/queries"
[ ! -f "$RA_TEST_DIR/query-fails" ] || exit 1
cat "$RA_TEST_DIR/routes"
''')
        self.executable("timeout", '''
printf '%s\\n' "$*" >> "$RA_TEST_DIR/probes"
shift 3
exec "$@"
''')
        self.executable("rdisc6", 'exit 0\n')

    def executable(self, name, body):
        path = self.directory / name
        path.write_text("#!/bin/sh\n" + body)
        path.chmod(0o755)

    def run_check(self, *options):
        return subprocess.run(["sh", str(SCRIPT), "wan0", *options],
                              env=self.environment, capture_output=True, text=True, timeout=10)

    def test_probe_only_when_wan_has_no_usable_long_lived_default(self):
        cases = (
            ("fresh", "default via fe80::1 dev wan0 proto ra expires 121sec", False),
            ("renewal boundary", "default via fe80::1 dev wan0 proto ra expires 120sec", True),
            ("missing", "", True),
            ("permanent", "default via fe80::1 dev wan0 metric 100", False),
            ("another usable router", "default via fe80::1 dev wan0 expires 3sec\n"
             "default via fe80::2 dev wan0 expires 900sec", False),
            ("link down", "default via fe80::1 dev wan0 expires 900sec linkdown", True),
            ("reject route", "unreachable default dev lo metric 1024", True),
        )
        for name, routes, probe in cases:
            with self.subTest(name=name):
                (self.directory / "routes").write_text(routes + "\n")
                (self.directory / "probes").unlink(missing_ok=True)
                result = self.run_check("--once")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual((self.directory / "probes").exists(), probe)
                if probe:
                    self.assertEqual((self.directory / "probes").read_text(),
                                     "-k 1 5 rdisc6 -n -m -r 1 -w 3000 wan0\n")
        self.assertEqual(set((self.directory / "queries").read_text().splitlines()),
                         {"-6 -o route show default dev wan0"})

    def test_query_error_is_not_mistaken_for_an_absent_route(self):
        (self.directory / "query-fails").touch()
        result = self.run_check("--once")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Cannot inspect", result.stderr)
        self.assertFalse((self.directory / "probes").exists())

    def test_daemon_retries_failed_probe_and_becomes_quiet_after_renewal(self):
        (self.directory / "routes").write_text("")
        self.executable("rdisc6", '''
if [ ! -f "$RA_TEST_DIR/failed-once" ]; then
    touch "$RA_TEST_DIR/failed-once"
    exit 1
fi
printf 'default via fe80::1 dev wan0 proto ra expires 1800sec\\n' > "$RA_TEST_DIR/routes"
''')
        self.executable("sleep", '''
printf '%s\\n' "$*" >> "$RA_TEST_DIR/sleeps"
if [ "$(wc -l < "$RA_TEST_DIR/sleeps")" -ge 3 ]; then kill -TERM "$PPID"; fi
''')
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Router discovery failed", result.stderr)
        self.assertEqual(len((self.directory / "probes").read_text().splitlines()), 2)
        self.assertEqual((self.directory / "sleeps").read_text(), "30\n30\n30\n")

    def test_gateway_overlay_includes_executable_service_and_offline_dependency(self):
        source = SCRIPT.parent.parent
        spec = importlib.util.spec_from_file_location("gateway_image", source / "image/gateway-image.py")
        builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(builder)
        assets = self.directory / "assets"
        assets.mkdir()
        (assets / "complete").touch()
        (assets / "world").write_bytes((source / "image/gateway-world").read_bytes())
        (assets / builder.KEY_NAME).write_text("test repository public key\n")
        lab = self.directory / "labs/12"
        lab.mkdir(parents=True)
        (lab / "id_ed25519.pub").write_text("test SSH public key\n")
        with patch.object(builder, "VAR", self.directory), patch.object(builder, "ASSETS", assets), \
                patch.object(builder, "make_iso"), patch.dict(os.environ, {
                    "VXLAB_WAN_MAC": "02:00:00:00:12:01", "VXLAB_LAN_MAC": "02:00:00:00:12:02",
                    "VXLAB_LAN_MODE": "dual", "VXLAB_UPLINK": "bridged"}):
            builder.prepare(12)
        with tarfile.open(lab / "gateway.apkovl.tar.gz") as archive:
            for target, name in (("etc/init.d/vflab-ipv6-router", "ipv6-router-service"),
                                 ("usr/local/lib/vflab/renew-ipv6-router.sh", "renew-ipv6-router.sh")):
                self.assertEqual(archive.getmember(target).mode, 0o755)
                self.assertEqual(archive.extractfile(target).read(), (SCRIPT.parent / name).read_bytes())
            self.assertIn(b"ndisc6\n", archive.extractfile("etc/apk/world").read())

    def test_shutdown_reaps_active_probe_and_poll_sleep(self):
        for phase, routes in (("rdisc6", ""), ("sleep", "default via fe80::1 dev wan0")):
            with self.subTest(phase=phase):
                (self.directory / "routes").write_text(routes)
                ready = self.directory / "child"
                ready.unlink(missing_ok=True)
                self.executable(phase, f'exec {shlex.quote(os.sys.executable)} -c '
                                + shlex.quote('import os, pathlib, time; '
                                              'pathlib.Path(os.environ["RA_TEST_DIR"], "child").write_text(str(os.getpid())); '
                                              'time.sleep(30)') + '\n')
                process = subprocess.Popen(["sh", str(SCRIPT), "wan0"], env=self.environment,
                                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                try:
                    deadline = time.monotonic() + 5
                    while not ready.exists() and time.monotonic() < deadline:
                        time.sleep(0.01)
                    self.assertTrue(ready.exists(), "daemon did not enter the expected phase")
                    child = int(ready.read_text())
                    process.terminate()
                    self.assertEqual(process.wait(timeout=5), 0)
                    with self.assertRaises(ProcessLookupError):
                        os.kill(child, 0)
                finally:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
