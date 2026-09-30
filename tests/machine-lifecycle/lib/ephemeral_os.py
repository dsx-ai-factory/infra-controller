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

"""Build a per-run OS definition from operator-supplied textual templates."""

from __future__ import annotations

import io
import secrets
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import bcrypt
import paramiko
import yaml
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey

_PUBLIC_KEY_PLACEHOLDER = "__MLT_SSH_PUBLIC_KEY__"
_ALLOW_PW_PLACEHOLDER = "__MLT_ALLOW_PW__"
_USER_PASSWORD_PLACEHOLDER = "__MLT_USER_PASSWORD__"
_LOCK_PASSWD_PLACEHOLDER = "__MLT_LOCK_PASSWD__"
_PASSWORD_PLACEHOLDERS = (
    _ALLOW_PW_PLACEHOLDER,
    _USER_PASSWORD_PLACEHOLDER,
    _LOCK_PASSWD_PLACEHOLDER,
)

TEMPORARY_OS_NAME_PREFIX = "mlt-os-"
TEMPORARY_OS_DESCRIPTION = "Ephemeral machine-lifecycle-test OS"

# No password hash matches '!', so the account cannot be logged into with a
# password when an external template opts in to MLT's password controls.
_LOCKED_PASSWORD = "!"

# Deliberately excludes characters that are easy to misread when the password
# is being typed by hand at a serial console (0/O, 1/l/I).
_PASSWORD_ALPHABET = "abcdefghjkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789"
_PASSWORD_LENGTH = 20
_BCRYPT_ROUNDS = 12


@dataclass(frozen=True)
class EphemeralOperatingSystem:
    """The transient OS definition and its in-memory SSH identity."""

    name: str
    ipxe_script: str
    user_data: str
    ssh_private_key: paramiko.PKey
    ssh_username: str
    console_password: str | None = None


def _hash_password(password: str) -> str:
    """Return a bcrypt hash suitable for /etc/shadow."""

    return bcrypt.hashpw(password.encode(), bcrypt.gensalt(rounds=_BCRYPT_ROUNDS)).decode()


def _read_text(path: Path, description: str) -> str:
    """Read one required non-empty UTF-8 text input without exposing its contents."""

    try:
        text = path.read_text(encoding="utf-8")
    except UnicodeError as error:
        raise ValueError(f"{description} {path} is not valid UTF-8") from error
    except OSError as error:
        raise ValueError(f"Could not read {description} {path}: {error}") from error
    if not text.strip():
        raise ValueError(f"{description} {path} must not be empty")
    return text


def _yaml_error_location(error: yaml.YAMLError) -> str:
    """Describe a YAML failure without copying input text into logs."""

    problem = getattr(error, "problem", None) or type(error).__name__
    mark = getattr(error, "problem_mark", None)
    location = ""
    if mark is not None:
        location = f" at line {mark.line + 1}, column {mark.column + 1}"
    return f"{problem}{location}"


def _parse_user_data(
    template: str, path: Path
) -> tuple[dict[str, Any], dict[str, Any], bool]:
    """Validate cloud-init structure and locate the key-bearing concrete user."""

    if template.lstrip().splitlines()[0].strip() != "#cloud-config":
        raise ValueError(f"User-data template {path} must start with #cloud-config")
    try:
        document = yaml.safe_load(template)
    except yaml.YAMLError as error:
        raise ValueError(
            f"User-data template {path} is not valid YAML: "
            f"{_yaml_error_location(error)}"
        ) from None
    if not isinstance(document, dict):
        raise ValueError(f"User-data template {path} must contain a YAML mapping")

    placeholder_count = template.count(_PUBLIC_KEY_PLACEHOLDER)
    if placeholder_count != 1:
        raise ValueError(
            f"User-data template {path} must contain exactly one "
            f"{_PUBLIC_KEY_PLACEHOLDER} placeholder; found {placeholder_count}"
        )

    users = document.get("users")
    if not isinstance(users, list):
        raise ValueError(
            f"User-data template {path} must define cloud-init users as a list"
        )
    matching_users: list[dict[str, Any]] = []
    for user in users:
        if not isinstance(user, dict):
            continue
        authorized_keys = user.get("ssh_authorized_keys")
        if isinstance(authorized_keys, list) and _PUBLIC_KEY_PLACEHOLDER in authorized_keys:
            matching_users.append(user)
    if len(matching_users) != 1:
        raise ValueError(
            f"{_PUBLIC_KEY_PLACEHOLDER} in {path} must be an ssh_authorized_keys "
            "entry on exactly one concrete cloud-init user"
        )

    ssh_user = matching_users[0]
    name = ssh_user.get("name")
    if not isinstance(name, str) or not name.strip():
        raise ValueError(
            f"The cloud-init user containing {_PUBLIC_KEY_PLACEHOLDER} in {path} "
            "must have a non-empty name"
        )
    if any(character.isspace() for character in name):
        raise ValueError(
            f"The cloud-init user containing {_PUBLIC_KEY_PLACEHOLDER} in {path} "
            "must not contain whitespace in its name"
        )

    counts = {
        placeholder: template.count(placeholder)
        for placeholder in _PASSWORD_PLACEHOLDERS
    }
    password_group_present = any(counts.values())
    if password_group_present and any(count != 1 for count in counts.values()):
        rendered_counts = ", ".join(
            f"{placeholder}={count}" for placeholder, count in counts.items()
        )
        raise ValueError(
            "Console-password placeholders are an all-or-none group with exactly "
            f"one occurrence each; found {rendered_counts}"
        )
    if password_group_present:
        if document.get("ssh_pwauth") != _ALLOW_PW_PLACEHOLDER:
            raise ValueError(
                f"{_ALLOW_PW_PLACEHOLDER} must be the top-level ssh_pwauth value"
            )
        if ssh_user.get("passwd") != _USER_PASSWORD_PLACEHOLDER:
            raise ValueError(
                f"{_USER_PASSWORD_PLACEHOLDER} must be passwd on the inferred SSH user"
            )
        if ssh_user.get("lock_passwd") != _LOCK_PASSWD_PLACEHOLDER:
            raise ValueError(
                f"{_LOCK_PASSWD_PLACEHOLDER} must be lock_passwd on the inferred SSH user"
            )
    return document, ssh_user, password_group_present


def _render_user_data(
    template: str,
    path: Path,
    public_keys: list[str],
    *,
    enable_console_password: bool,
) -> tuple[str, str, str | None]:
    """Validate and render cloud-init, returning text, SSH user, and password."""

    document, ssh_user, password_group_present = _parse_user_data(template, path)
    if enable_console_password and not password_group_present:
        raise ValueError(
            "debug.enable_console_password=true requires all three console-password "
            f"placeholders in {path}"
        )

    authorized_keys = ssh_user["ssh_authorized_keys"]
    placeholder_index = authorized_keys.index(_PUBLIC_KEY_PLACEHOLDER)
    authorized_keys[placeholder_index : placeholder_index + 1] = public_keys

    console_password: str | None = None
    if password_group_present:
        user_password = _LOCKED_PASSWORD
        if enable_console_password:
            console_password = "".join(
                secrets.choice(_PASSWORD_ALPHABET) for _ in range(_PASSWORD_LENGTH)
            )
            user_password = _hash_password(console_password)
        document["ssh_pwauth"] = enable_console_password
        ssh_user["passwd"] = user_password
        ssh_user["lock_passwd"] = not enable_console_password

    rendered = "#cloud-config\n" + yaml.safe_dump(
        document,
        allow_unicode=True,
        sort_keys=False,
    )
    return rendered, ssh_user["name"], console_password


def build_ephemeral_operating_system(
    ipxe_script_path: str | Path,
    user_data_template_path: str | Path,
    *,
    debug_public_key: str | None = None,
    enable_console_password: bool = False,
) -> EphemeralOperatingSystem:
    """Read external templates, generate an Ed25519 identity, and render cloud-init."""

    ipxe_path = Path(ipxe_script_path)
    user_data_path = Path(user_data_template_path)
    ipxe_script = _read_text(ipxe_path, "iPXE script")
    if not ipxe_script.startswith("#!ipxe"):
        raise ValueError(f"iPXE script {ipxe_path} must start with #!ipxe")
    if "${cloudinit-url}" not in ipxe_script:
        raise ValueError(f"iPXE script {ipxe_path} must reference ${{cloudinit-url}}")
    user_data_template = _read_text(user_data_path, "user-data template")

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

    public_keys = [public_key]
    if debug_public_key and debug_public_key.strip():
        public_keys.append(debug_public_key.strip())

    user_data, ssh_username, console_password = _render_user_data(
        user_data_template,
        user_data_path,
        public_keys,
        enable_console_password=enable_console_password,
    )

    return EphemeralOperatingSystem(
        name=f"{TEMPORARY_OS_NAME_PREFIX}{uuid.uuid4().hex[:12]}",
        ipxe_script=ipxe_script,
        user_data=user_data,
        ssh_private_key=paramiko_key,
        ssh_username=ssh_username,
        console_password=console_password,
    )
