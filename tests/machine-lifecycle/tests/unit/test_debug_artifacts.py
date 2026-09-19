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

import os
import stat

from lib.debug_artifacts import publish_console_password


def test_writes_the_password_into_the_artifact_directory(monkeypatch, tmp_path):
    monkeypatch.setenv("ARTIFACT_DIR", str(tmp_path))

    report = publish_console_password("hunter2hunter2hunter")

    assert report is not None
    body = report.read_text(encoding="utf-8")
    assert "hunter2hunter2hunter" in body
    assert "username: machine-lifecycle-test-user" in body


def test_creates_the_artifact_directory_when_absent(monkeypatch, tmp_path):
    target = tmp_path / "artifacts" / "nested"
    monkeypatch.setenv("ARTIFACT_DIR", str(target))

    report = publish_console_password("hunter2hunter2hunter")

    assert report is not None and report.is_file()


def test_includes_the_instance_address_when_known(monkeypatch, tmp_path):
    monkeypatch.setenv("ARTIFACT_DIR", str(tmp_path))

    report = publish_console_password("pw", instance_ip_address="10.0.0.5")

    assert "10.0.0.5" in report.read_text(encoding="utf-8")


def test_returns_none_when_the_platform_supplies_no_artifact_directory(monkeypatch):
    monkeypatch.delenv("ARTIFACT_DIR", raising=False)

    assert publish_console_password("hunter2hunter2hunter") is None


def test_an_unwritable_artifact_directory_does_not_raise(monkeypatch, tmp_path):
    """Losing the debug credential must not fail an otherwise good run."""
    blocker = tmp_path / "not-a-directory"
    blocker.write_text("", encoding="utf-8")
    monkeypatch.setenv("ARTIFACT_DIR", str(blocker / "artifacts"))

    assert publish_console_password("hunter2hunter2hunter") is None


def test_explains_how_to_avoid_too_many_authentication_failures(monkeypatch, tmp_path):
    """ssh offers loaded keys first and sshd disconnects before prompting."""
    monkeypatch.setenv("ARTIFACT_DIR", str(tmp_path))

    body = publish_console_password("hunter2hunter2hunter").read_text(encoding="utf-8")

    assert "PreferredAuthentications=password" in body
    assert "PubkeyAuthentication=no" in body
    assert "Too many authentication failures" in body


def test_the_password_file_is_not_world_readable(monkeypatch, tmp_path):
    """It carries passwordless sudo, so the umask default is not good enough."""
    monkeypatch.setenv("ARTIFACT_DIR", str(tmp_path / "artifacts"))
    original_umask = os.umask(0o022)
    try:
        report = publish_console_password("hunter2hunter2hunter")
    finally:
        os.umask(original_umask)

    assert stat.S_IMODE(report.stat().st_mode) == 0o600
    assert stat.S_IMODE(report.parent.stat().st_mode) == 0o700


def test_an_existing_artifact_is_replaced_not_rewritten_in_place(monkeypatch, tmp_path):
    """A re-run must not write the password into a loosely-moded inode.

    Truncating the old file and chmod'ing afterwards leaves a window in which
    anything can open it at the old mode; permissions are checked at open, so
    such a reader would then see the new password. Replacing the inode means a
    reader that won that race is left holding the old one.
    """
    monkeypatch.setenv("ARTIFACT_DIR", str(tmp_path))
    stale = tmp_path / "debug-access.txt"
    stale.write_text("stale", encoding="utf-8")
    os.chmod(stale, 0o644)
    stale_inode = stale.stat().st_ino
    # Holds the old inode open exactly as a racing reader would.
    with open(stale, encoding="utf-8") as racing_reader:
        report = publish_console_password("hunter2hunter2hunter")
        assert "hunter2hunter2hunter" not in racing_reader.read()

    assert stat.S_IMODE(report.stat().st_mode) == 0o600
    assert report.stat().st_ino != stale_inode
    assert "stale" not in report.read_text(encoding="utf-8")


def test_no_staged_temporary_file_is_left_behind(monkeypatch, tmp_path):
    """The staging file must not survive to be collected as an artifact."""
    monkeypatch.setenv("ARTIFACT_DIR", str(tmp_path))

    publish_console_password("hunter2hunter2hunter")

    assert [p.name for p in tmp_path.iterdir()] == ["debug-access.txt"]
