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

import pytest

from lib import output
from tests.lifecycle import machine_lifecycle_test as lifecycle


def test_stage_and_failure_banners_identify_failure_location(capsys):
    output.start_stage("Machine reingestion")
    output.print_failure("machine did not become Ready")

    captured = capsys.readouterr()
    assert "STAGE: Machine reingestion" in captured.out
    assert "MLT FAILED" in captured.err
    assert "Stage:  Machine reingestion" in captured.err
    assert "Reason: machine did not become Ready" in captured.err


def test_error_and_exit_uses_active_stage_in_failure_banner(monkeypatch, capsys):
    monkeypatch.delenv("PYTEST_VERSION", raising=False)
    output.start_stage("Temporary operating-system creation")

    with pytest.raises(SystemExit):
        lifecycle._error_and_exit("OS did not become Ready")

    captured = capsys.readouterr()
    assert "MLT FAILED" in captured.err
    assert "Stage:  Temporary operating-system creation" in captured.err
    assert "Reason: OS did not become Ready" in captured.err


def test_success_banner_includes_duration(capsys):
    output.print_success(65.4321)

    captured = capsys.readouterr()
    assert "MLT PASSED" in captured.out
    assert "Duration: 65.4 seconds" in captured.out
