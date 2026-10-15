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

import base64
import subprocess
import traceback
from types import SimpleNamespace

import pytest

from lib import oauth
from lib.oauth import token_request_url


def test_grant_parameters_are_added_to_a_bare_endpoint():
    url = token_request_url("https://issuer.example.test/token", "carbide")

    assert url == (
        "https://issuer.example.test/token"
        "?grant_type=client_credentials&scope=carbide"
    )


def test_an_endpoint_that_already_has_a_query_string_keeps_it():
    url = token_request_url("https://issuer.example.test/token?realm=one", "carbide")

    assert url == (
        "https://issuer.example.test/token"
        "?realm=one&grant_type=client_credentials&scope=carbide"
    )


@pytest.mark.parametrize("scope", ["two scopes", "scope/with-slash", "a&b"])
def test_a_scope_needing_escaping_is_encoded(scope):
    url = token_request_url("https://issuer.example.test/token", scope)

    # The scope must not escape its own parameter.
    assert url.count("grant_type=client_credentials") == 1
    assert " " not in url


def test_the_credential_never_reaches_the_argument_list(monkeypatch):
    """Arguments show up in `ps` and in TimeoutExpired's string form."""
    captured = {}

    def capture(args, **kwargs):
        captured["args"] = args
        captured["input"] = kwargs.get("input")
        return SimpleNamespace(
            returncode=0, stdout='{"access_token": "bearer"}\n200', stderr=""
        )

    monkeypatch.setattr(oauth.subprocess, "run", capture)

    oauth.fetch_access_token(
        "client-id:super-secret", "https://issuer.example.test/token", "carbide"
    )

    rendered = " ".join(captured["args"])
    encoded = base64.b64encode(b"client-id:super-secret").decode()
    assert "super-secret" not in rendered
    assert encoded not in rendered
    # It reaches curl as a config file on stdin instead.
    assert encoded in captured["input"]


def test_a_timeout_does_not_put_the_credential_in_the_traceback(monkeypatch):
    encoded = base64.b64encode(b"client-id:super-secret").decode()

    def time_out(args, **kwargs):
        # The real exception carries the command it was given.
        raise subprocess.TimeoutExpired(cmd=args, timeout=10)

    monkeypatch.setattr(oauth.subprocess, "run", time_out)

    with pytest.raises(RuntimeError):
        try:
            oauth.fetch_access_token(
                "client-id:super-secret", "https://issuer.example.test/token", "carbide"
            )
        except RuntimeError:
            rendered = traceback.format_exc()
            assert "super-secret" not in rendered
            assert encoded not in rendered
            raise


def test_an_error_response_echoing_the_header_is_redacted(monkeypatch):
    encoded = base64.b64encode(b"client-id:super-secret").decode()

    def echo_request(args, **kwargs):
        # An intermediary reflecting request detail back in its error body.
        return SimpleNamespace(
            returncode=0,
            stdout=f'{{"error":"bad request","seen":"Basic {encoded}"}}\n400',
            stderr="",
        )

    monkeypatch.setattr(oauth.subprocess, "run", echo_request)

    with pytest.raises(RuntimeError) as error:
        oauth.fetch_access_token(
            "client-id:super-secret", "https://issuer.example.test/token", "carbide"
        )

    assert encoded not in str(error.value)
    assert "<redacted>" in str(error.value)


def test_an_error_body_echoing_the_decoded_secret_is_redacted(monkeypatch):
    """A server can echo the field it parsed, not the header it arrived in."""
    def echo_decoded(args, **kwargs):
        return SimpleNamespace(
            returncode=0,
            stdout='{"error":"invalid_client","client_secret":"super-secret"}\n401',
            stderr="",
        )

    monkeypatch.setattr(oauth.subprocess, "run", echo_decoded)

    with pytest.raises(RuntimeError) as error:
        oauth.fetch_access_token(
            "client-id:super-secret", "https://issuer.example.test/token", "carbide"
        )

    assert "super-secret" not in str(error.value)
    assert "<redacted>" in str(error.value)


def test_response_keys_are_redacted_when_no_token_comes_back(monkeypatch):
    """Key names are server-chosen and can carry the credential."""
    encoded = base64.b64encode(b"client-id:super-secret").decode()

    def key_carries_credential(args, **kwargs):
        return SimpleNamespace(
            returncode=0, stdout='{"%s": "x"}\n200' % encoded, stderr=""
        )

    monkeypatch.setattr(oauth.subprocess, "run", key_carries_credential)

    with pytest.raises(RuntimeError) as error:
        oauth.fetch_access_token(
            "client-id:super-secret", "https://issuer.example.test/token", "carbide"
        )

    assert encoded not in str(error.value)


def test_absent_curl_raises_the_documented_error(monkeypatch):
    def not_installed(args, **kwargs):
        raise FileNotFoundError(2, "No such file or directory", "curl")

    monkeypatch.setattr(oauth.subprocess, "run", not_installed)

    with pytest.raises(RuntimeError, match="needs curl"):
        oauth.fetch_access_token(
            "client-id:secret", "https://issuer.example.test/token", "carbide"
        )
