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
PREFERRED = "00-forge-scout-preferred.network"
NONPREFERRED = "01-forge-scout-nonpreferred.network"
LEGACY = "00-forge-scout-nonpreferred.network"
EARLY_HOOK = "etc/systemd/system/systemd-network-generator.service.d/50-forge-scout-network.conf"


class ScoutNetworkTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.cmdline = self.directory / "cmdline"
        self.cmdline.write_text("mac=02:00:00:00:00:01\n")
        self.network = self.directory / "net"
        self.network.mkdir()
        self.runtime = self.directory / "runtime"
        self.config = self.directory / "config"
        self.config.mkdir()
        self.dhcp = self.config / "dhcp.network"
        self.dhcp.write_text("[Match]\nName=enx* enp* enP*\n[Network]\nDHCP=yes\n")
        self.env = {
            **os.environ,
            "SCOUT_CMDLINE_FILE": str(self.cmdline),
            "SCOUT_SYS_CLASS_NET": str(self.network),
            "SCOUT_IP_COMMAND": str(self.directory / "ip"),
            "SCOUT_NETWORKCTL_COMMAND": str(self.directory / "networkctl"),
            "SCOUT_SYSTEMCTL_COMMAND": str(self.directory / "systemctl"),
            "SCOUT_WAIT_ONLINE_COMMAND": str(self.directory / "wait-online"),
            "SCOUT_NETWORKD_RUNTIME_DIR": str(self.runtime),
            "SCOUT_NETWORKD_CONFIG_DIR": str(self.config),
            "SCOUT_NETWORKD_DHCP_FILE": str(self.dhcp),
            "SCOUT_NETWORK_WAIT_SECONDS": "2",
            "SCOUT_NETWORK_POLL_INTERVAL": "1",
            "TEST_DIRECTORY": str(self.directory),
            "TEST_ADDRESS_STATE": "ipv4",
        }
        # Record network changes and model iproute2's address flag filters.
        stub = self.directory / "command"
        stub.write_text(f"#!{sys.executable}\n" + textwrap.dedent('''\
            import json
            import os
            from pathlib import Path
            import sys

            root = Path(os.environ["TEST_DIRECTORY"])
            def record_stub_error(kind, value, traceback):
                (root / "stub_error").write_text(str(value))
                sys.__excepthook__(kind, value, traceback)
            sys.excepthook = record_stub_error
            args = sys.argv[1:]
            command = Path(sys.argv[0]).name
            with (root / "calls").open("a") as calls:
                calls.write(command + " " + " ".join(args) + "\\n")
            if command == "ip":
                if args[:4] == ["-o", "address", "show", "dev"]:
                    if args[4] == "enp1s0":
                        assert args == ["-o", "address", "show", "dev", "enp1s0", "scope", "global", "-tentative", "-dadfailed"], args
                        counter = root / "probes"
                        count = int(counter.read_text()) + 1 if counter.exists() else 1
                        counter.write_text(str(count))
                        state = os.environ["TEST_ADDRESS_STATE"]
                        if (state == "inspection_failed"
                                or (state == "inspection_failed_after_wait" and (root / "native_wait").exists())):
                            print("injected ip inspection failure", file=sys.stderr)
                            sys.exit(7)
                        flag = "tentative" if state == "tentative" and count == 1 else ""
                        if state == "dadfailed":
                            flag = "dadfailed"
                        phase_file = root / "selected_status"
                        phase = int(phase_file.read_text()) if phase_file.exists() else 0
                        if ((state == "lost_on_reload" and phase == 3)
                                or (state == "lost_on_reload_forever" and count >= 2)
                                or (state == "lost_after_wait" and (root / "native_wait").exists())):
                            sys.exit(0)
                        if not flag or "-" + flag not in args:
                            address = "inet 192.0.2.10/24" if state == "ipv4" else "inet6 2001:db8::10/64"
                            print("2: enp1s0 " + address + " scope global " + flag)
                    else:
                        assert args == ["-o", "address", "show", "dev", "enp2s0", "scope", "global"], args
                        counter = root / "withdrawal_probes"
                        count = int(counter.read_text()) + 1 if counter.exists() else 1
                        counter.write_text(str(count))
                        if os.environ.get("TEST_COMPETING_STATE") == "converges" and count == 1:
                            print("3: enp2s0 inet 198.51.100.20/24 scope global")
                else:
                    assert args in [
                        ["-o", "-4", "route", "show", "dev", "enp2s0", "scope", "global"],
                        ["-o", "-6", "route", "show", "dev", "enp2s0", "scope", "global"],
                    ], args
                    state = os.environ.get("TEST_COMPETING_STATE")
                    count = int((root / "withdrawal_probes").read_text())
                    if state == "converges" and args[1] == "-4" and count == 2:
                        print("default via 198.51.100.1 dev enp2s0")
                    if args[1] == "-6":
                        print("fe80::/64 dev enp2s0")
                        if state == "persists" or (state == "converges" and count == 3):
                            print("default via fe80::1 dev enp2s0")
            elif command == "networkctl":
                if args == ["reload"] or args[:1] == ["reconfigure"]:
                    assert args == ["reload"] or args in [["reconfigure", "enp1s0"], ["reconfigure", "enp2s0"]], args
                    failure = root / ("fail_" + args[0])
                    if failure.exists():
                        failure.unlink()
                        sys.exit(7)
                    if args == ["reload"]:
                        probes = root / "probes"
                        (root / "reload_probe").write_text(probes.read_text() if probes.exists() else "0")
                else:
                    assert len(args) == 3 and args[:2] == ["--json=short", "status"], args
                    assert (root / "net" / args[2] / "address").is_file(), args
                    mac = (root / "net" / args[2] / "address").read_text().strip().lower()
                    preferred = root / "runtime/00-forge-scout-preferred.network"
                    filename = root / "runtime/01-forge-scout-nonpreferred.network"
                    if preferred.exists() and "MACAddress=" + mac + "\\n" in preferred.read_text():
                        filename = preferred
                    if not filename.exists():
                        filename = root / "config/dhcp.network"
                    state = "configured"
                    if args[2] == "enp1s0" and os.environ["TEST_ADDRESS_STATE"] == "lost_on_reload":
                        counter = root / "selected_status"
                        phase = int(counter.read_text()) + 1 if counter.exists() else 1
                        counter.write_text(str(phase))
                        if phase == 1:
                            filename = root / "config/dhcp.network"
                        elif phase == 2:
                            state = "configuring"
                    elif args[2] == "enp1s0" and os.environ["TEST_ADDRESS_STATE"] == "lost_on_reload_forever" and (root / "reload_probe").exists():
                        state = "configuring"
                    print(json.dumps({"NetworkFile": str(filename), "AdministrativeState": state}, separators=(",", ":")))
            elif command == "systemctl":
                assert args == ["reload", "systemd-networkd.service"], args
                assert not any((root / "runtime" / name).exists() for name in ("00-forge-scout-preferred.network", "01-forge-scout-nonpreferred.network"))
            else:
                assert command == "wait-online", command
                (root / "native_wait").touch()
                flags = [] if (root / "runtime/00-forge-scout-preferred.network").exists() else ["--any", "--operational-state=routable"]
                assert args == flags + ["--timeout=" + os.environ["SCOUT_NETWORK_WAIT_SECONDS"]], args
                if (root / "fail_wait").exists():
                    sys.exit(7)
        '''))
        stub.chmod(0o700)
        for command in ("ip", "networkctl", "systemctl", "wait-online"):
            (self.directory / command).symlink_to(stub)

    def add_interfaces(self):
        (self.network / "bonding_masters").touch()
        for interface, mac in (("enp1s0", "01"), ("enp2s0", "02")):
            link = self.network / interface
            link.mkdir()
            (link / "address").write_text(f"02:00:00:00:00:{mac}\n")
            (link / "carrier").write_text("1\n")

    def run_helper(self, *args, expected=0):
        result = subprocess.run(
            ["sh", str(SCRIPT), *map(str, args)], env=self.env,
            capture_output=True, text=True, timeout=10,
        )
        stub_error = self.directory / "stub_error"
        self.assertFalse(stub_error.exists(), stub_error.read_text() if stub_error.exists() else "")
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def calls(self):
        path = self.directory / "calls"
        return path.read_text().splitlines() if path.exists() else []

    def test_prepare_selects_mac_before_interfaces_exist(self):
        self.cmdline.write_text("mac=02:AB:00:00:00:01\n")
        self.runtime.mkdir()
        for directory in (self.runtime, self.config):
            (directory / LEGACY).write_text("old Scout policy\n")
            (directory / "operator.network").write_text("operator configuration\n")
        self.run_helper("prepare")
        self.assertEqual(list(self.network.iterdir()), [])
        self.assertEqual(self.calls(), [])
        preferred = (self.runtime / PREFERRED).read_text()
        self.assertEqual(preferred, "[Match]\nMACAddress=02:ab:00:00:00:01\n\n[Network]\nDHCP=yes\n\n[DHCP]\nClientIdentifier=mac\n")
        nonpreferred = (self.runtime / NONPREFERRED).read_text()
        self.assertEqual(nonpreferred, "[Match]\nName=enx* enp* enP*\n\n[Link]\nRequiredForOnline=no\n\n[Network]\nDHCP=no\nIPv6AcceptRA=no\nConfigureWithoutCarrier=yes\n")
        for directory in (self.runtime, self.config):
            self.assertFalse((directory / LEGACY).exists())
            self.assertEqual((directory / "operator.network").read_text(), "operator configuration\n")
        self.assertIn("DHCP=yes\n", self.dhcp.read_text())

    def test_invalid_selection_removes_owned_policy_without_network_commands(self):
        cases = [
            ("missing MAC", "quiet", "mac_parameter_count"),
            ("malformed MAC", "mac=not-a-mac", "invalid_mac_parameter"),
            ("ambiguous parameters", "mac=02:00:00:00:00:01 mac=02:00:00:00:00:02", "mac_parameter_count"),
        ]
        for name, cmdline, reason in cases:
            with self.subTest(name=name):
                self.cmdline.write_text("mac=02:00:00:00:00:01\n")
                self.run_helper("prepare")
                self.cmdline.write_text(cmdline + "\n")
                result = self.run_helper("prepare")
                self.assertIn("reason=" + reason, result.stdout)
                self.assertFalse((self.runtime / PREFERRED).exists())
                self.assertFalse((self.runtime / NONPREFERRED).exists())
                self.assertEqual(self.calls(), [])

    def test_rejected_invocations_preserve_existing_policy(self):
        self.run_helper("prepare")
        before = {name: (self.runtime / name).read_bytes() for name in (PREFERRED, NONPREFERRED)}
        cases = [("prepare", "extra", "invalid_arguments"), ("wait-online", "extra", "invalid_arguments"),
                 ("prepare", None, "invalid_timing_value")]
        for mode, extra, reason in cases:
            with self.subTest(mode=mode, reason=reason):
                self.env["SCOUT_NETWORK_WAIT_SECONDS"] = "2" if extra else "0"
                result = self.run_helper(mode, *([extra] if extra else []), expected=1)
                self.assertIn("reason=" + reason, result.stdout)
                self.assertEqual({name: (self.runtime / name).read_bytes() for name in before}, before)
                self.assertEqual(self.calls(), [])

    def test_preferred_interface_waits_for_usable_address(self):
        self.add_interfaces()
        cases = [
            {"name": "tentative IPv6 becomes ready", "state": "tentative", "probes": 2},
            {"name": "failed IPv6 opens other interfaces", "state": "dadfailed", "reason": "no_global_address"},
            {"name": "inspection error retains policy", "state": "inspection_failed", "reason": "address_inspection_failed"},
            {"name": "missing interface opens other interfaces", "state": "ipv4", "reason": "no_interface"},
            {"name": "carrier down opens other interfaces", "state": "ipv4", "reason": "no_carrier"},
            {"name": "IPv4 is already ready", "state": "ipv4", "probes": 1},
        ]
        for case in cases:
            with self.subTest(name=case["name"]):
                for name in ("calls", "probes", "reload_probe"):
                    (self.directory / name).unlink(missing_ok=True)
                reason = case.get("reason")
                self.env["TEST_ADDRESS_STATE"] = case["state"]
                (self.network / "enp1s0/address").write_text("02:00:00:00:00:" + ("03" if reason == "no_interface" else "01"))
                (self.network / "enp1s0/carrier").write_text("0" if reason == "no_carrier" else "1")
                self.run_helper("prepare")
                result = self.run_helper(expected=1 if reason else 0)
                retained = not reason or reason == "address_inspection_failed"
                self.assertEqual((self.runtime / PREFERRED).exists(), retained)
                self.assertEqual((self.runtime / NONPREFERRED).exists(), retained)
                if reason == "address_inspection_failed":
                    self.assertIn("reason=address_inspection_failed", result.stdout)
                    self.assertFalse(any(call.startswith("networkctl ") for call in self.calls()))
                    continue
                self.assertIn("networkctl reload", self.calls())
                if not reason:
                    self.assertEqual(int((self.directory / "probes").read_text()), case["probes"] + 1)
                    self.assertEqual(int((self.directory / "reload_probe").read_text()), case["probes"])
                else:
                    self.assertIn("reason=" + reason, result.stderr)
                    self.assertIn("networkctl reconfigure enp2s0", self.calls())
                    if case["state"] == "dadfailed":
                        # A later readiness check restores selection after DHCP recovers.
                        self.env["TEST_ADDRESS_STATE"] = "ipv4"
                        self.run_helper()
                        self.assertTrue((self.runtime / PREFERRED).exists())
                        self.assertTrue((self.runtime / NONPREFERRED).exists())

    def test_failed_networkd_reload_retains_selection_and_is_retried(self):
        self.add_interfaces()
        self.run_helper("prepare")
        (self.directory / "fail_reload").touch()
        result = self.run_helper(expected=1)
        self.assertIn("reason=networkd_reload_failed", result.stdout)
        self.assertFalse((self.directory / "fail_reload").exists())
        self.assertTrue((self.runtime / PREFERRED).exists())
        self.assertTrue((self.runtime / NONPREFERRED).exists())
        self.assertFalse(any(call.startswith("networkctl reconfigure") for call in self.calls()))
        reloads = self.calls().count("networkctl reload")
        self.run_helper()
        self.assertEqual(self.calls().count("networkctl reload"), reloads + 1)
        self.assertTrue((self.runtime / PREFERRED).exists())
        self.assertTrue((self.runtime / NONPREFERRED).exists())

    def test_reload_must_reestablish_selected_readiness(self):
        self.add_interfaces()
        for state, expected in (("lost_on_reload", 0), ("lost_on_reload_forever", 1)):
            with self.subTest(state=state):
                for name in ("calls", "probes", "reload_probe", "selected_status"):
                    (self.directory / name).unlink(missing_ok=True)
                self.env["TEST_ADDRESS_STATE"] = state
                self.env["SCOUT_NETWORK_WAIT_SECONDS"] = "5" if not expected else "2"
                result = self.run_helper(expected=expected)
                self.assertEqual((self.runtime / PREFERRED).exists(), not expected)
                if not expected:
                    self.assertEqual(int((self.directory / "reload_probe").read_text()), 1)
                    self.assertEqual(int((self.directory / "probes").read_text()), 3)
                    self.assertEqual(int((self.directory / "selected_status").read_text()), 4)
                    self.assertNotIn("networkctl reconfigure enp1s0", self.calls())
                else:
                    self.assertIn("reason=preferred_interface_timeout", result.stdout)
                    self.assertIn("reason=no_global_address", result.stderr)
                    self.assertEqual(self.calls().count("networkctl reload"), 2)
                    self.assertIn("networkctl reconfigure enp1s0", self.calls())

    def test_competing_addresses_and_routes_must_withdraw(self):
        self.add_interfaces()
        for state, expected, probes in (("converges", 0, 4), ("persists", 1, None)):
            with self.subTest(state=state):
                (self.directory / "withdrawal_probes").unlink(missing_ok=True)
                self.env["TEST_COMPETING_STATE"] = state
                self.env["SCOUT_NETWORK_WAIT_SECONDS"] = "5" if state == "converges" else "2"
                self.run_helper("prepare")
                result = self.run_helper(expected=expected)
                if probes:
                    self.assertEqual(int((self.directory / "withdrawal_probes").read_text()), probes)
                else:
                    self.assertIn("reason=interface_reconfiguration_timeout", result.stdout)
                    self.assertIn("reason=global_ipv6_routes_remain", result.stderr)
                self.assertTrue((self.runtime / PREFERRED).exists())
                self.assertTrue((self.runtime / NONPREFERRED).exists())
                self.assertNotIn("networkctl reconfigure enp1s0", self.calls())

    def test_wait_online_does_not_require_the_system_bus(self):
        self.add_interfaces()
        cases = [
            ("selected ready", True, "ipv4", False, None, True),
            ("selected native wait fails", True, "ipv4", True, "selected_wait_online_failed", True),
            ("selected loses readiness", True, "lost_after_wait", False, None, False),
            ("selected inspection fails", True, "inspection_failed_after_wait", False, "address_inspection_failed", True),
            ("broad bootstrap", False, "ipv4", False, None, False),
            ("broad native wait fails", False, "ipv4", True, "fallback_wait_online_failed", False),
        ]
        for name, selected, state, fail_wait, reason, retained in cases:
            with self.subTest(name=name):
                for filename in ("calls", "probes", "native_wait", "fail_wait", "stub_error"):
                    (self.directory / filename).unlink(missing_ok=True)
                self.cmdline.write_text("mac=02:00:00:00:00:01")
                self.run_helper("prepare")
                if not selected:
                    self.cmdline.write_text("quiet")
                self.env["TEST_ADDRESS_STATE"] = state
                if fail_wait:
                    (self.directory / "fail_wait").touch()
                result = self.run_helper("wait-online", expected=1 if reason else 0)
                if reason:
                    self.assertIn("reason=" + reason, result.stdout)
                self.assertEqual((self.runtime / PREFERRED).exists(), retained)
                self.assertEqual((self.runtime / NONPREFERRED).exists(), retained)
                calls = self.calls()
                self.assertFalse(any(call.startswith("networkctl ") for call in calls))
                waits = ["wait-online --timeout=2"] if selected else []
                if not retained:
                    waits.append("wait-online --any --operational-state=routable --timeout=2")
                self.assertEqual([call for call in calls if call.startswith("wait-online ")], waits)
                reload = "systemctl reload systemd-networkd.service"
                self.assertEqual(reload in calls, not retained)
                if not retained:
                    self.assertEqual(calls[-2:], [reload, waits[-1]])

    def test_handoff_uses_the_downloaded_images_network_support(self):
        self.add_interfaces()
        cases = [
            {"name": "new image prepares its own policy", "early_hook": True, "carrier": "1", "legacy": False},
            {"name": "old image keeps a usable selection", "early_hook": False, "carrier": "1", "legacy": True},
            {"name": "old image keeps recovery when selection is unusable", "early_hook": False, "carrier": "0", "legacy": False},
            {"name": "old image inspection fails visibly", "early_hook": False, "carrier": "1", "legacy": True, "error": True},
        ]
        for index, case in enumerate(cases):
            with self.subTest(name=case["name"]):
                self.run_helper("prepare")
                (self.network / "enp1s0/carrier").write_text(case["carrier"] + "\n")
                nextroot = self.directory / f"nextroot-{index}"
                config = nextroot / "etc/systemd/network"
                config.mkdir(parents=True)
                legacy = config / LEGACY
                legacy.write_text("stale Scout policy\n")
                unrelated = config / "operator.network"
                unrelated.write_text("operator configuration\n")
                if case["early_hook"]:
                    hook = nextroot / EARLY_HOOK
                    hook.parent.mkdir(parents=True)
                    hook.write_text("[Service]\nExecStartPost=/opt/forge/forge-scout-network.sh prepare\n")
                error = case.get("error", False)
                self.env["TEST_ADDRESS_STATE"] = "inspection_failed" if error else "ipv4"
                result = self.run_helper("handoff", nextroot, expected=1 if error else 0)
                self.assertEqual((self.runtime / PREFERRED).exists(), case["early_hook"] or error)
                self.assertEqual((self.runtime / NONPREFERRED).exists(), case["early_hook"] or error)
                self.assertEqual(unrelated.read_text(), "operator configuration\n")
                self.assertEqual(legacy.exists(), case["legacy"])
                if error:
                    self.assertIn("reason=address_inspection_failed", result.stdout)
                    self.assertEqual(legacy.read_text(), "stale Scout policy\n")
                elif case["legacy"]:
                    self.assertIn("Property=!INTERFACE=enp1s0\n", legacy.read_text())
                    self.assertIn("DHCP=no\nIPv6AcceptRA=no\n", legacy.read_text())

    def test_failed_legacy_install_can_be_retried(self):
        self.add_interfaces()
        self.run_helper("prepare")
        nextroot = self.directory / "nextroot"
        config = nextroot / "etc/systemd/network"
        config.parent.mkdir(parents=True)
        config.write_text("temporary mount failure\n")
        self.run_helper("handoff", nextroot, expected=1)
        self.assertTrue((self.runtime / PREFERRED).exists())
        self.assertTrue((self.runtime / NONPREFERRED).exists())

        config.unlink()
        self.run_helper("handoff", nextroot)
        self.assertIn("Property=!INTERFACE=enp1s0\n", (config / LEGACY).read_text())
        self.assertFalse((self.runtime / PREFERRED).exists())
        self.assertFalse((self.runtime / NONPREFERRED).exists())


if __name__ == "__main__":
    unittest.main()
