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

import bcrypt
import pytest
import yaml

from lib.ephemeral_os import build_ephemeral_operating_system

_IPXE = "#!ipxe\necho ${cloudinit-url}\n"
_USER_DATA = """#cloud-config
users:
  - default
  - name: portable-test-user
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - __MLT_SSH_PUBLIC_KEY__
"""
_PASSWORD_USER_DATA = """#cloud-config
ssh_pwauth: __MLT_ALLOW_PW__
users:
  - name: portable-test-user
    passwd: __MLT_USER_PASSWORD__
    lock_passwd: __MLT_LOCK_PASSWD__
    ssh_authorized_keys:
      - __MLT_SSH_PUBLIC_KEY__
"""
_DEBUG_KEY = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB9V6E0 operator@example"


def _inputs(tmp_path, *, ipxe=_IPXE, user_data=_USER_DATA):
    ipxe_path = tmp_path / "boot.ipxe"
    user_data_path = tmp_path / "user-data.yaml"
    ipxe_path.write_text(ipxe, encoding="utf-8")
    user_data_path.write_text(user_data, encoding="utf-8")
    return ipxe_path, user_data_path


def _build(tmp_path, **kwargs):
    return build_ephemeral_operating_system(*_inputs(tmp_path), **kwargs)


def _ssh_user(operating_system):
    document = yaml.safe_load(operating_system.user_data)
    return next(
        user
        for user in document["users"]
        if isinstance(user, dict)
        and operating_system.ssh_private_key.asbytes()
        == base64.b64decode(user["ssh_authorized_keys"][0].split()[1])
    )


def test_embeds_a_new_per_run_ed25519_key_and_infers_its_username(tmp_path):
    first = _build(tmp_path)
    second = _build(tmp_path)

    assert first.name.startswith("mlt-os-")
    assert first.name != second.name
    assert first.ssh_private_key.asbytes() != second.ssh_private_key.asbytes()
    assert first.ssh_username == "portable-test-user"
    assert _ssh_user(first)["name"] == first.ssh_username
    assert "__MLT_SSH_PUBLIC_KEY__" not in first.user_data
    assert first.ipxe_script == _IPXE


def test_adds_the_debug_public_key_without_changing_the_inferred_user(tmp_path):
    awkward_key = f'{_DEBUG_KEY} #2 spare: "primary"'
    operating_system = _build(tmp_path, debug_public_key=awkward_key)

    assert operating_system.ssh_username == "portable-test-user"
    assert _ssh_user(operating_system)["ssh_authorized_keys"][1] == awkward_key


@pytest.mark.parametrize("which", ["ipxe", "user-data"])
def test_rejects_missing_inputs(which, tmp_path):
    ipxe_path, user_data_path = _inputs(tmp_path)
    missing = tmp_path / "missing"

    with pytest.raises(ValueError, match="Could not read"):
        build_ephemeral_operating_system(
            missing if which == "ipxe" else ipxe_path,
            missing if which == "user-data" else user_data_path,
        )


@pytest.mark.parametrize("which", ["ipxe", "user-data"])
def test_rejects_unreadable_inputs(which, tmp_path):
    ipxe_path, user_data_path = _inputs(tmp_path)
    unreadable = tmp_path / "directory"
    unreadable.mkdir()

    with pytest.raises(ValueError, match="Could not read"):
        build_ephemeral_operating_system(
            unreadable if which == "ipxe" else ipxe_path,
            unreadable if which == "user-data" else user_data_path,
        )


@pytest.mark.parametrize("which", ["ipxe", "user-data"])
def test_rejects_empty_inputs(which, tmp_path):
    ipxe_path, user_data_path = _inputs(tmp_path)
    (ipxe_path if which == "ipxe" else user_data_path).write_text(
        " \n", encoding="utf-8"
    )

    with pytest.raises(ValueError, match="must not be empty"):
        build_ephemeral_operating_system(ipxe_path, user_data_path)


@pytest.mark.parametrize("which", ["ipxe", "user-data"])
def test_rejects_non_utf8_inputs(which, tmp_path):
    ipxe_path, user_data_path = _inputs(tmp_path)
    (ipxe_path if which == "ipxe" else user_data_path).write_bytes(b"\xff\xfe")

    with pytest.raises(ValueError, match="not valid UTF-8"):
        build_ephemeral_operating_system(ipxe_path, user_data_path)


@pytest.mark.parametrize(
    ("ipxe", "message"),
    [
        ("echo ${cloudinit-url}\n", "start with #!ipxe"),
        ("#!ipxe\necho no-cloud-init\n", r"reference \$\{cloudinit-url\}"),
    ],
)
def test_rejects_invalid_ipxe_contract(ipxe, message, tmp_path):
    paths = _inputs(tmp_path, ipxe=ipxe)

    with pytest.raises(ValueError, match=message):
        build_ephemeral_operating_system(*paths)


@pytest.mark.parametrize(
    ("user_data", "message"),
    [
        ("#cloud-config\nusers: [\n", "not valid YAML"),
        ("users: []\n", "start with #cloud-config"),
        ("#cloud-config\nusers: []\n", "exactly one"),
        (
            "#cloud-config\nnote: __MLT_SSH_PUBLIC_KEY__\nusers: []\n",
            "concrete cloud-init user",
        ),
        (
            "#cloud-config\nusers:\n  - ssh_authorized_keys:\n"
            "      - __MLT_SSH_PUBLIC_KEY__\n",
            "non-empty name",
        ),
        (
            _USER_DATA.replace(
                "      - __MLT_SSH_PUBLIC_KEY__",
                "      - __MLT_SSH_PUBLIC_KEY__\n      - __MLT_SSH_PUBLIC_KEY__",
            ),
            "exactly one",
        ),
    ],
)
def test_rejects_invalid_cloud_init_and_key_placeholder_contract(
    user_data, message, tmp_path
):
    paths = _inputs(tmp_path, user_data=user_data)

    with pytest.raises(ValueError, match=message):
        build_ephemeral_operating_system(*paths)


@pytest.mark.parametrize("name", [" portable-test-user ", "portable test user"])
def test_rejects_ssh_username_with_whitespace(tmp_path, name):
    paths = _inputs(
        tmp_path,
        user_data=_USER_DATA.replace(
            "name: portable-test-user", f'name: "{name}"'
        ),
    )

    with pytest.raises(ValueError, match="must not contain whitespace"):
        build_ephemeral_operating_system(*paths)


def test_yaml_errors_do_not_expose_template_contents(tmp_path):
    secret_marker = "template-content-must-not-be-logged"
    paths = _inputs(
        tmp_path,
        user_data=(
            "#cloud-config\nusers:\n"
            f"  - name: {secret_marker}\n"
            "    broken: [\n"
            "    ssh_authorized_keys: [__MLT_SSH_PUBLIC_KEY__]\n"
        ),
    )

    with pytest.raises(ValueError) as raised:
        build_ephemeral_operating_system(*paths)

    assert secret_marker not in str(raised.value)


def test_password_placeholders_may_all_be_absent_for_normal_ssh(tmp_path):
    operating_system = _build(tmp_path)

    assert operating_system.console_password is None
    assert operating_system.ssh_username == "portable-test-user"


@pytest.mark.parametrize(
    "user_data",
    [
        _USER_DATA + "ssh_pwauth: __MLT_ALLOW_PW__\n",
        _PASSWORD_USER_DATA.replace("    passwd: __MLT_USER_PASSWORD__\n", ""),
        _PASSWORD_USER_DATA.replace("    lock_passwd: __MLT_LOCK_PASSWD__\n", ""),
    ],
)
def test_rejects_partial_password_placeholder_groups(user_data, tmp_path):
    paths = _inputs(tmp_path, user_data=user_data)

    with pytest.raises(ValueError, match="all-or-none"):
        build_ephemeral_operating_system(*paths)


def test_console_password_requires_the_optional_placeholder_group(tmp_path):
    with pytest.raises(ValueError, match="enable_console_password=true requires"):
        _build(tmp_path, enable_console_password=True)


def test_password_group_preserves_secure_defaults(tmp_path):
    operating_system = build_ephemeral_operating_system(
        *_inputs(tmp_path, user_data=_PASSWORD_USER_DATA)
    )
    document = yaml.safe_load(operating_system.user_data)
    user = _ssh_user(operating_system)

    assert operating_system.console_password is None
    assert document["ssh_pwauth"] is False
    assert user["passwd"] == "!"
    assert user["lock_passwd"] is True


def test_console_password_is_rendered_for_the_inferred_ssh_user(tmp_path):
    operating_system = build_ephemeral_operating_system(
        *_inputs(tmp_path, user_data=_PASSWORD_USER_DATA),
        enable_console_password=True,
    )
    document = yaml.safe_load(operating_system.user_data)
    user = _ssh_user(operating_system)

    assert operating_system.console_password is not None
    assert document["ssh_pwauth"] is True
    assert user["lock_passwd"] is False
    assert bcrypt.checkpw(
        operating_system.console_password.encode(), user["passwd"].encode()
    )
    assert operating_system.console_password not in operating_system.user_data


@pytest.mark.parametrize(
    ("old", "new", "message"),
    [
        (
            "ssh_pwauth: __MLT_ALLOW_PW__",
            "note: __MLT_ALLOW_PW__",
            "top-level ssh_pwauth",
        ),
        (
            "passwd: __MLT_USER_PASSWORD__",
            "note: __MLT_USER_PASSWORD__",
            "must be passwd",
        ),
        (
            "lock_passwd: __MLT_LOCK_PASSWD__",
            "other: __MLT_LOCK_PASSWD__",
            "must be lock_passwd",
        ),
    ],
)
def test_password_group_must_control_the_inferred_user(old, new, message, tmp_path):
    paths = _inputs(tmp_path, user_data=_PASSWORD_USER_DATA.replace(old, new))

    with pytest.raises(ValueError, match=message):
        build_ephemeral_operating_system(*paths)
