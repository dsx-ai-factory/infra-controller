#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Manage private Ethernet labs independently of their Ubuntu VM disks."""

from contextlib import contextmanager
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import subprocess
import sys
import time

SOURCE = Path(__file__).resolve().parent / "lab"
DEFAULT_ROOT = Path.home() / ".nico-devspace/vfkit-labs"
OWNER = "nico-vfkit-labs-v1\n"
ALPINE_VERSION = "3.24.1"
HTTPS_PROBE = "https://www.cloudflare.com/cdn-cgi/trace"


def run(args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def vmnet_run():
    helper = shutil.which("vmnet-run")
    if helper:
        return helper
    for path in ("/opt/homebrew/opt/vmnet-helper/libexec/vmnet-run",
                 "/opt/vmnet-helper/bin/vmnet-run"):
        if os.access(path, os.X_OK):
            return path
    raise ValueError("install vmnet-helper and put vmnet-run on PATH for bridged networking")


def validate_uplink(uplink, interface):
    if uplink not in ("nat", "bridged"):
        raise ValueError("lab uplink must be nat or bridged")
    if uplink == "bridged" and not interface:
        raise ValueError("--lab-uplink bridged requires --lab-interface")
    if uplink == "nat" and interface:
        raise ValueError("--lab-interface requires --lab-uplink bridged")


class Lab:
    def __init__(self, root, number):
        if not 1 <= number <= 99:
            raise ValueError("lab ID must be between 1 and 99")
        root = Path(root).expanduser()
        if root.is_symlink():
            raise ValueError("lab root must not be a symlink")
        self.root = root.resolve()
        if any(character in str(self.root) for character in (",", "\n", "\r")):
            raise ValueError("lab paths cannot contain commas or newlines")
        self.number = number
        self.directory = self.root / "labs" / str(number)
        if (self.root / "labs").is_symlink() or self.directory.is_symlink():
            raise ValueError("lab state must not be a symlink")
        identifier = hashlib.sha256(str(self.directory).encode()).hexdigest()[:16]
        self.runtime = Path(f"/tmp/nico-vfkit-lab-{os.getuid()}-{identifier}")
        self.socket = self.runtime / "switch.sock"
        self.config_path = self.directory / "config.json"

    @property
    def config(self):
        return json.loads(self.config_path.read_text()) if self.config_path.exists() else None

    @contextmanager
    def locked(self):
        # Never adopt an arbitrary directory (including a standalone vflab tree).
        marker = self.root / ".owner"
        if self.root.exists() and not marker.exists() and any(self.root.iterdir()):
            raise ValueError("lab root must be empty or owned by this launcher")
        self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
        if not marker.exists():
            with marker.open("x") as stream:
                stream.write(OWNER)
        if marker.read_text() != OWNER:
            raise ValueError("unrecognized lab root owner")
        with (self.root / "lock").open("a") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if self.runtime.is_symlink():
                raise ValueError("lab runtime must not be a symlink")
            self.runtime.mkdir(mode=0o700, exist_ok=True)
            if self.runtime.stat().st_uid != os.getuid():
                raise ValueError("lab runtime is not owned by this user")
            self.runtime.chmod(0o700)
            yield

    def pid(self, component):
        path = self.runtime / f"{component}.pid"
        if not path.exists():
            return None
        try:
            pid = int(path.read_text())
            result = subprocess.run(["ps", "-p", str(pid), "-o", "command="],
                                    capture_output=True, text=True, check=False)
        except ValueError:
            return None
        identity = (str(self.runtime / "gateway.sock") if component == "gateway"
                    else str(self.socket))
        # Lab state is shared across checkouts; the originating checkout need
        # not be the one performing this lifecycle operation.
        executable = "vfkit" if component == "gateway" else "/vfkit/lab/lib/switch.py"
        return pid if result.returncode == 0 and executable in result.stdout and identity in result.stdout else None

    def clients(self):
        # Inspect actual attachments, including socket-mode clients outside the
        # managed VM registry. A failed process listing must block shutdown.
        commands = run(["ps", "-axo", "command="], capture_output=True, text=True).stdout
        attachments = []
        for line in commands.splitlines():
            if str(self.runtime / "gateway.sock") in line:
                continue
            for path in re.findall(r"unixSocketPath=([^,\s]+)", line):
                if Path(path).resolve() == self.socket.resolve():
                    attachments.append(line)
                    break
        return attachments

    def require_no_clients(self):
        if self.clients():
            raise ValueError("lab has running VM attachments; stop those VMs before stopping/restarting the lab")

    def status(self):
        return {"lab_id": self.number, "directory": str(self.directory),
                "socket": str(self.socket), "config": self.config,
                "gateway_pid": self.pid("gateway"), "switch_pid": self.pid("switch"),
                "running_attachments": len(self.clients())}

    def environment(self):
        config = self.config
        # Explicit overrides prevent a user's standalone vflab environment from
        # directing image preparation into another checkout or cache.
        return {**os.environ, "VXLAB_VAR": str(self.root), "ALPINE_VERSION": ALPINE_VERSION,
                "ALPINE_ARCH": "aarch64", "VXLAB_LAN_MODE": "dual",
                "VXLAB_UPLINK": config["uplink"],
                "VXLAB_WAN_MAC": config["wan_mac"], "VXLAB_LAN_MAC": config["lan_mac"]}

    def ensure(self, uplink=None, interface=None):
        config = self.config
        if config:
            for key, value in (("uplink", uplink), ("interface", interface)):
                if value is not None and value != config[key]:
                    raise ValueError(f"lab {key} differs from saved configuration; use another lab ID")
        else:
            uplink = uplink or "nat"
            validate_uplink(uplink, interface)
            self.directory.mkdir(parents=True, mode=0o700, exist_ok=True)
            # Roots with the same lab ID must not duplicate WAN MACs on a LAN.
            suffix = hashlib.sha256(str(self.directory).encode()).digest()[:4]
            prefix = "02:" + ":".join(f"{byte:02x}" for byte in suffix)
            config = {"uplink": uplink, "interface": interface,
                      "wan_mac": prefix + ":01", "lan_mac": prefix + ":02"}
            self.config_path.write_text(json.dumps(config, indent=2) + "\n")
        validate_uplink(config["uplink"], config["interface"])
        if config["uplink"] == "bridged":
            vmnet_run()
        if not self.pid("switch"):
            self.require_no_clients()
            self.stop_gateway()
        if self.pid("gateway") and self.pid("switch"):
            self.ready()
            return
        for binary in ("vfkit", "xorriso", "openssl", "ssh-keygen", "curl"):
            if not shutil.which(binary):
                raise ValueError(f"managed labs require {binary} on PATH")
        run([sys.executable, SOURCE / "image/gateway-image.py", "build"], env=self.environment())
        key = self.directory / "id_ed25519"
        if not key.exists():
            run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", key])
        run([sys.executable, SOURCE / "image/gateway-image.py", "prepare", self.number],
            env=self.environment())
        if not self.pid("switch"):
            with (self.directory / "switch.log").open("a") as log:
                subprocess.Popen([sys.executable, str(SOURCE / "lib/switch.py"), str(self.socket)],
                                 stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
            for _ in range(50):
                if self.pid("switch") and self.socket.is_socket():
                    break
                time.sleep(.1)
            else:
                raise ValueError(f"lab switch did not start; inspect {self.directory}/switch.log")
        # Recreate the gateway only after the old process stopped; its ISO and
        # persistent leases/host key remain owned by this one lab.
        for name in ("gateway.sock", "gateway.pid"):
            (self.runtime / name).unlink(missing_ok=True)
        (self.directory / "serial.log").write_text("")
        with (self.directory / "gateway.log").open("a") as log:
            subprocess.Popen([sys.executable, str(SOURCE / "lib/socket_vm.py"), "--", *self.command()],
                             stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                             start_new_session=True)
        self.ready()

    def command(self):
        config = self.config
        assets = self.root / "images" / f"gateway-{ALPINE_VERSION}-aarch64"
        command = ["vfkit", "--cpus", "2", "--memory", "1024", "--bootloader",
                   f'linux,kernel={assets}/vmlinux,initrd={assets}/initramfs-virt,cmdline="console=hvc0 modules=loop,squashfs,sd-mod,usb-storage quiet"',
                   "--device", f"virtio-blk,path={self.directory}/gateway.iso",
                   "--device", f"virtio-fs,sharedDir={self.directory}/data,mountTag=vflab-state",
                   "--device", "virtio-rng", "--device", f"virtio-serial,logFilePath={self.directory}/serial.log",
                   "--restful-uri", f"unix://{self.runtime}/gateway.sock",
                   "--pidfile", str(self.runtime / "gateway.pid")]
        wan = "nat"
        if config["uplink"] == "bridged":
            command = [vmnet_run(), "--operation-mode", "bridged", "--shared-interface",
                       config["interface"], "--"] + command
            wan = "fd=4"
        return command + ["--device", f"virtio-net,{wan},mac={config['wan_mac']}",
                          "--device", f"virtio-net,unixSocketPath={self.socket},mac={config['lan_mac']}"]

    def gateway_ip(self):
        serial = self.directory / "serial.log"
        if not serial.exists():
            return None
        text = serial.read_text(errors="replace").rsplit("OpenRC", 1)[-1]
        matches = re.findall(r"eth0: leased ([0-9.]+) for DHCP", text)
        return matches[-1] if matches else None

    def ssh(self, command, **kwargs):
        address = self.gateway_ip()
        if not address:
            raise ValueError("gateway has no WAN IPv4 lease")
        return run(["ssh", "-F", "/dev/null", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
                    "-o", "ConnectTimeout=5", "-o", "StrictHostKeyChecking=accept-new",
                    "-o", "UserKnownHostsFile=" + json.dumps(str(self.directory / "known_hosts").replace("%", "%%")),
                    "-i", self.directory / "id_ed25519", "root@" + address, command], **kwargs)

    def ready(self):
        for _ in range(60):
            address = self.gateway_ip()
            if address:
                if ipaddress.ip_address(address) in ipaddress.ip_network(f"10.{self.number}.0.0/24"):
                    raise ValueError("lab subnet overlaps the gateway WAN; choose another lab ID")
                try:
                    self.ssh(f'test "$(cat /run/vflab-ready)" = {self.number}',
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, timeout=8)
                    return
                except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
                    pass
            time.sleep(2)
        raise ValueError(f"lab gateway did not become ready; inspect {self.directory}/serial.log")

    def verify(self):
        for family in (4, 6):
            try:
                self.ssh(f"curl -{family} --fail --silent --show-error --connect-timeout 10 "
                         f"--max-time 20 -o /dev/null {shlex.quote(HTTPS_PROBE)}", timeout=30)
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
                raise ValueError(f"lab gateway IPv{family} HTTPS failed; NAT requires working upstream IPv6. "
                                 "Use a bridged lab on a suitable interface; no automatic fallback is performed") from error

    def stop_gateway(self):
        if not self.pid("gateway"):
            return
        # Direct Linux boot does not reliably deliver the virtual power-button
        # request to Alpine. Return the SSH acknowledgement before powering off.
        self.ssh("nohup sh -c 'sleep 1; exec poweroff' </dev/null >/dev/null 2>&1 &", timeout=10)
        for _ in range(60):
            if not self.pid("gateway"):
                return
            time.sleep(1)
        raise ValueError("gateway shutdown timed out; gateway left running")

    def stop(self):
        self.require_no_clients()
        self.stop_gateway()
        pid = self.pid("switch")
        if pid:
            os.kill(pid, signal.SIGTERM)
            for _ in range(50):
                if not self.pid("switch"):
                    return
                time.sleep(.1)
            raise ValueError("switch shutdown timed out; state preserved")

    def delete(self):
        expected = {"config.json", "data", "gateway.iso", "gateway.apkovl.tar.gz",
                    "id_ed25519", "id_ed25519.pub", "known_hosts", "known_hosts.old",
                    "serial.log", "gateway.log", "switch.log"}
        if self.directory.exists() and any(path.name not in expected for path in self.directory.iterdir()):
            raise ValueError("refusing to delete unexpected files in lab state")
        self.stop()
        for directory, folders, _ in os.walk(self.directory):
            if os.path.ismount(directory) or any((Path(directory) / name).is_mount() for name in folders):
                raise ValueError("refusing to delete mounted lab state")
        if self.directory.exists():
            shutil.rmtree(self.directory)
        print("Deleted this lab's gateway data and keys (not recoverable); VM disks and shared image cache preserved")
