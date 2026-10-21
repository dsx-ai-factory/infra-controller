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

"""Exchange OAuth client credentials for a NICo bearer token.

The authorization server and scope come from configuration; nothing here
assumes a particular provider.

Grant parameters go on the query string rather than in the form body RFC 6749
describes. That is what these deployments accept, so it is left alone -- but an
arbitrary compliant server may need the body form.
"""

from __future__ import annotations

import base64
import json
import subprocess
from urllib.parse import urlencode, urlsplit, urlunsplit

REQUEST_TIMEOUT_SECONDS = 10


def token_request_url(token_url: str, scope: str) -> str:
    """Add the grant parameters to a token endpoint, keeping any it carries."""
    parts = urlsplit(token_url)
    grant = urlencode({"grant_type": "client_credentials", "scope": scope})
    query = f"{parts.query}&{grant}" if parts.query else grant
    return urlunsplit(parts._replace(query=query))


def _redactor(*secrets: str):
    """Return a function that replaces *secrets* with a redaction marker.

    Base64 is not encryption, so the encoded credential is as sensitive as the
    pair itself and neither may reach an exception message.
    """

    material = [secret for secret in secrets if secret]

    def redact(text: str) -> str:
        for secret in material:
            text = text.replace(secret, "<redacted>")
        return text

    return redact


def fetch_access_token(credential: str, token_url: str, scope: str) -> str:
    """Exchange a ``client_id:client_secret`` credential for an access token.

    ``curl`` rather than ``requests`` so the exchange works in minimal images
    with no Python TLS trust store, with timeouts at both the curl and
    subprocess layers.

    :raises RuntimeError: on timeout, non-zero curl exit, a non-2xx response, a
      non-JSON body, or a response carrying no ``access_token``. No error
      raised here contains credential material.
    """
    # A credential read from a secret store can carry a trailing newline, which
    # would corrupt the Basic-auth header.
    credential = credential.strip()
    encoded = base64.b64encode(credential.encode()).decode()
    # The pair first, so it is replaced whole rather than leaving half of it
    # beside a marker. The secret alone matters because a server can echo the
    # decoded field back rather than the header it arrived in.
    _client_id, _, client_secret = credential.partition(":")
    redact = _redactor(credential, encoded, client_secret)
    url = token_request_url(token_url, scope)

    # The header goes to curl on stdin, never in argv: subprocess.TimeoutExpired
    # renders the whole command in its string form, which would put the
    # credential in a job log. It also keeps it out of `ps`.
    curl_config = f'header = "Authorization: Basic {encoded}"\n'
    try:
        proc = subprocess.run(
            [
                # No -f: we want the error body on a 4xx (e.g.
                # {"error":"invalid_client"}) to diagnose a failure. -w appends
                # the status as a trailing line.
                "curl", "-s",
                "-w", "\n%{http_code}",
                "--max-time", str(REQUEST_TIMEOUT_SECONDS),
                "-H", "Content-Type:application/x-www-form-urlencoded",
                "-K", "-",
                "-X", "POST",
                url,
            ],
            input=curl_config,
            capture_output=True,
            text=True,
            timeout=REQUEST_TIMEOUT_SECONDS,
        )
    except FileNotFoundError as e:
        raise RuntimeError("Token request needs curl, which is not on PATH") from e
    except subprocess.TimeoutExpired as e:
        raise RuntimeError(
            f"Token request timed out after {REQUEST_TIMEOUT_SECONDS}s calling {url}"
        ) from e

    if proc.returncode != 0:
        raise RuntimeError(
            f"Token request curl failed (rc={proc.returncode}): "
            f"{redact(proc.stderr.strip())}"
        )

    # stdout is "<body>\n<http_code>" (status appended by -w).
    body, _, http_code = proc.stdout.rpartition("\n")
    http_code = http_code.strip()
    if http_code != "200":
        # An intermediary can echo request detail into an error body.
        raise RuntimeError(
            f"Token request returned HTTP {http_code} from {url}: "
            f"{redact(body.strip())[:500]}"
        )

    try:
        data = json.loads(body)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"Token request returned a non-JSON response: {e}; "
            f"body={redact(body.strip())[:500]!r}"
        ) from e

    token = data.get("access_token")
    if not token:
        # Even key names are redacted: they are attacker- or server-chosen.
        raise RuntimeError(
            f"No access_token in response: {redact(str(sorted(data)))}"
        )
    print("NICo bearer token obtained")
    return token
