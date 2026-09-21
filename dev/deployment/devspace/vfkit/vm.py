#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""One Ubuntu VM for Docker, DevSpace/MAT, and native Linux development."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid

SCRIPT_DIR = Path(__file__).resolve().parent
REPO = SCRIPT_DIR.parents[3]
GUEST_REPO = "/home/nico/infra-controller"
IMAGE_URL = "https://cloud-images.ubuntu.com/releases/noble/release/ubuntu-24.04-server-cloudimg-arm64.img"


def run(args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def host_git(*args, **kwargs):
    """Run Git against this checkout, ignoring inherited repository overrides."""
    environment = os.environ.copy()
    # Git hooks export repository-local variables that take precedence over -C.
    local_vars = run(["git", "rev-parse", "--local-env-vars"],
                     capture_output=True, text=True).stdout.splitlines()
    for name in local_vars:
        environment.pop(name, None)
    return run(["git", "-C", REPO, *args], env=environment, **kwargs)


def check_vfkit_version():
    """Require cloud-init network-config support before creating or starting a VM."""
    version = run(["vfkit", "--version"], capture_output=True, text=True).stdout.strip()
    match = re.fullmatch(r"vfkit version: v?(\d+)\.(\d+)\.(\d+)", version)
    if not match or tuple(map(int, match.groups())) < (0, 6, 2):
        raise ValueError(f"vfkit 0.6.2 or newer is required for cloud-init network-config; "
                         f"found {version!r}. Upgrade with: brew upgrade vfkit")


def digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            checksum.update(block)
    return checksum.hexdigest()


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def device_path(path):
    # vfkit uses comma-separated device properties without path escaping.
    value = str(path)
    if any(character in value for character in (",", "\n", "\r")):
        raise ValueError(f"vfkit paths cannot contain commas or newlines: {value!r}")
    return value


def check_volume(path):
    path = path.expanduser().resolve()
    if path.parts[1:2] == ("Volumes",):
        volume = Path(*path.parts[:3])
        if not os.path.ismount(volume):
            raise ValueError(f"external volume is not mounted: {volume}")
    return path


def cloud_config(public_key, mac, instance_id, data_disk=False, network_mode="nat"):
    # Socket activation gives SSH a vsock listener without a guest networking
    # dependency. The host exposes it only through a private Unix socket.
    files = {
        "/etc/docker/daemon.json": json.dumps({"ipv6": True, "ip6tables": True,
                                              "fixed-cidr-v6": "fd00:6e69:636f:1::/64"}),
        "/etc/systemd/system/nico-ssh.socket": """[Unit]
Description=NICo SSH over virtio-vsock
After=ssh.service
[Socket]
ListenStream=vsock::22
Accept=yes
[Install]
WantedBy=sockets.target
""",
        "/etc/systemd/system/nico-ssh@.service": """[Unit]
Description=NICo SSH connection
[Service]
ExecStartPre=+/usr/bin/mkdir -p /run/sshd
ExecStart=/usr/sbin/sshd -i
StandardInput=socket
StandardError=journal
""",
        "/etc/sysctl.d/90-nico-network.conf": """net.ipv4.ip_forward=1
net.ipv6.conf.all.forwarding=1
net.ipv6.conf.default.forwarding=1
net.ipv6.conf.eth0.accept_ra=2
""",
    }
    user = {
        "hostname": "nico-dev", "manage_etc_hosts": True,
        "ssh_pwauth": False, "disable_root": True,
        "users": [{"name": "nico", "shell": "/bin/bash", "lock_passwd": True,
                   "sudo": "ALL=(ALL) NOPASSWD:ALL", "ssh_authorized_keys": [public_key]}],
        "growpart": {"mode": "auto", "devices": ["/"]}, "resize_rootfs": True,
        "write_files": [{"path": path, "content": content, "permissions": "0644"}
                        for path, content in files.items()],
        "runcmd": [["systemctl", "daemon-reload"],
                   ["systemctl", "enable", "--now", "nico-ssh.socket"],
                   ["sysctl", "--system"]],
    }
    if data_disk:
        user["write_files"].append({"path": "/usr/local/sbin/nico-mount-data",
                                    "permissions": "0755",
                                    "content": (SCRIPT_DIR / "mount-data.sh").read_text()})
        for service in ("docker", "containerd"):
            user["write_files"].append({
                "path": f"/etc/systemd/system/{service}.service.d/10-data-mounts.conf",
                "permissions": "0644",
                "content": "[Unit]\nRequiresMountsFor=/var/lib/docker /var/lib/containerd\n"})
        user["runcmd"].insert(0, ["/usr/local/sbin/nico-mount-data"])
    network = {"version": 2, "ethernets": {"eth0": {
        "match": {"macaddress": mac}, "set-name": "eth0", "dhcp4": True,
        "dhcp6": True, "accept-ra": True,
    }}}
    if network_mode == "nat":
        # Larger HTTPS requests retransmit indefinitely on some macOS NAT/VPN
        # paths. Keep the IPv6 minimum MTU, including after DHCP/RA refreshes.
        network["ethernets"]["eth0"].update({
            "mtu": 1280,
            "dhcp4-overrides": {"use-mtu": False},
            "dhcp6-overrides": {"use-mtu": False},
        })
        user["write_files"].append({
            "path": "/etc/systemd/network/10-netplan-eth0.network.d/10-nico-mtu.conf",
            "permissions": "0644",
            "content": "[IPv6AcceptRA]\nUseMTU=no\n",
        })
        # Cloud-init writes the drop-in after the initial network setup.
        user["runcmd"].extend([["networkctl", "reload"],
                              ["networkctl", "reconfigure", "eth0"]])
    return {"user-data": "#cloud-config\n" + json.dumps(user, indent=2) + "\n",
            "meta-data": json.dumps({"instance-id": instance_id, "local-hostname": "nico-dev"}),
            "network-config": json.dumps(network)}


class VM:
    def __init__(self, directory):
        self.directory = directory.resolve()
        device_path(self.directory)
        self.config_path = self.directory / "config.json"
        self.config = json.loads(self.config_path.read_text()) if self.config_path.exists() else None
        # Unix sockets have a short path limit, including when disks are external.
        identifier = hashlib.sha256(str(self.directory).encode()).hexdigest()[:16]
        self.runtime = Path(f"/tmp/nico-vfkit-{os.getuid()}-{identifier}")
        self.runtime.mkdir(mode=0o700, exist_ok=True)
        if self.runtime.is_symlink() or self.runtime.stat().st_uid != os.getuid():
            raise ValueError(f"runtime directory is not owned by this user: {self.runtime}")
        self.runtime.chmod(0o700)

    def require_config(self):
        if not self.config:
            raise ValueError("VM does not exist; run up first")

    def pid(self):
        path = self.runtime / "vfkit.pid"
        if not path.exists():
            return None
        try:
            pid = int(path.read_text())
            command = run(["ps", "-p", pid, "-o", "command="], capture_output=True, text=True).stdout
        except (ValueError, subprocess.CalledProcessError):
            return None
        # Never act on a PID reused by an unrelated VM or process.
        if "vfkit" in command and str(self.directory / "root.raw") in command:
            return pid
        return None

    def create(self, args):
        if self.config:
            for key in ("cpus", "memory_gib", "disk_gib", "network", "interface", "network_socket",
                        "data_dir", "data_disk_gib"):
                value = getattr(args, key)
                if key == "data_dir" and value is not None:
                    value = str(check_volume(value))
                if value is not None and value != self.config.get(key):
                    raise ValueError(f"{key} differs from saved configuration; use a new --vm-dir")
            return
        if any(self.directory.iterdir()):
            raise ValueError(f"new VM directory must be empty: {self.directory}")
        network = args.network or "nat"
        if network == "bridged" and not args.interface:
            raise ValueError("--network bridged requires --interface (for example en0)")
        if network == "socket" and not args.network_socket:
            raise ValueError("--network socket requires --network-socket from your gateway")
        if args.interface and network != "bridged":
            raise ValueError("--interface requires --network bridged")
        if args.network_socket and network != "socket":
            raise ValueError("--network-socket requires --network socket")
        if args.network_socket:
            if not Path(args.network_socket).is_absolute():
                raise ValueError("--network-socket must be an absolute path")
            device_path(args.network_socket)
        if bool(args.image) != bool(args.image_sha256):
            raise ValueError("--image and --image-sha256 must be supplied together")
        data_dir = check_volume(args.data_dir) if args.data_dir else None
        if data_dir:
            device_path(data_dir)
            if data_dir == REPO or REPO in data_dir.parents or data_dir == self.directory:
                raise ValueError("--data-dir must be separate from the checkout and VM directory")
            if data_dir.exists() and any(data_dir.iterdir()):
                raise ValueError("--data-dir must initially be empty; existing disks are not adopted")
        self.config = {"cpus": args.cpus or 6, "memory_gib": args.memory_gib or 16,
                       "disk_gib": args.disk_gib or (40 if data_dir else 200), "network": network,
                       "data_dir": str(data_dir) if data_dir else None,
                       "data_disk_gib": args.data_disk_gib or 200,
                       "interface": args.interface, "network_socket": args.network_socket,
                       "mac": "02:" + ":".join(f"{b:02x}" for b in os.urandom(5)),
                       "instance_id": str(uuid.uuid4())}
        # Record configuration before downloading so interrupted creations can
        # resume. root.raw is published only after verification and conversion.
        write_json(self.config_path, self.config)

    def ensure_disk(self, args):
        if self.config.get("data_dir"):
            data_dir = check_volume(Path(self.config["data_dir"]))
            data_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
            data_disk = data_dir / "data.raw"
            if not data_disk.exists():
                if self.config.get("data_initialized"):
                    raise ValueError("data disk is missing; refusing to replace it with an empty disk")
                with data_disk.open("xb") as stream:
                    stream.truncate(self.config["data_disk_gib"] * 1024**3)
            elif data_disk.stat().st_size != self.config["data_disk_gib"] * 1024**3:
                raise ValueError("data disk size differs from saved configuration")
            self.config["data_initialized"] = True
            write_json(self.config_path, self.config)
        destination = self.directory / "root.raw"
        if destination.exists():
            return
        if (self.directory / "image.json").exists():
            raise ValueError("root disk is missing; refusing to replace an initialized VM")
        with tempfile.TemporaryDirectory(prefix="image-", dir=self.directory) as temporary:
            work = Path(temporary)
            if args.image:
                source = args.image.resolve()
                expected = args.image_sha256
                image_format = "raw"
            else:
                if not shutil.which("qemu-img"):
                    raise ValueError("qemu-img is required for Ubuntu images; brew install qemu")
                source = work / "ubuntu.img"
                checksums = work / "SHA256SUMS"
                run(["curl", "--fail", "--location", "--retry", "3", "--output", checksums,
                     IMAGE_URL.rsplit("/", 1)[0] + "/SHA256SUMS"])
                matches = [line.split()[0] for line in checksums.read_text().splitlines()
                           if line.split()[-1].lstrip("*") == IMAGE_URL.rsplit("/", 1)[1]]
                if len(matches) != 1:
                    raise ValueError("Ubuntu checksum manifest does not contain one matching image")
                expected = matches[0]
                run(["curl", "--fail", "--location", "--retry", "3", "--output", source, IMAGE_URL])
                image_format = "qcow2"
            if digest(source) != expected.lower():
                raise ValueError("Ubuntu image SHA256 mismatch; no VM disk was created")
            raw = work / "root.raw"
            if image_format == "raw":
                with source.open("rb") as stream:
                    if stream.read(4) == b"QFI\xfb":
                        raise ValueError("--image requires a raw EFI-bootable Ubuntu ARM64 disk, not qcow2")
                if subprocess.run(["cp", "-c", source, raw], stderr=subprocess.DEVNULL).returncode:
                    run(["cp", source, raw])
            else:
                run(["qemu-img", "convert", "-f", "qcow2", "-O", "raw", source, raw])
            size = self.config["disk_gib"] * 1024**3
            if raw.stat().st_size > size:
                raise ValueError("--disk-gib would shrink the image")
            with raw.open("r+b") as stream:
                stream.truncate(size)
            raw.replace(destination)
            write_json(self.directory / "image.json", {"source": str(args.image or IMAGE_URL),
                                                       "sha256": expected})

    def seed(self):
        key = self.directory / "id_ed25519"
        if not key.exists():
            run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", key])
        for name, content in cloud_config(key.with_suffix(".pub").read_text().strip(),
                                         self.config["mac"], self.config["instance_id"],
                                         bool(self.config.get("data_dir")),
                                         self.config["network"]).items():
            (self.directory / name).write_text(content)

    def command(self):
        config = self.config
        command = ["vfkit", "--cpus", str(config["cpus"]),
                   "--memory", str(config["memory_gib"] * 1024),
                   "--bootloader", f"efi,variable-store={self.directory}/efi-store,create",
                   "--device", f"virtio-blk,path={self.directory}/root.raw",
                   "--device", f"virtio-serial,logFilePath={self.directory}/serial.log",
                   "--device", "virtio-rng", "--device", "virtio-balloon",
                   "--device", f"virtio-vsock,port=22,socketURL={self.runtime}/ssh.sock,connect",
                   "--cloud-init", ",".join(str(self.directory / name)
                                             for name in ("user-data", "meta-data", "network-config")),
                   "--restful-uri", f"unix://{self.runtime}/rest.sock",
                   "--pidfile", str(self.runtime / "vfkit.pid")]
        if config.get("data_dir"):
            data_disk = check_volume(Path(config["data_dir"])) / "data.raw"
            if not data_disk.is_file():
                raise ValueError(f"data disk is missing; refusing to start without it: {data_disk}")
            command += ["--device", f"virtio-blk,path={data_disk},deviceId=nico-data"]
        network = config["network"]
        if network == "bridged":
            helper = shutil.which("vmnet-run")
            if not helper:
                for candidate in ("/opt/homebrew/opt/vmnet-helper/libexec/vmnet-run",
                                  "/opt/vmnet-helper/bin/vmnet-run"):
                    if Path(candidate).is_file():
                        helper = candidate
                        break
            if not helper:
                raise ValueError("install vmnet-helper and put vmnet-run on PATH for bridged networking")
            command = [helper, "--operation-mode", "bridged", "--shared-interface",
                       config["interface"], "--"] + command
            attachment = "fd=4"
        elif network == "socket":
            socket = Path(config["network_socket"])
            if not socket.is_socket():
                raise ValueError(f"gateway socket is not available: {socket}")
            attachment = f"unixSocketPath={device_path(socket)}"
        else:
            attachment = "nat"
        return command + ["--device", f"virtio-net,{attachment},mac={config['mac']}"]

    def start(self):
        self.require_config()
        if self.pid():
            print("Reusing running VM", flush=True)
            return
        if not (self.directory / "root.raw").exists():
            raise ValueError("VM disk is missing; run up to finish creation")
        command = self.command()
        for name in ("ssh.sock", "rest.sock", "vfkit.pid"):
            (self.runtime / name).unlink(missing_ok=True)
        with (self.directory / "vfkit.log").open("a") as log:
            subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log,
                             start_new_session=True)
        print(f"Starting Ubuntu VM; logs: {self.directory}/serial.log", flush=True)
        for _ in range(30):
            if self.pid():
                return
            time.sleep(1)
        raise ValueError(f"vfkit did not start; inspect {self.directory}/vfkit.log")

    def ssh_args(self):
        return ["ssh", "-F", "/dev/null", "-o", "BatchMode=yes", "-o", "IdentitiesOnly=yes",
                "-o", "ConnectTimeout=5", "-o", "StrictHostKeyChecking=accept-new",
                "-o", "UserKnownHostsFile=" + json.dumps(str(self.directory / "known_hosts").replace("%", "%%")),
                "-o", "ProxyCommand=" + shlex.join(["/usr/bin/nc", "-U", str(self.runtime / "ssh.sock")]),
                "-i", str(self.directory / "id_ed25519")]

    def ssh(self, *command, **kwargs):
        return run(self.ssh_args() + ["nico@nico-vfkit", shlex.join(command)], **kwargs)

    def ready(self):
        if not self.pid():
            raise ValueError("VM is stopped; run start or up")
        for _ in range(120):
            try:
                # Readiness probes must not consume stdin intended for exec.
                self.ssh("true", stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                self.ssh("sudo", "cloud-init", "status", "--wait", "--long",
                         stdin=subprocess.DEVNULL, stdout=sys.stderr, timeout=600)
                return
            except subprocess.CalledProcessError as error:
                if error.returncode != 255:
                    raise
            if not self.pid():
                raise ValueError("vfkit exited; inspect vfkit.log and serial.log")
            time.sleep(2)
        raise ValueError("SSH did not become ready; inspect serial.log and cloud-init")

    def sync(self):
        self.ssh("mkdir", "-p", GUEST_REPO)
        # Transfer tracked and untracked, nonignored files, including local edits.
        # Excluding .git avoids dangling worktree pointers and copying host credentials.
        files = host_git("ls-files", "-z", "--cached", "--others",
                         "--exclude-standard", capture_output=True).stdout
        files = b"\0".join(name for name in files.split(b"\0")
                            if name and os.path.lexists(REPO / os.fsdecode(name))) + b"\0"
        # BSD tar preserves the repository's absolute, dangling Linux symlinks;
        # Apple's openrsync fails when they occur in --files-from input.
        with tempfile.TemporaryDirectory(prefix="nico-source-") as temporary:
            manifest = Path(temporary) / "files"
            manifest.write_bytes(files)
            environment = dict(os.environ, COPYFILE_DISABLE="1")
            with subprocess.Popen(["tar", "--no-xattrs", "-C", str(REPO), "-cf", "-", "--no-recursion",
                                   "--null", "-T", str(manifest)], stdout=subprocess.PIPE,
                                  env=environment) as archive:
                self.ssh("tar", "-xf", "-", "-C", GUEST_REPO, stdin=archive.stdout)
                archive.stdout.close()
                if archive.wait() != 0:
                    raise ValueError("checkout archive failed")
        # DevSpace needs a resolvable HEAD for image tags. This private guest
        # repository is a working snapshot, not the host's linked worktree.
        self.ssh("bash", "-c", 'if ! git -C "$1" rev-parse --verify HEAD >/dev/null 2>&1; then git -C "$1" init -q && '
                 'git -C "$1" -c user.name="NICo VM" -c user.email="nico@localhost" '
                 'commit -q --allow-empty -m "Initialize VM workspace"; fi', "_", GUEST_REPO)
        self.sync_build_version()

    def sync_build_version(self):
        # The synthetic guest HEAD has no release ancestry. Preserve the host
        # version via the same build-script inputs CI uses, not a fabricated tag.
        version = host_git("describe", "--tags", "--first-parent", "--always", "--long",
                           capture_output=True, text=True).stdout.strip()
        if not re.match(r"^v[0-9]", version):
            raise ValueError("checkout lacks a version tag; fetch repository tags before syncing native tests")
        sha = host_git("rev-parse", "--short=8", "HEAD",
                       capture_output=True, text=True).stdout.strip()
        environment = (f"export VERSION={shlex.quote(version)}\n"
                       f"export CI_COMMIT_SHORT_SHA={shlex.quote(sha)}\n")
        self.ssh("bash", "-c", 'umask 077; mkdir -p "$HOME/.config/nico" && '
                 'cat > "$HOME/.config/nico/vfkit-source-version.sh.tmp" && '
                 'mv "$HOME/.config/nico/vfkit-source-version.sh.tmp" '
                 '"$HOME/.config/nico/vfkit-source-version.sh"', input=environment, text=True)

    def provision(self, deploy=False):
        command = ["sudo", "bash", GUEST_REPO + "/dev/deployment/devspace/setup-devspace-on-host.sh",
                   "--user", "nico", "--repo-dir", GUEST_REPO,
                   "--ip-family", "dual"]
        if not deploy:
            command.append("--skip-deploy")
        self.ssh(*command)

    def verify(self):
        with (SCRIPT_DIR / "verify.sh").open() as stream:
            self.ssh("bash", "-s", "--", self.config["network"],
                     "external" if self.config.get("data_dir") else "root", stdin=stream)

    def prepare_dev(self):
        self.ssh("sudo", "bash", GUEST_REPO + "/dev/deployment/devspace/prepare-ubuntu-host-for-dev.sh",
                 "--user", "nico", "--repo-dir", GUEST_REPO)

    def stop(self):
        if not self.pid():
            print("VM is stopped")
            return
        run(["curl", "--fail", "--silent", "--show-error", "--max-time", "10",
             "--unix-socket", self.runtime / "rest.sock", "-H", "Content-Type: application/json",
             "-d", '{"state":"Stop"}', "http://localhost/vm/state"])
        for _ in range(60):
            if not self.pid():
                print("VM stopped; its disk and caches are preserved")
                return
            time.sleep(1)
        raise ValueError("graceful shutdown timed out; VM left running, inspect the guest before retrying")


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def parser():
    result = argparse.ArgumentParser(description=__doc__, epilog="""
Actions: up creates/starts Ubuntu, syncs this checkout, installs Docker, creates
dual-stack kind and installs native cargo-test dependencies with PostgreSQL.
deploy syncs and runs the full Linux DevSpace setup. prepare-dev refreshes native
dependencies for cargo test, including --profile ci-tests. start/stop
preserve the disk. sync copies local nonignored files without deleting guest files.
verify checks the guest, Docker and kind. ssh opens a shell; exec runs arguments
following -- in the guest checkout. forward tunnels REST/Keycloak to localhost
ports 18388/18082 until Ctrl-C. Configuration is saved on first up; use the same
--vm-dir for subsequent actions. Resource/network options apply only to first up.
""")
    result.add_argument("--vm-dir", type=Path,
                        default=Path(os.environ.get("VFKIT_VM_DIR", "~/.nico-devspace/vfkit")).expanduser(),
                        help="VM state and disk directory, including external storage (default: %(default)s)")
    result.add_argument("--cpus", type=positive, help="vCPUs (default: 6)")
    result.add_argument("--memory-gib", type=positive, help="RAM in GiB (default: 16)")
    result.add_argument("--disk-gib", type=positive, help="root disk GiB (default: 200, or 40 with --data-dir)")
    result.add_argument("--data-dir", type=Path, help="optional separate directory for a data disk backing /home and Docker")
    result.add_argument("--data-disk-gib", type=positive, help="data disk GiB with --data-dir (default: 200)")
    result.add_argument("--network", choices=("nat", "bridged", "socket"),
                        help="nat (default), VMNet bridge, or gateway Unix datagram socket")
    result.add_argument("--interface", help="macOS interface for bridged networking, e.g. en0")
    result.add_argument("--network-socket", help="absolute path to a running gateway's vfkit-compatible socket")
    result.add_argument("--image", type=Path, help="local raw Ubuntu 24.04 ARM64 EFI cloud image; otherwise download official qcow2")
    result.add_argument("--image-sha256", help="required SHA256 of --image")
    result.add_argument("action", choices=("up", "start", "stop", "status", "sync", "deploy",
                                          "prepare-dev", "verify", "ssh", "exec", "forward"))
    return result


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    command = []
    if "--" in argv:
        index = argv.index("--")
        command, argv = argv[index + 1:], argv[:index]
    args = parser().parse_args(argv)
    if (args.action == "exec") != bool(command):
        raise ValueError("exec requires a command after --; other actions do not accept one")
    creation_options = (args.cpus, args.memory_gib, args.disk_gib, args.network,
                        args.interface, args.network_socket, args.image, args.image_sha256,
                        args.data_dir, args.data_disk_gib)
    if args.action != "up" and any(value is not None for value in creation_options):
        raise ValueError("resource, network and image options are accepted only with up")
    if bool(args.image) != bool(args.image_sha256):
        raise ValueError("--image and --image-sha256 must be supplied together")
    if args.image_sha256 and not re.fullmatch(r"[0-9a-fA-F]{64}", args.image_sha256):
        raise ValueError("--image-sha256 must contain exactly 64 hexadecimal characters")
    if args.data_disk_gib and not args.data_dir:
        raise ValueError("--data-disk-gib requires --data-dir")
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise ValueError("requires Apple Silicon macOS")
    if int(platform.mac_ver()[0].split(".")[0]) < 13:
        raise ValueError("EFI boot requires macOS 13 or newer")
    if args.action in ("up", "start"):
        check_vfkit_version()
    directory = check_volume(args.vm_dir)
    if directory.resolve() == REPO or REPO in directory.resolve().parents:
        raise ValueError("--vm-dir must be outside the checkout to avoid copying VM disks into the guest")
    if args.action == "up":
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    if not directory.is_dir():
        raise ValueError(f"VM directory does not exist: {directory}")
    os.umask(0o077)
    vm = VM(directory)
    with (vm.runtime / "lock").open("w") as lock:
        # Exclude concurrent lifecycle/provisioning operations, not interactive SSH.
        if args.action not in ("ssh", "exec", "forward", "status"):
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.action == "up":
            vm.create(args)
            vm.ensure_disk(args)
            vm.seed()
            vm.start()
            vm.ready()
            vm.sync()
            vm.provision()
            vm.prepare_dev()
            vm.verify()
        elif args.action == "stop":
            vm.stop()
        elif args.action == "status":
            print(json.dumps({"directory": str(directory), "pid": vm.pid(), "config": vm.config}, indent=2))
        else:
            vm.require_config()
            if args.action == "start":
                vm.start()
            vm.ready()
            if args.action in ("sync", "deploy", "prepare-dev"):
                vm.sync()
            if args.action == "deploy":
                vm.provision(deploy=True)
                vm.verify()
            elif args.action == "prepare-dev":
                vm.prepare_dev()
            elif args.action == "verify":
                vm.verify()
            elif args.action == "ssh":
                run(vm.ssh_args() + ["-t", "nico@nico-vfkit", f"cd {GUEST_REPO} && exec bash -l"])
            elif args.action == "exec":
                vm.ssh("bash", "-lc", f"cd {GUEST_REPO} && exec {shlex.join(command)}")
            elif args.action == "forward":
                node_ip = vm.ssh("docker", "inspect", "-f",
                                 '{{(index .NetworkSettings.Networks "kind").IPAddress}}',
                                 "nico-dev-control-plane", capture_output=True, text=True).stdout.strip()
                run(vm.ssh_args() + ["-N", "-o", "ExitOnForwardFailure=yes", "-L",
                    f"127.0.0.1:18388:{node_ip}:30388", "-L",
                    f"127.0.0.1:18082:{node_ip}:30082", "nico@nico-vfkit"])


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"[vfkit] {error}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
