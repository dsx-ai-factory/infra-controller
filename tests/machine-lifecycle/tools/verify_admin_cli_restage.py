#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""Verify by hand that the admin-cli identity survives losing its pod.

The unit tests cover the decision logic. This exercises what they cannot: the
error strings real kubectl produces when a pod goes away, re-resolving a
deployment to its replacement, and staging files into a real /dev/shm. It needs
no site, no Vault and no API -- the "admin-cli" is /bin/echo and the identity is
two strings -- so it can run against any namespace you can create a pod in.

Stand up something to exec into:

    NS=<your namespace>
    kubectl -n $NS create deployment fake-api --image=busybox \
        -- sh -c 'while true; do sleep 3600; done'
    kubectl -n $NS rollout status deployment/fake-api

Run this against it:

    ADMIN_CLI_K8S_NAMESPACE=$NS \
    ADMIN_CLI_K8S_DEPLOYMENT=fake-api \
    ADMIN_CLI_IN_POD_PATH=/bin/echo \
        uv run python tools/verify_admin_cli_restage.py

Then, in another shell, take the pod away:

    kubectl -n $NS delete pod -l app=fake-api

What to look for. Calls tick along naming the pod they reached, then one fails,
then `re-staged the admin-cli identity into ...` names a different pod and the
calls resume. The staged file check should pass against the new pod. If instead
the run stops on an error that was not recognised, the message is printed in
full: that string is the gap, and belongs in `_lost_staged_identity`.

Ctrl-C to stop. Whatever was staged is removed on the way out, and the harness
reports anything it could not remove so you can check the pod yourself.
"""

from __future__ import annotations

import subprocess
import sys
import time

from lib import admin_cli, kubectl

# Recognisable in a `ps` or an audit log, and obviously not a real credential.
FAKE_CERTIFICATE = "NOT-A-CERTIFICATE-restage-harness"
FAKE_PRIVATE_KEY = "NOT-A-KEY-restage-harness"
POLL_SECONDS = 5


def _stage_initial_identity() -> None:
    """Put the harness in the state `client_identity` would leave it in."""
    pod = kubectl.get_deployment_pod(
        admin_cli.ADMIN_CLI_K8S_NAMESPACE, admin_cli.ADMIN_CLI_K8S_DEPLOYMENT
    )
    directory = f"{admin_cli._STAGING_ROOT}/mlt-restage-harness"
    staged = admin_cli._StagedCertificate(
        pod=pod,
        cert_path=f"{directory}/client.crt",
        key_path=f"{directory}/client.key",
    )
    # Recorded before the writes, for the same reason the code under test does
    # it: the key is written first, so a write that fails part-way has already
    # left one behind, and only a recorded location gets cleaned up.
    admin_cli._staged_locations.clear()
    admin_cli._staged_locations.append((pod, directory))
    kubectl.write_pod_file(
        admin_cli.ADMIN_CLI_K8S_NAMESPACE, pod, staged.key_path, FAKE_PRIVATE_KEY
    )
    kubectl.write_pod_file(
        admin_cli.ADMIN_CLI_K8S_NAMESPACE, pod, staged.cert_path, FAKE_CERTIFICATE
    )
    admin_cli._grpc_api_target = admin_cli._GrpcApiTarget(
        api_url="https://harness.invalid:1079",
        root_ca_path="/dev/null",
        certificate=staged,
    )
    admin_cli._staged_identity = (FAKE_CERTIFICATE, FAKE_PRIVATE_KEY)
    admin_cli._staged_api_build = "harness-build-1"
    print(f"staged into {admin_cli.ADMIN_CLI_K8S_NAMESPACE}/{pod}:{directory}")


def _staged_files_present() -> bool:
    """Whether the identity is readable where the prefix currently points."""
    staged = admin_cli._grpc_api_target.certificate
    result = subprocess.run(
        [
            "kubectl", "exec", "-n", admin_cli.ADMIN_CLI_K8S_NAMESPACE, staged.pod,
            "--", "cat", staged.cert_path,
        ],
        capture_output=True,
        text=True,
    )
    return result.returncode == 0 and FAKE_CERTIFICATE in result.stdout


def main() -> int:
    if admin_cli.ADMIN_CLI_IN_POD_PATH != "/bin/echo":
        print(
            "Refusing to run: set ADMIN_CLI_IN_POD_PATH=/bin/echo so this cannot "
            "invoke a real admin-cli.",
            file=sys.stderr,
        )
        return 2

    # Staging is inside the try: it writes the key before the certificate, so a
    # failure part-way through still owes cleanup.
    try:
        _stage_initial_identity()
        print(f"polling every {POLL_SECONDS}s -- delete the pod in another shell\n")
        restaged_from = admin_cli._grpc_api_target.certificate.pod
        while True:
            pod = admin_cli._grpc_api_target.certificate.pod
            try:
                admin_cli._run_admin_cli_process(
                    ["restage-harness-probe"], json_output=False, timeout=60
                )
            except subprocess.CalledProcessError as error:
                stderr = (error.stderr or "").strip()
                recognised = admin_cli._lost_staged_identity(stderr)
                print(
                    f"  call failed against {pod}\n"
                    f"    recognised as a lost pod: {recognised}\n"
                    f"    stderr: {stderr}"
                )
                if not recognised:
                    print(
                        "\nThat string is not matched by _lost_staged_identity, so "
                        "no re-stage was attempted. Add it there.",
                        file=sys.stderr,
                    )
                    return 1
                # A recognised failure is retried inside _run_admin_cli_process;
                # reaching here means the retry failed too.
                print("    re-stage did not recover the call", file=sys.stderr)
                return 1

            now = admin_cli._grpc_api_target.certificate.pod
            if now != restaged_from:
                print(f"  RE-STAGED: {restaged_from} -> {now}")
                print(f"  identity readable in the new pod: {_staged_files_present()}")
                print(f"  locations owed cleanup: {admin_cli._staged_locations}")
                restaged_from = now
            else:
                print(f"  ok against {now}")
            time.sleep(POLL_SECONDS)
    except KeyboardInterrupt:
        print("\nstopping")
    finally:
        leftover = []
        for pod, directory in list(admin_cli._staged_locations):
            if not admin_cli._discard_staged_location(pod, directory, warn=False):
                leftover.append((pod, directory))
        if leftover:
            print(f"could not remove: {leftover}", file=sys.stderr)
        else:
            print("cleaned up every staged location")
    return 0


if __name__ == "__main__":
    sys.exit(main())
