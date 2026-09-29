# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Exercise Scout interface selection without changing the host network."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT = REPO_ROOT / "pxe/common_files/forge-scout-network.sh"


class ScoutNetworkTest(unittest.TestCase):
    def test_preferred_interface_waits_for_usable_address(self):
        cases = [
            {"name": "tentative IPv6 becomes ready", "state": "tentative", "ready": True, "probes": 2},
            {"name": "failed IPv6 never becomes ready", "state": "dadfailed", "ready": False, "probes": 2},
            {"name": "IPv4 is already ready", "state": "ipv4", "ready": True, "probes": 1},
        ]
        for case in cases:
            state = case["state"]
            ready = case["ready"]
            probes = case["probes"]
            with self.subTest(name=case["name"]), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                cmdline = directory / "cmdline"
                cmdline.write_text("mac=02:00:00:00:00:01\n")
                network = directory / "net"
                for interface, mac in (("enp1s0", "01"), ("enp2s0", "02")):
                    link = network / interface
                    link.mkdir(parents=True)
                    (link / "address").write_text(f"02:00:00:00:00:{mac}\n")
                    (link / "carrier").write_text("1\n")

                # The stub models iproute2's flag filters and records when
                # networkd is reloaded relative to the address becoming usable.
                stub = directory / "command"
                stub.write_text(f"#!{sys.executable}\n" + textwrap.dedent('''\
                    import json
                    import os
                    from pathlib import Path
                    import sys

                    root = Path(os.environ["TEST_DIRECTORY"])
                    args = sys.argv[1:]
                    command = Path(sys.argv[0]).name
                    with (root / "calls").open("a") as calls:
                        calls.write(command + " " + " ".join(args) + "\\n")
                    if command == "ip":
                        if args[:4] == ["-o", "address", "show", "dev"]:
                            if args[4] == "enp1s0":
                                counter = root / "probes"
                                count = int(counter.read_text()) + 1 if counter.exists() else 1
                                counter.write_text(str(count))
                                state = os.environ["TEST_ADDRESS_STATE"]
                                flag = "tentative" if state == "tentative" and count == 1 else ""
                                if state == "dadfailed":
                                    flag = "dadfailed"
                                if not flag or "-" + flag not in args:
                                    address = "inet 192.0.2.10/24" if state == "ipv4" else "inet6 2001:db8::10/64"
                                    print("2: enp1s0 " + address + " scope global " + flag)
                            else:
                                assert args == ["-o", "address", "show", "dev", "enp2s0", "scope", "global"], args
                        else:
                            assert args in [
                                ["-o", "-4", "route", "show", "dev", "enp2s0", "scope", "global"],
                                ["-o", "-6", "route", "show", "dev", "enp2s0", "scope", "global"],
                            ], args
                    elif args == ["reload"]:
                        (root / "reload_probe").write_text((root / "probes").read_text())
                    else:
                        assert args == ["--json=short", "status", "enp2s0"], args
                        filename = root / "dhcp.network"
                        if (root / "reload_probe").exists():
                            filename = root / "runtime/00-forge-scout-nonpreferred.network"
                        print(json.dumps({"NetworkFile": str(filename), "AdministrativeState": "configured"}, separators=(",", ":")))
                '''))
                stub.chmod(0o700)
                for command in ("ip", "networkctl"):
                    (directory / command).symlink_to(stub)

                result = subprocess.run(
                    ["sh", str(SCRIPT)],
                    env={
                        **os.environ,
                        "SCOUT_CMDLINE_FILE": str(cmdline),
                        "SCOUT_SYS_CLASS_NET": str(network),
                        "SCOUT_IP_COMMAND": str(directory / "ip"),
                        "SCOUT_NETWORKCTL_COMMAND": str(directory / "networkctl"),
                        "SCOUT_NETWORKD_RUNTIME_DIR": str(directory / "runtime"),
                        "SCOUT_NETWORKD_DHCP_FILE": str(directory / "dhcp.network"),
                        "SCOUT_NETWORK_WAIT_SECONDS": "2",
                        "SCOUT_NETWORK_POLL_INTERVAL": "1",
                        "TEST_DIRECTORY": str(directory),
                        "TEST_ADDRESS_STATE": state,
                    },
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(result.returncode, 0 if ready else 1, result.stdout + result.stderr)
                self.assertEqual(int((directory / "probes").read_text()), probes)
                configuration = directory / "runtime/00-forge-scout-nonpreferred.network"
                if ready:
                    self.assertEqual(int((directory / "reload_probe").read_text()), probes)
                    configuration_text = configuration.read_text()
                    self.assertIn("Property=!INTERFACE=enp1s0\n", configuration_text)
                    self.assertIn("DHCP=no\nIPv6AcceptRA=no\n", configuration_text)
                else:
                    self.assertFalse(configuration.exists())
                    self.assertNotIn("networkctl", (directory / "calls").read_text())
                    self.assertIn("reason=no_global_address", result.stderr)


if __name__ == "__main__":
    unittest.main()
