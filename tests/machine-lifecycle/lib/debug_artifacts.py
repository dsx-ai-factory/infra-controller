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

"""Publish per-run debug credentials through the platform's artifact directory.

This is the one seam between the test and whatever CI platform is running it.
``ARTIFACT_DIR`` names the directory the platform collects after the job;
each platform has its own way of declaring one. Adding a platform means
pointing that variable somewhere; it does not mean touching the test.

Only credentials the operator explicitly opted in to are published here. The
run's own SSH private key is deliberately not written out: an operator who
wants durable access to a wedged instance supplies ``debug.ssh_public_key``,
which puts nothing secret in the artifact at all.
"""

import contextlib
import os
import tempfile
from pathlib import Path

_ARTIFACT_DIR_ENV = "ARTIFACT_DIR"
_FILENAME = "debug-access.txt"


def publish_console_password(
    password: str, *, instance_ip_address: str | None = None
) -> Path | None:
    """Write the run's console password to the artifact directory.

    Returns the path written, or ``None`` when the platform provides no
    artifact directory. Never raises: losing the debug credential must not
    fail an otherwise good run.
    """
    artifact_dir = os.environ.get(_ARTIFACT_DIR_ENV)
    if not artifact_dir:
        print(
            f"${_ARTIFACT_DIR_ENV} is not set; the console password for this run "
            "will not be recoverable once the job exits"
        )
        return None

    location = "" if instance_ip_address is None else f" at {instance_ip_address}"
    body = (
        "Console access for this machine-lifecycle-test run.\n"
        "\n"
        f"  instance{location}\n"
        "  username: machine-lifecycle-test-user\n"
        f"  password: {password}\n"
        "\n"
        "This is the account the test itself logs in as. It carries passwordless\n"
        "sudo, so treat the password as root. It exists only on the instance this\n"
        "run created and is destroyed with it. Reach it over the BMC serial\n"
        "console or over SSH.\n"
        "\n"
        "Over SSH, force password authentication:\n"
        "\n"
        "  ssh -o PreferredAuthentications=password -o PubkeyAuthentication=no \\\n"
        "      machine-lifecycle-test-user@<instance-ip>\n"
        "\n"
        "Without those options ssh offers every key your agent has loaded first\n"
        "and sshd closes the connection with \"Too many authentication failures\"\n"
        "before it ever gets round to asking for the password.\n"
    )

    try:
        target = Path(artifact_dir)
        target.mkdir(parents=True, exist_ok=True)
        # Best effort, and before the write: the directory is the platform's,
        # and a run that cannot narrow it has still written the file safely.
        try:
            os.chmod(target, 0o700)
        except OSError as error:
            print(f"could not restrict permissions on {target}: {error}")
        report = target / _FILENAME
        # Stage a fresh 0600 inode and swap it in, rather than truncating any
        # existing artifact and narrowing it afterwards. A file already there
        # keeps its old mode until the chmod lands, and a reader that opened it
        # inside that window would keep access to whatever is written next --
        # permissions are checked at open, not per read. os.replace leaves such
        # a reader holding the old inode instead.
        descriptor, staged = tempfile.mkstemp(dir=target, prefix=f".{_FILENAME}.")
        try:
            with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
                handle.write(body)
            os.replace(staged, report)
        except OSError:
            with contextlib.suppress(OSError):
                os.unlink(staged)
            raise
    except OSError as error:
        print(f"could not write debug access artifact: {error}")
        return None

    print(f"Wrote console access credentials to {report}")
    return report
