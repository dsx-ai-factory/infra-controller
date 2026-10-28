#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Export the local Vault public CA as Helm values for Core's admin trust."""

import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--context", required=True)
    parser.add_argument("--namespace", required=True)
    parser.add_argument("--pki-mount", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not args.context.startswith("kind-"):
        parser.error("local Vault CA export requires a kind- context")
    try:
        ca = subprocess.check_output([
            "kubectl", "--context", args.context, "--request-timeout=30s", "-n", args.namespace,
            "exec", "statefulset/nico-vault", "--", "env", "VAULT_ADDR=http://127.0.0.1:8200",
            "vault", "read", "-field=certificate", args.pki_mount + "/cert/ca",
        ], text=True, timeout=45)
        # This endpoint is public. Never publish anything other than its CA.
        if "PRIVATE KEY" in ca or not ca.strip().startswith("-----BEGIN CERTIFICATE-----"):
            raise ValueError("Vault did not return a public CA certificate")
        subprocess.run(["openssl", "x509", "-noout"], input=ca, text=True,
                       capture_output=True, check=True, timeout=5)
        values = json.dumps({"siteConfig": {"adminRootCertPem": ca}}, indent=2) + "\n"
        if args.output.exists() and args.output.read_text() == values:
            return 0
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode="w", dir=args.output.parent, delete=False) as output:
            temporary = Path(output.name)
            try:
                output.write(values)
                output.close()
                temporary.replace(args.output)
            finally:
                temporary.unlink(missing_ok=True)
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        # Commands contain only public endpoint arguments; never print their output.
        print(f"Could not export the local Vault public CA: {error}; output was not changed",
              file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
