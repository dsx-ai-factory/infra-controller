#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Image authenticity checks without booting guests or downloading artifacts."""

import importlib.util
import hashlib
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("gateway_image_verification", ROOT / "lab/image/gateway-image.py")
gateway = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gateway)


class GatewayImageTests(unittest.TestCase):
    def test_cached_iso_with_invalid_signature_is_rejected_offline(self):
        with tempfile.TemporaryDirectory() as temporary:
            images = Path(temporary)
            cached = images / "downloads" / f"alpine-virt-{gateway.VERSION}-{gateway.ARCH}.iso"
            cached.parent.mkdir()
            cached.write_bytes(b"substituted ISO")
            cached.with_suffix(".iso.asc").write_text("invalid signature")
            with patch.object(gateway, "IMAGES", images), \
                    patch.object(gateway, "run", side_effect=AssertionError("unexpected download")), \
                    self.assertRaisesRegex(ValueError, "signature"):
                gateway.stock_iso()

    def test_unverified_download_is_not_published(self):
        with tempfile.TemporaryDirectory() as temporary:
            images = Path(temporary)
            content = b"substituted image with matching unsigned checksum"

            def download(*args, **kwargs):
                if "-o" in args:
                    target = Path(args[args.index("-o") + 1])
                    target.write_bytes(b"invalid signature" if args[-1].endswith(".asc") else content)
                return subprocess.CompletedProcess(args, 0, hashlib.sha256(content).hexdigest())

            with patch.object(gateway, "IMAGES", images), patch.object(gateway, "run", side_effect=download), \
                    self.assertRaisesRegex(ValueError, "signature"):
                gateway.stock_iso()
            self.assertEqual(list((images / "downloads").glob("*.iso")), [])


class SignatureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Keep GPG agent socket paths within macOS's Unix socket path limit.
        cls.temporary = tempfile.TemporaryDirectory(prefix="image-key-", dir="/tmp")
        cls.addClassCleanup(cls.temporary.cleanup)
        cls.root = Path(cls.temporary.name)
        cls.home = cls.root / "signer"
        cls.home.mkdir(mode=0o700)
        cls.addClassCleanup(subprocess.run,
                            ["gpgconf", "--homedir", str(cls.home), "--kill", "all"], check=True)
        cls.command = ["gpg", "--no-options", "--batch", "--homedir", str(cls.home),
                       "--pinentry-mode", "loopback", "--passphrase", ""]
        for name in ("publisher", "substitute"):
            subprocess.run([*cls.command, "--quick-generate-key", name, "ed25519", "sign", "0"],
                           check=True, capture_output=True)
            subprocess.run([*cls.command, "--armor", "--output", str(cls.root / f"{name}.asc"),
                            "--export", name], check=True, capture_output=True)
        cls.data = cls.root / "image"
        cls.data.write_bytes(b"authentic image")
        cls.signature = cls.root / "image.asc"
        subprocess.run([*cls.command, "--local-user", "publisher", "--armor", "--detach-sign",
                        "--output", str(cls.signature), str(cls.data)], check=True, capture_output=True)

    def test_only_selected_publisher_is_trusted_even_with_other_keys_in_user_keyring(self):
        # The signing home contains both keys; verification must ignore GNUPGHOME.
        with patch.dict("os.environ", {"GNUPGHOME": str(self.home)}):
            gateway.image_verification.verify_signature(self.data, self.signature, self.root / "publisher.asc")
            with self.assertRaisesRegex(ValueError, "signature"):
                gateway.image_verification.verify_signature(self.data, self.signature, self.root / "substitute.asc")

    def test_signed_bytes_cannot_be_changed(self):
        tampered = self.root / "tampered"
        tampered.write_bytes(self.data.read_bytes() + b"changed")
        with self.assertRaisesRegex(ValueError, "signature"):
            gateway.image_verification.verify_signature(tampered, self.signature, self.root / "publisher.asc")

    def test_valid_cached_iso_works_offline(self):
        images = self.root / "images"
        cached = images / "downloads" / f"alpine-virt-{gateway.VERSION}-{gateway.ARCH}.iso"
        cached.parent.mkdir(parents=True)
        cached.write_bytes(self.data.read_bytes())
        cached.with_suffix(".iso.asc").write_bytes(self.signature.read_bytes())
        with patch.object(gateway, "IMAGES", images), \
                patch.object(gateway, "IMAGE_SIGNING_KEY", self.root / "publisher.asc"), \
                patch.object(gateway, "run", side_effect=AssertionError("unexpected download")):
            self.assertEqual(gateway.stock_iso(), cached)


if __name__ == "__main__":
    unittest.main()
