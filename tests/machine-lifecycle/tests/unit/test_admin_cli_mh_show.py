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

import subprocess

import pytest

from lib import admin_cli

HOST_ID = "fm100ht0tf951mmj1um2el4hcm6a7tg9klu71bgipfpn67oubloi0a6i7sg"
DPU_ID = "fm100dse41frj7rlir4h898bdpq3o8ihhhc3m9o2g7kqmhr0u5ociumdr70"
HOST = {"machine_id": HOST_ID, "dpus": [{"machine_id": DPU_ID}]}


@pytest.fixture
def recorded(monkeypatch):
    calls = []

    def record(result):
        def fake(command, *args, **kwargs):
            calls.append(command)
            if isinstance(result, Exception):
                raise result
            return result

        monkeypatch.setattr(admin_cli, "run_admin_cli", fake)
        return calls

    return record


def test_asks_for_the_one_host_rather_than_the_whole_site(recorded):
    calls = recorded(HOST)

    assert admin_cli.get_machine_from_mh_show(HOST_ID) == HOST
    assert calls == [["managed-host", "show", HOST_ID]]


def test_accepts_a_host_wrapped_in_managed_hosts(recorded):
    recorded({"managed_hosts": [HOST]})

    assert admin_cli.get_machine_from_mh_show(HOST_ID) == HOST


def test_accepts_a_bare_list(recorded):
    recorded([HOST])

    assert admin_cli.get_machine_from_mh_show(HOST_ID) == HOST


def test_a_dpu_id_returns_the_host_that_owns_it(recorded):
    calls = recorded(HOST)

    assert admin_cli.get_machine_from_mh_show(DPU_ID) == HOST
    assert calls == [["managed-host", "show", DPU_ID]]


def _cli_error(stderr: str) -> subprocess.CalledProcessError:
    return subprocess.CalledProcessError(1, ["managed-host", "show"], stderr=stderr)


def test_a_missing_machine_raises_by_default(recorded):
    recorded(_cli_error("Error: managed host not found"))

    with pytest.raises(Exception, match="not found"):
        admin_cli.get_machine_from_mh_show(HOST_ID)


def test_a_missing_machine_is_none_when_allowed(recorded):
    recorded(_cli_error("Error: managed host not found"))

    assert admin_cli.get_machine_from_mh_show(HOST_ID, allow_missing=True) is None


@pytest.mark.parametrize("allow_missing", [False, True])
def test_an_unrelated_not_found_error_is_preserved(recorded, allow_missing):
    recorded(_cli_error('Error from server (NotFound): pods "nico-api-123" not found'))

    with pytest.raises(subprocess.CalledProcessError):
        admin_cli.get_machine_from_mh_show(HOST_ID, allow_missing=allow_missing)


@pytest.mark.parametrize(
    "stderr",
    [
        "error: the API call to the NICo API server returned code: 'Unauthenticated'",
        "Error, decoded message length too large: found 4892461 bytes",
        "transport error: connection refused",
        "",
    ],
)
def test_a_real_failure_is_never_read_as_a_missing_machine(recorded, stderr):
    recorded(_cli_error(stderr))

    with pytest.raises(subprocess.CalledProcessError):
        admin_cli.get_machine_from_mh_show(HOST_ID, allow_missing=True)


def test_an_empty_response_is_none_when_allowed(recorded):
    recorded({})

    assert admin_cli.get_machine_from_mh_show(HOST_ID, allow_missing=True) is None
