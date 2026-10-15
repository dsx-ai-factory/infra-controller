#!/usr/bin/env python3
"""Verify by hand that a file staged into a pod arrives intact.

`kubectl exec -i` does not guarantee stdin is delivered: a remote command that
produces no output can finish before it arrives, leaving an empty file and an
exit status of zero. `write_pod_file` guards against that by having the remote
script report the size it wrote. Unit tests cover the guard's logic; only a real
pod can show whether the delivery itself holds up, and for content larger than a
single packet.

Needs nothing but a namespace you can create a pod in:

    NS=<your namespace>
    kubectl -n $NS create deployment fake-api --image=busybox \
        -- sh -c 'while true; do sleep 3600; done'
    kubectl -n $NS rollout status deployment/fake-api

    POD_FILE_NS=$NS POD_FILE_DEPLOYMENT=fake-api \
        uv run python tools/verify_pod_file_write.py

Each case is written with `write_pod_file`, then read back in a separate exec
and compared byte for byte -- the read-back is independent of the size the write
reported, so a guard that lied would still be caught.

A control runs the bare `cat > path` this replaced. It is expected to FAIL,
writing nothing: that is the bug, still reproducible, and it is what makes the
other results meaningful. A control that passes means this pod cannot reproduce
the problem, so a pass here says less than it appears to.

The rejection path -- a short write raising rather than passing silently -- is
covered by tests/unit/test_kubectl.py, since forcing a truncated delivery on
demand is not something this can do honestly.
"""

from __future__ import annotations

import os
import subprocess
import sys

from lib import kubectl

NAMESPACE = os.getenv("POD_FILE_NS", "")
DEPLOYMENT = os.getenv("POD_FILE_DEPLOYMENT", "fake-api")
REMOTE_ROOT = "/dev/shm/verify-pod-file-write"
# Short enough that a partial delivery is unmistakable against a full one.
CONTROL_PAYLOAD = "ABCDEFGHIJ"

# Sizes either side of what a certificate and key actually need, plus a couple
# that will not fit in one packet.
CASES: list[tuple[str, str]] = [
    ("single byte", "x"),
    ("ten bytes", "ABCDEFGHIJ"),
    ("multi-byte characters", "€ = three bytes, ü = two" * 8),
    ("certificate sized (4 KiB)", "-----BEGIN CERTIFICATE-----\n" + ("QUJD" * 1000)),
    ("large (256 KiB)", "0123456789abcdef" * 16384),
    ("trailing newline", "keeps its\ntrailing newline\n"),
]


def _read_back(pod: str, path: str) -> str:
    """Read a file out of the pod, independently of how it was written."""
    result = subprocess.run(
        ["kubectl", "exec", "-n", NAMESPACE, pod, "--", "cat", path],
        capture_output=True,
        text=True,
    )
    if result.returncode:
        raise RuntimeError(f"could not read {path} back: {result.stderr.strip()}")
    return result.stdout


def _control_bare_redirect(pod: str) -> int | None:
    """Write with the old script shape.

    Returns the bytes that landed, or None if that could not be measured --
    which is not the same as the bug reproducing, and must not be reported as
    though it were.
    """
    path = f"{REMOTE_ROOT}/control"
    written = subprocess.run(
        [
            "kubectl", "exec", "-i", "-n", NAMESPACE, pod, "--", "sh", "-c",
            f"mkdir -p {REMOTE_ROOT} && cat > {path}",
        ],
        input=CONTROL_PAYLOAD,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if written.returncode:
        return None
    measured = subprocess.run(
        ["kubectl", "exec", "-n", NAMESPACE, pod, "--", "sh", "-c", f"wc -c < {path}"],
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if measured.returncode:
        return None
    try:
        return int(measured.stdout.strip())
    except ValueError:
        return None


def main() -> int:
    if not NAMESPACE:
        print("Set POD_FILE_NS to a namespace you can create pods in.", file=sys.stderr)
        return 2

    pod = kubectl.get_deployment_pod(NAMESPACE, DEPLOYMENT)
    print(f"writing into {NAMESPACE}/{pod}\n")

    failures = 0
    try:
        for index, (name, content) in enumerate(CASES):
            path = f"{REMOTE_ROOT}/case-{index}"
            expected = len(content.encode("utf-8"))
            try:
                kubectl.write_pod_file(NAMESPACE, pod, path, content)
                landed = _read_back(pod, path)
            except Exception as error:
                # One unreadable case must not cost the remaining cases or the
                # control, which is what decides whether any of this counts.
                print(f"  FAIL  {name}: {error}")
                failures += 1
                continue
            if landed == content:
                print(f"  ok    {name} ({expected} bytes)")
            else:
                print(
                    f"  FAIL  {name}: wrote {expected} bytes, read back "
                    f"{len(landed.encode('utf-8'))} and the content differs"
                )
                failures += 1

        control = _control_bare_redirect(pod)
        print("")
        if control is None:
            print(
                "control INCONCLUSIVE: the bare 'cat > path' write could not be "
                "measured. Nothing above is evidence either way."
            )
        elif control == len(CONTROL_PAYLOAD.encode("utf-8")):
            print(
                f"control DID NOT reproduce the bug: the bare 'cat > path' wrote "
                f"all {control} bytes on this pod, so a pass above proves little."
            )
        else:
            print(
                f"control reproduces the bug: the bare 'cat > path' wrote {control} "
                f"of {len(CONTROL_PAYLOAD.encode('utf-8'))} bytes, so the results "
                "above mean something."
            )
    finally:
        try:
            kubectl.remove_pod_path(NAMESPACE, pod, REMOTE_ROOT)
        except Exception as error:
            print(f"could not clean up {REMOTE_ROOT}: {error}", file=sys.stderr)

    print(f"\n{len(CASES) - failures}/{len(CASES)} cases landed intact")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
