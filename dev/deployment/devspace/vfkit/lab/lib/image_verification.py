# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Verify publisher signatures with a bundled key, never the user's keyring."""

from pathlib import Path
import shutil
import subprocess
import tempfile


def verify_signature(data: Path, signature: Path, public_key: Path):
    for program in ("gpg", "gpgv"):
        if not shutil.which(program):
            raise ValueError("Image signature verification requires GnuPG; brew install gnupg")
    with tempfile.TemporaryDirectory(prefix="image-gpg-") as temporary:
        home = Path(temporary)
        keyring = home / "publisher.gpg"
        try:
            subprocess.run(["gpg", "--no-options", "--batch", "--homedir", str(home),
                            "--output", str(keyring), "--dearmor", str(public_key)],
                           check=True, capture_output=True, text=True)
            # gpgv trusts only this keyring and does not retrieve keys from a server.
            subprocess.run(["gpgv", "--homedir", str(home), "--keyring", str(keyring),
                            str(signature), str(data)],
                           check=True, capture_output=True, text=True)
        except subprocess.CalledProcessError as error:
            raise ValueError(f"Image signature verification failed for {data.name}: "
                             f"{error.stderr.strip()}") from error
