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

"""Build a per-run MLT operating system definition and SSH identity."""

import io
import json
import secrets
import uuid
from dataclasses import dataclass
from pathlib import Path

import bcrypt
import paramiko
import yaml
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

_TEMPLATE_DIR = Path(__file__).with_name("os_templates")
_IPXE_TEMPLATE = _TEMPLATE_DIR / "ubuntu-24.04-arm64.ipxe"
_USER_DATA_TEMPLATE = _TEMPLATE_DIR / "ubuntu-24.04-arm64-user-data.yaml"
_PUBLIC_KEY_PLACEHOLDER = "__MLT_SSH_PUBLIC_KEY__"
_ALLOW_PW_PLACEHOLDER = "__MLT_ALLOW_PW__"
_USER_PASSWORD_PLACEHOLDER = "__MLT_USER_PASSWORD__"
_LOCK_PASSWD_PLACEHOLDER = "__MLT_LOCK_PASSWD__"

TEMPORARY_OS_NAME_PREFIX = "mlt-os-"
TEMPORARY_OS_DESCRIPTION = "Ephemeral machine-lifecycle-test OS"

# No password hash matches '!', so the account cannot be logged into with a
# password. This is what the template carries unless the operator opts in to
# a console password.
_LOCKED_PASSWORD = "!"

# Deliberately excludes characters that are easy to misread when the password
# is being typed by hand at a serial console (0/O, 1/l/I).
_PASSWORD_ALPHABET = "abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789"
_PASSWORD_LENGTH = 20
_BCRYPT_ROUNDS = 12


@dataclass(frozen=True)
class EphemeralOperatingSystem:
    """The transient OS definition and its in-memory SSH private key."""

    name: str
    ipxe_script: str
    user_data: str
    ssh_private_key: paramiko.PKey
    console_password: str | None = None


def _hash_password(password: str) -> str:
    """Return a bcrypt hash suitable for /etc/shadow.

    bcrypt rather than the stdlib ``crypt``: ``crypt`` is deprecated (PEP 594)
    and removed in 3.13, and on macOS it offers only ``METHOD_CRYPT``, where
    asking for SHA-512 silently yields a 13-character DES hash instead of
    raising. A break-glass credential that is quietly wrong on the maintainer's
    laptop is worse than one that does not build. libxcrypt on Ubuntu 24.04
    accepts the ``$2b$`` prefix this produces.
    """
    return bcrypt.hashpw(password.encode(), bcrypt.gensalt(rounds=_BCRYPT_ROUNDS)).decode()


def _render_authorized_keys(template: str, public_keys: list[str]) -> str:
    """Expand the single key placeholder into one YAML list entry per key."""
    placeholder_count = template.count(_PUBLIC_KEY_PLACEHOLDER)
    if placeholder_count != 1:
        raise ValueError(
            f"{_USER_DATA_TEMPLATE} must contain exactly one " +
            f"{_PUBLIC_KEY_PLACEHOLDER} placeholder; found {placeholder_count}"
        )

    placeholder_line = next(
        line for line in template.splitlines() if _PUBLIC_KEY_PLACEHOLDER in line
    )
    indent = placeholder_line[: len(placeholder_line) - len(placeholder_line.lstrip())]
    # Quote each key. An operator-supplied key carries a free-form comment, and
    # a bare " #" in it starts a YAML comment: the key is silently truncated
    # there, still parses, and still counts as one entry, so the validation
    # below cannot see it. A JSON string is a valid YAML double-quoted scalar.
    rendered = "\n".join(
        f"{indent}- {json.dumps(public_key)}" for public_key in public_keys
    )
    return template.replace(placeholder_line, rendered)


def _validate_rendered_user_data(
    user_data: str, *, expected_keys: int, expect_password_login: bool
) -> None:
    """Confirm the rendered document is the cloud-init config we intended.

    Rendering splices indented list entries and injects a bcrypt hash full of
    '$' and '/', so a template edit can produce something that parses as YAML
    but means the wrong thing -- or does not parse at all. Either way the
    failure would otherwise surface as a provisioning timeout an hour later,
    with nothing pointing back at this file.
    """
    try:
        document = yaml.safe_load(user_data)
    except yaml.YAMLError as error:
        raise ValueError(
            f"{_USER_DATA_TEMPLATE} rendered to invalid YAML: {error}"
        ) from error

    try:
        allow_pw = document["ssh_pwauth"]
        users = document["users"]
        test_user = next(
            user
            for user in users
            if user.get("name") == "machine-lifecycle-test-user"
        )
        nvidia_user = next(
            user for user in users if user.get("name") == "nvidia"
        )
        password = test_user["passwd"]
        locked = test_user["lock_passwd"]
        authorized = test_user["ssh_authorized_keys"]
        nvidia_locked = nvidia_user["lock_passwd"]
    except (AttributeError, KeyError, StopIteration, TypeError) as error:
        raise ValueError(
            f"{_USER_DATA_TEMPLATE} rendered without the expected cloud-init "
            f"structure: {error}"
        ) from error

    # A quoted 'false' would be truthy to cloud-init; assert the real boolean.
    if allow_pw is not expect_password_login:
        raise ValueError(
            f"rendered ssh_pwauth is {allow_pw!r}, expected {expect_password_login!r}"
        )
    if nvidia_locked is not True:
        raise ValueError(
            f"rendered nvidia lock_passwd is {nvidia_locked!r}, expected True"
        )
    if not isinstance(password, str) or not password:
        raise ValueError(f"rendered user password is not a string: {password!r}")
    # Cloud-init locks the account regardless of the hash when this is true,
    # so a mismatch here would silently defeat the console password.
    if locked is expect_password_login:
        raise ValueError(
            f"rendered lock_passwd is {locked!r} with password login "
            f"{'enabled' if expect_password_login else 'disabled'}"
        )
    if not isinstance(authorized, list) or len(authorized) != expected_keys:
        raise ValueError(
            f"rendered {len(authorized) if isinstance(authorized, list) else authorized} "
            f"authorized key(s), expected {expected_keys}"
        )


def _require_placeholder(template: str, placeholder: str) -> None:
    count = template.count(placeholder)
    if count != 1:
        raise ValueError(
            f"{_USER_DATA_TEMPLATE} must contain exactly one {placeholder} "
            f"placeholder; found {count}"
        )


def build_ephemeral_operating_system(
    *,
    debug_public_key: str | None = None,
    enable_console_password: bool = False,
) -> EphemeralOperatingSystem:
    """Generate an Ed25519 identity and render it into the cloud-init template.

    The run's own private key is converted directly in memory to Paramiko's key
    type. It is never written to disk or placed in an environment variable.

    ``debug_public_key`` is an optional operator-supplied public key added
    alongside it, so a wedged instance stays reachable after the run that
    created it has exited. It is a public key, so there is nothing secret to
    store in the repository or hand to the operator.

    ``enable_console_password`` mints a random password for
    ``machine-lifecycle-test-user`` -- the same account the test logs in as --
    and returns the plaintext for the caller to publish. It covers the case
    where the box finished installing but is not reachable over SSH, leaving
    the BMC serial console as the only way in. Note that the account carries
    passwordless sudo, so this credential is root-equivalent.
    """
    private_key = Ed25519PrivateKey.generate()
    public_key = (
        private_key.public_key()
        .public_bytes(
            encoding=serialization.Encoding.OpenSSH,
            format=serialization.PublicFormat.OpenSSH,
        )
        .decode()
    )
    public_key = f"{public_key} machine-lifecycle-test"

    private_key_text = private_key.private_bytes(
        encoding=serialization.Encoding.PEM,
        format=serialization.PrivateFormat.OpenSSH,
        encryption_algorithm=serialization.NoEncryption(),
    ).decode()
    with io.StringIO(private_key_text) as private_key_file:
        paramiko_key = paramiko.Ed25519Key.from_private_key(private_key_file)

    authorized_keys = [public_key]
    if debug_public_key:
        authorized_keys.append(debug_public_key.strip())

    console_password: str | None = None
    user_password = _LOCKED_PASSWORD
    if enable_console_password:
        console_password = "".join(
            secrets.choice(_PASSWORD_ALPHABET) for _ in range(_PASSWORD_LENGTH)
        )
        user_password = _hash_password(console_password)

    user_data = _USER_DATA_TEMPLATE.read_text(encoding="utf-8")
    _require_placeholder(user_data, _ALLOW_PW_PLACEHOLDER)
    _require_placeholder(user_data, _USER_PASSWORD_PLACEHOLDER)
    _require_placeholder(user_data, _LOCK_PASSWD_PLACEHOLDER)
    user_data = _render_authorized_keys(user_data, authorized_keys)
    # Single-quoted so the leading '$' of a crypt hash, and the bare '!' of a
    # locked account, are read by YAML as literal scalars rather than as an
    # alias or a tag indicator.
    user_data = user_data.replace(
        _USER_PASSWORD_PLACEHOLDER, f"'{user_password}'"
    )
    user_data = user_data.replace(
        _LOCK_PASSWD_PLACEHOLDER, "false" if enable_console_password else "true"
    )
    user_data = user_data.replace(
        _ALLOW_PW_PLACEHOLDER, "true" if enable_console_password else "false"
    )
    _validate_rendered_user_data(
        user_data,
        expected_keys=len(authorized_keys),
        expect_password_login=enable_console_password,
    )

    return EphemeralOperatingSystem(
        name=f"{TEMPORARY_OS_NAME_PREFIX}{uuid.uuid4().hex[:12]}",
        ipxe_script=_IPXE_TEMPLATE.read_text(encoding="utf-8"),
        user_data=user_data,
        ssh_private_key=paramiko_key,
        console_password=console_password,
    )
