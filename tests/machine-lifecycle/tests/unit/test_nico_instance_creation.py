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

import datetime
import json
import os
from types import SimpleNamespace

import pytest
import requests

from lib import admin_cli, nico_rest
from tests.lifecycle import machine_lifecycle_test as lifecycle


def _response(payload, status_code=200, headers=None):
    """Build a real requests.Response so lib.nico_rest's parsing runs unmocked."""
    response = requests.Response()
    response.status_code = status_code
    response._content = json.dumps(payload).encode()
    response.headers["Content-Type"] = "application/json"
    for key, value in (headers or {}).items():
        response.headers[key] = value
    return response


@pytest.fixture
def nico_env(monkeypatch):
    monkeypatch.setenv("NICO_BASE_URL", "https://nico.example.test/")
    monkeypatch.setenv("NICO_ORG", "ncx")
    monkeypatch.setenv("NICO_API_NAME", "nico")
    monkeypatch.setenv("NICO_TOKEN", "test-bearer")


@pytest.fixture
def sent(monkeypatch):
    """Capture every request lib.nico_rest puts on the wire."""
    calls = []

    def fake_send(method, url, params, body):
        calls.append(
            {
                "method": method,
                "url": url,
                "params": dict(params) if params else params,
                "body": body,
            }
        )
        return fake_send.response

    fake_send.response = _response({})
    monkeypatch.setattr(nico_rest, "_send", fake_send)
    return SimpleNamespace(calls=calls, set_response=lambda r: setattr(fake_send, "response", r))


def test_create_instance_targets_machine_without_instance_type(
    monkeypatch, nico_env, sent
):
    monkeypatch.setattr(nico_rest, "get_tenant_uuid", lambda: "tenant-id")
    sent.set_response(_response({"id": "instance-id"}))

    instance = nico_rest.create_instance(
        instance_name="mlt-instance",
        machine_id="machine-id",
        network_interface="vpcPrefixId=prefix-id",
        operating_system_uuid="os-id",
        virtual_private_cloud_uuid="vpc-id",
    )

    assert instance == {"id": "instance-id"}
    assert sent.calls == [
        {
            "method": "POST",
            "url": "https://nico.example.test/v2/org/ncx/nico/instance",
            "params": None,
            "body": {
                "name": "mlt-instance",
                "tenantId": "tenant-id",
                "machineId": "machine-id",
                "vpcId": "vpc-id",
                "operatingSystemId": "os-id",
                "interfaces": [{"vpcPrefixId": "prefix-id"}],
                "phoneHomeEnabled": True,
            },
        }
    ]


def test_wait_for_instance_status_retries_transient_transport_failure(
    monkeypatch, capsys
):
    statuses = iter(
        [
            requests.ReadTimeout("REST request timed out"),
            "Ready",
        ]
    )
    sleeps = []

    def get_status(_instance_uuid, _site):
        result = next(statuses)
        if isinstance(result, Exception):
            raise result
        return result

    monkeypatch.setattr(nico_rest, "get_instance_status", get_status)
    monkeypatch.setattr(nico_rest.time, "sleep", sleeps.append)

    nico_rest.wait_for_instance_status(
        "instance-id", nico_rest.Site("example-site"), "Ready", timeout=60
    )

    assert len(sleeps) == 1
    assert 0 < sleeps[0] <= 60
    output = capsys.readouterr().out
    assert "transient REST transport failure" in output
    assert "ReadTimeout: REST request timed out" in output
    assert "attempt 1/10" in output
    assert "instance instance-id reached desired status (Ready)!" in output


def test_wait_for_instance_status_stops_after_ten_consecutive_timeouts(
    monkeypatch,
):
    calls = []
    sleeps = []

    def get_status(_instance_uuid, _site):
        calls.append(None)
        raise requests.ReadTimeout("REST request timed out")

    monkeypatch.setattr(nico_rest, "get_instance_status", get_status)
    monkeypatch.setattr(nico_rest.time, "sleep", sleeps.append)

    with pytest.raises(TimeoutError, match="timed out 10 consecutive times") as exc_info:
        nico_rest.wait_for_instance_status(
            "instance-id", nico_rest.Site("example-site"), "Ready", timeout=60
        )

    assert isinstance(exc_info.value.__cause__, requests.ReadTimeout)
    assert len(calls) == 10
    assert len(sleeps) == 9


def test_successful_status_response_resets_consecutive_timeout_count(monkeypatch):
    statuses = iter(
        [
            requests.ReadTimeout("first timeout"),
            "Provisioning",
            requests.ReadTimeout("second timeout"),
            "Ready",
        ]
    )
    sleeps = []

    def get_status(_instance_uuid, _site):
        result = next(statuses)
        if isinstance(result, Exception):
            raise result
        return result

    monkeypatch.setattr(nico_rest, "MAX_CONSECUTIVE_INSTANCE_STATUS_TIMEOUTS", 2)
    monkeypatch.setattr(nico_rest, "get_instance_status", get_status)
    monkeypatch.setattr(nico_rest.time, "sleep", sleeps.append)

    nico_rest.wait_for_instance_status(
        "instance-id", nico_rest.Site("example-site"), "Ready", timeout=60
    )

    assert len(sleeps) == 3


def test_instance_status_poll_sleep_is_capped_at_remaining_time(monkeypatch):
    sleeps = []
    deadline = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=5)
    monkeypatch.setattr(nico_rest.time, "sleep", sleeps.append)

    nico_rest._sleep_before_next_instance_status_poll(deadline)

    assert len(sleeps) == 1
    assert 0 < sleeps[0] <= 5


def test_instance_status_poll_does_not_sleep_after_deadline(monkeypatch):
    sleeps = []
    deadline = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=1)
    monkeypatch.setattr(nico_rest.time, "sleep", sleeps.append)

    nico_rest._sleep_before_next_instance_status_poll(deadline)

    assert sleeps == []


def test_wait_for_instance_status_does_not_retry_api_error(monkeypatch):
    def get_status(_instance_uuid, _site):
        raise nico_rest.NicoError("REST returned HTTP 500")

    monkeypatch.setattr(nico_rest, "get_instance_status", get_status)

    with pytest.raises(nico_rest.NicoError, match="REST returned HTTP 500"):
        nico_rest.wait_for_instance_status(
            "instance-id", nico_rest.Site("example-site"), "Ready", timeout=60
        )


def test_wait_for_instance_status_does_not_retry_non_transient_request_error(
    monkeypatch,
):
    def get_status(_instance_uuid, _site):
        raise requests.exceptions.InvalidURL("invalid REST URL")

    monkeypatch.setattr(nico_rest, "get_instance_status", get_status)

    with pytest.raises(requests.exceptions.InvalidURL, match="invalid REST URL"):
        nico_rest.wait_for_instance_status(
            "instance-id", nico_rest.Site("example-site"), "Ready", timeout=60
        )


def test_list_pages_until_total_reached(nico_env, monkeypatch):
    """More items than one page must not truncate."""
    monkeypatch.setattr(nico_rest, "PAGE_SIZE", 2)
    pages = [
        _response([{"id": "a"}, {"id": "b"}], headers={"X-Pagination": '{"total": 3}'}),
        _response([{"id": "c"}], headers={"X-Pagination": '{"total": 3}'}),
    ]
    seen = []

    def fake_send(method, url, params, body):
        seen.append(params["pageNumber"])
        return pages[len(seen) - 1]

    monkeypatch.setattr(nico_rest, "_send", fake_send)

    assert nico_rest._list("vpc") == [{"id": "a"}, {"id": "b"}, {"id": "c"}]
    assert seen == ["1", "2"]


def test_expired_bearer_is_refreshed_and_replayed(nico_env, monkeypatch):
    """Replay covers POST, not just idempotent verbs."""
    responses = [_response({"message": "expired"}, status_code=401),
                 _response({"id": "instance-id"})]
    tokens = []

    def fake_send(method, url, params, body):
        tokens.append(__import__("os").environ.get("NICO_TOKEN"))
        return responses[len(tokens) - 1]

    def fake_refresh():
        __import__("os").environ["NICO_TOKEN"] = "fresh-bearer"
        return True

    monkeypatch.setattr(nico_rest, "_send", fake_send)
    monkeypatch.setattr(nico_rest, "refresh_token", fake_refresh)

    response = nico_rest._request("POST", "instance", body={"name": "x"})

    assert response.json() == {"id": "instance-id"}
    assert tokens == ["test-bearer", "fresh-bearer"]


def test_missing_machine_returns_missing_sentinel(nico_env, monkeypatch):
    monkeypatch.setattr(
        nico_rest, "_send", lambda *a: _response({"message": "not found"}, status_code=404)
    )

    assert (
        nico_rest.get_machine_status("machine-id", nico_rest.Site("example-site"), allow_missing_machine=True)
        == "<Missing>"
    )


def test_missing_machine_raises_when_not_allowed(nico_env, monkeypatch):
    monkeypatch.setattr(
        nico_rest, "_send", lambda *a: _response({"message": "not found"}, status_code=404)
    )

    with pytest.raises(nico_rest.NicoError, match="HTTP 404"):
        nico_rest.get_machine_status("machine-id", nico_rest.Site("example-site"))


def test_machine_without_instance_type_is_safe(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_m_show",
        lambda _machine_id, allow_missing: {"instance_type_id": None},
    )

    lifecycle.verify_machine_has_no_instance_type(
        SimpleNamespace(machine_under_test="machine-id")
    )


def test_missing_machine_is_rejected(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_m_show",
        lambda _machine_id, allow_missing: None,
    )

    def fail(message):
        raise ValueError(message)

    monkeypatch.setattr(lifecycle, "_error_and_exit", fail)

    with pytest.raises(ValueError, match="was not found"):
        lifecycle.verify_machine_has_no_instance_type(
            SimpleNamespace(machine_under_test="machine-id")
        )


def test_machine_with_instance_type_is_rejected(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_m_show",
        lambda _machine_id, allow_missing: {"instance_type_id": "type-id"},
    )

    def fail(message):
        raise ValueError(message)

    monkeypatch.setattr(lifecycle, "_error_and_exit", fail)

    with pytest.raises(ValueError, match="dissociate this instance type"):
        lifecycle.verify_machine_has_no_instance_type(
            SimpleNamespace(machine_under_test="machine-id")
        )


def test_refresh_re_exchanges_the_retained_credential(monkeypatch):
    """The real refresh path, not a stand-in: a full run outlasts the bearer."""
    monkeypatch.setattr(
        nico_rest,
        "_token_refresh",
        ("client-id:client-secret", "https://issuer.example.test/token", "carbide"),
    )
    monkeypatch.setenv("NICO_TOKEN", "stale-bearer")

    exchanges = []
    monkeypatch.setattr(
        nico_rest.oauth,
        "fetch_access_token",
        lambda credential, url, scope: (
            exchanges.append((credential, url, scope)) or "fresh-bearer"
        ),
    )

    assert nico_rest.refresh_token() is True
    assert exchanges == [
        ("client-id:client-secret", "https://issuer.example.test/token", "carbide")
    ]
    assert os.environ["NICO_TOKEN"] == "fresh-bearer"


def test_refresh_declines_when_no_credential_was_configured(monkeypatch):
    """A run given NICO_TOKEN directly has nothing to re-exchange with."""
    monkeypatch.setattr(nico_rest, "_token_refresh", None)

    def must_not_exchange(*_args, **_kwargs):
        raise AssertionError("refresh must not call the authorization server")

    monkeypatch.setattr(nico_rest.oauth, "fetch_access_token", must_not_exchange)

    assert nico_rest.refresh_token() is False


def test_configured_credential_stays_out_of_the_environment(monkeypatch):
    """Subprocesses inherit os.environ; the client secret must not be in it."""
    monkeypatch.setattr(nico_rest, "_token_refresh", None)

    nico_rest.configure_token_refresh(
        "client-id:client-secret", "https://issuer.example.test/token", "carbide"
    )

    assert "client-id:client-secret" not in repr(sorted(os.environ.items()))
    assert nico_rest._token_refresh[0] == "client-id:client-secret"


# ---------------------------------------------------------------------------
# NICO_BASE_URL policy
# ---------------------------------------------------------------------------
@pytest.mark.parametrize(
    "base_url",
    [
        "https://nico.example.test",
        "https://nico.example.test:8443",
        "http://localhost:8080",
        "http://127.0.0.1:8080",
        # Every endpoint in use today, including one on a non-default port.
        "http://carbide-rest-api.carbide-rest.svc.cluster.local",
        "http://nico-rest-api.carbide-rest.svc.cluster.local:8388",
        "http://carbide-rest-api.carbide-rest.svc",
        "http://carbide-rest-api.carbide-rest.svc.cluster.local.",
    ],
)
def test_base_url_accepts_https_loopback_and_in_cluster(monkeypatch, base_url):
    """https anywhere; http only where it cannot leave the host or cluster."""
    monkeypatch.setenv("NICO_BASE_URL", base_url)

    assert nico_rest.check_base_url() == base_url


@pytest.mark.parametrize(
    "base_url",
    [
        "http://nico.example.test",
        # The suffix is matched exactly: this one resolves on the internet.
        "http://carbide-rest-api.svc.example.com",
        "http://svc.example.com",
        "http://10.0.0.5:8080",
    ],
)
def test_base_url_rejects_cleartext_to_a_remote_host(monkeypatch, base_url):
    """The bearer rides every request, so it must not cross cleartext."""
    monkeypatch.setenv("NICO_BASE_URL", base_url)

    with pytest.raises(nico_rest.NicoError, match="must use https"):
        nico_rest.check_base_url()


def test_no_environment_variable_can_waive_the_rule(monkeypatch):
    """In-clusterness is a property of the URL, not something a job asserts.

    An attacker who can set NICO_BASE_URL can set anything else alongside it,
    so an opt-out flag would protect nobody.
    """
    monkeypatch.setenv("NICO_BASE_URL", "http://nico.example.test")
    for suspect in ("MLT_ALLOW_INSECURE_NICO_URL", "NICO_ALLOW_INSECURE"):
        monkeypatch.setenv(suspect, "true")

    with pytest.raises(nico_rest.NicoError, match="must use https"):
        nico_rest.check_base_url()


@pytest.mark.parametrize("base_url", ["nico.example.test", "", "/v2/org"])
def test_base_url_requires_a_host(monkeypatch, base_url):
    """A relative value names no destination to check in the first place."""
    monkeypatch.setenv("NICO_BASE_URL", base_url)

    with pytest.raises(nico_rest.NicoError, match="must include a host"):
        nico_rest.check_base_url()


def test_base_url_rejects_a_non_http_scheme(monkeypatch):
    """Only the two schemes the REST client actually speaks are allowed."""
    monkeypatch.setenv("NICO_BASE_URL", "ftp://nico.example.test")

    with pytest.raises(nico_rest.NicoError, match="must use https"):
        nico_rest.check_base_url()


def test_every_request_rechecks_the_base_url(monkeypatch, nico_env):
    """Preflight is not the only gate: the check sits on the request path."""
    monkeypatch.setenv("NICO_BASE_URL", "http://nico.example.test")

    with pytest.raises(nico_rest.NicoError, match="must use https"):
        nico_rest._endpoint_prefix()
