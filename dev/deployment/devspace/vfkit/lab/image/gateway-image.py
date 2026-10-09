#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Build an offline Alpine live image and per-lab boot overlays; never install a disk."""
from __future__ import annotations

import argparse
import fcntl
import gzip
import hashlib
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
VAR = Path(os.environ.get("VXLAB_VAR", ROOT / "var"))
VERSION = os.environ.get("ALPINE_VERSION", "3.24.1")
ARCH = os.environ.get("ALPINE_ARCH", "aarch64")
IMAGES = VAR / "images"
ASSETS = IMAGES / f"gateway-{VERSION}-{ARCH}"
WORLD = ROOT / "image/gateway-world"
CMDLINE = "console=hvc0 modules=loop,squashfs,sd-mod,usb-storage quiet"
KEY_NAME = "vflab-repository.rsa.pub"


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def overlay(path: Path, files: dict[str, tuple[bytes, int]]):
    """Numeric root ownership; explicit services, without Alpine firstboot."""
    services = {
        "sysinit": "devfs dmesg mdev hwdrivers modloop",
        "boot": "modules sysctl hostname bootmisc syslog",
        "shutdown": "mount-ro killprocs savecache",
        "default": "local",
    }
    with tarfile.open(path, "w:gz") as archive:
        for name, (data, mode) in files.items():
            entry = tarfile.TarInfo(name)
            entry.mode, entry.size = mode, len(data)
            archive.addfile(entry, io.BytesIO(data))
        for level, names in services.items():
            for name in names.split():
                entry = tarfile.TarInfo(f"etc/runlevels/{level}/{name}")
                entry.type, entry.mode = tarfile.SYMTYPE, 0o777
                entry.linkname = f"/etc/init.d/{name}"
                archive.addfile(entry)


def make_iso(source: Path, target: Path, apkovl: Path, packages: Path | None = None):
    # Stage a fresh image then rename, never modify a running guest's boot media.
    with tempfile.TemporaryDirectory(prefix="iso-", dir=target.parent) as temporary:
        staged = Path(temporary) / "gateway.iso"
        command = ["xorriso", "-indev", source, "-outdev", staged,
                   "-map", apkovl, "/localhost.apkovl.tar.gz"]
        if packages:
            command += ["-rm_r", "/apks", "--", "-map", packages, "/apks"]
        run(*command, "-commit", "-end", stdout=subprocess.DEVNULL)
        staged.replace(target)


def stock_iso() -> Path:
    filename = f"alpine-virt-{VERSION}-{ARCH}.iso"
    path = IMAGES / "downloads" / filename
    if path.exists():
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    url = f"https://dl-cdn.alpinelinux.org/alpine/v{VERSION.rsplit('.', 1)[0]}/releases/{ARCH}/{filename}"
    run("curl", "-fL", "--retry", "3", "-o", str(path) + ".part", url)
    checksum = run("curl", "-fsSL", url + ".sha256", capture_output=True, text=True).stdout.split()[0]
    pending = Path(str(path) + ".part")
    if hashlib.sha256(pending.read_bytes()).hexdigest() != checksum:
        raise SystemExit("Alpine ISO SHA-256 mismatch")
    pending.replace(path)
    return path


def sign_index(index: Path, directory: Path):
    """Alpine v2 index signature: a tar fragment followed by the original gzip stream."""
    private = directory / "repository.rsa"
    run("openssl", "genrsa", "-out", private, "2048", stdout=subprocess.DEVNULL)
    private.chmod(0o600)
    run("openssl", "rsa", "-in", private, "-pubout", "-out", directory / KEY_NAME,
        stdout=subprocess.DEVNULL)
    signature = run("openssl", "dgst", "-sha256", "-sign", private, index, capture_output=True).stdout
    entry = tarfile.TarInfo(f".SIGN.RSA256.{KEY_NAME}")
    entry.mode, entry.size = 0o644, len(signature)
    # No tar EOF blocks: apk expects the following compressed tar member to continue it.
    fragment = entry.tobuf() + signature + b"\0" * (-len(signature) % 512)
    index.write_bytes(gzip.compress(fragment, mtime=0) + index.read_bytes())


def fingerprint() -> str:
    digest = hashlib.sha256((VERSION + ARCH).encode())
    for path in (WORLD, ROOT / "guest/fetch-gateway-packages.sh", Path(__file__)):
        digest.update(path.read_bytes())
    return digest.hexdigest()


def build():
    if ARCH != "aarch64":
        raise SystemExit("This vfkit live-image builder supports aarch64 only")
    IMAGES.mkdir(parents=True, exist_ok=True)
    with (IMAGES / "gateway-build.lock").open("w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        marker = ASSETS / "complete"
        if marker.exists() and marker.read_text().strip() == fingerprint():
            print(f"# gateway image assets already built: {ASSETS}", flush=True)
            return
        # Refuse to replace boot assets while a gateway uses them.
        processes = run("ps", "-axo", "command=", capture_output=True, text=True).stdout
        if any(str(ASSETS / "vmlinux") in line and "--bootloader" in line for line in processes.splitlines()):
            raise SystemExit("Stop diskless gateways before rebuilding their shared boot assets")
        source = stock_iso()
        with tempfile.TemporaryDirectory(prefix="gateway-build-", dir=IMAGES) as temporary:
            work = Path(temporary)
            extracted = work / "boot"
            run("xorriso", "-osirrox", "on", "-indev", source, "-extract", "/boot", extracted,
                stdout=subprocess.DEVNULL)
            run("python3", ROOT / "image/extract-kernel.py", extracted / "vmlinuz-virt", work / "vmlinux")
            shutil.copyfile(extracted / "initramfs-virt", work / "initramfs-virt")
            output = work / "output"
            output.mkdir()
            shutil.copyfile(WORLD, output / "world")
            branch = VERSION.rsplit(".", 1)[0]
            (output / "repositories").write_text("".join(
                f"https://dl-cdn.alpinelinux.org/alpine/v{branch}/{repo}\n" for repo in ("main", "community")))
            apkovl = work / "builder.apkovl.tar.gz"
            overlay(apkovl, {
                "etc/apk/world": (b"dhcpcd\ndhcpcd-openrc\nifupdown-ng\n", 0o644),
                "etc/local.d/fetch.start": ((ROOT / "guest/fetch-gateway-packages.sh").read_bytes(), 0o755),
            })
            iso = work / "builder.iso"
            make_iso(source, iso, apkovl)
            state = IMAGES / "gateway-builder-state"
            state.mkdir(exist_ok=True)
            print(f"# fetching offline packages in a disposable diskless VM; log: {state}/serial.log", flush=True)
            with (state / "builder.log").open("w") as log:
                process = subprocess.Popen([
                    "vfkit", "--cpus", "2", "--memory", "1024", "--bootloader",
                    f'linux,kernel={work}/vmlinux,initrd={work}/initramfs-virt,cmdline="{CMDLINE}"',
                    "--device", f"virtio-blk,path={iso}",
                    "--device", f"virtio-fs,sharedDir={output},mountTag=vflab-build",
                    "--device", "virtio-net,nat", "--device", "virtio-rng",
                    "--device", f"virtio-serial,logFilePath={state}/serial.log",
                ], stdin=subprocess.DEVNULL, stdout=log, stderr=log)
                try:
                    status = process.wait(timeout=600)
                    if status or not (output / "success").exists():
                        raise SystemExit(f"Package build failed; see {state}/serial.log and builder.log")
                finally:
                    if process.poll() is None:
                        process.terminate()
                        process.wait(timeout=45)
            sign_index(output / "apks/aarch64/APKINDEX.tar.gz", work)
            (output / "apks/.boot_repository").touch()
            make_iso(source, work / "base.iso", apkovl, output / "apks")
            # The builder apkovl is replaced with the lab overlay before use.
            ASSETS.mkdir(exist_ok=True)
            marker.unlink(missing_ok=True)
            for name in ("vmlinux", "initramfs-virt", "base.iso", KEY_NAME):
                shutil.copyfile(work / name, ASSETS / name)
            shutil.copyfile(output / "world", ASSETS / "world")
            shutil.copyfile(output / "apks/aarch64/APKINDEX.tar.gz", ASSETS / "APKINDEX.tar.gz")
            marker.write_text(fingerprint() + "\n")
            print(f"# diskless gateway assets ready: {ASSETS}", flush=True)


def prepare(x: int):
    lan_mode = os.environ.get("VXLAB_LAN_MODE", "dual") or "dual"
    if lan_mode not in ("dual", "ipv4", "ipv6"):
        raise SystemExit("VXLAB_LAN_MODE must be dual, ipv4 or ipv6")
    if not 1 <= x <= 99:
        raise SystemExit("lab index must be 1..99")
    if not (ASSETS / "complete").exists():
        raise SystemExit("Build the managed gateway image assets before preparing a lab")
    directory = VAR / "labs" / str(x)
    directory.mkdir(parents=True, exist_ok=True)
    data = directory / "data"
    data.mkdir(mode=0o700, exist_ok=True)
    files = {
        "etc/apk/world": ((ASSETS / "world").read_bytes(), 0o644),
        f"etc/apk/keys/{KEY_NAME}": ((ASSETS / KEY_NAME).read_bytes(), 0o644),
        "etc/hostname": (f"alpine-gw-{x}\n".encode(), 0o644),
        "root/.ssh/authorized_keys": ((directory / "id_ed25519.pub").read_bytes(), 0o600),
        "etc/vflab/lab.conf": ((f"X={x}\nLAB_HOSTNAME=alpine-gw-{x}\n"
                                f"WAN_MAC={os.environ['VXLAB_WAN_MAC']}\n"
                                f"LAN_MAC={os.environ['VXLAB_LAN_MAC']}\n"
                                f"export VXLAB_LAN_MODE={lan_mode}\n"
                                f"export VXLAB_UPLINK={os.environ['VXLAB_UPLINK']}\n").encode(), 0o644),
        "etc/local.d/gateway.start": ((ROOT / "guest/diskless-gateway.sh").read_bytes(), 0o755),
        "etc/init.d/vflab-ipv6-router": ((ROOT / "guest/ipv6-router-service").read_bytes(), 0o755),
    }
    for name in ("networking-gateway.sh", "install-gateway.sh", "renew-ipv6-router.sh"):
        files[f"usr/local/lib/vflab/{name}"] = ((ROOT / "guest" / name).read_bytes(), 0o755)
    apkovl = directory / "gateway.apkovl.tar.gz"
    overlay(apkovl, files)
    make_iso(ASSETS / "base.iso", directory / "gateway.iso", apkovl)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("build", "prepare"))
    parser.add_argument("lab", nargs="?", type=int)
    args = parser.parse_args()
    if args.command == "build":
        if args.lab is not None:
            parser.error("build takes no lab index")
        build()
    else:
        if args.lab is None:
            parser.error("prepare requires a lab index")
        prepare(args.lab)
