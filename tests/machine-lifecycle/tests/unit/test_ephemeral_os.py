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
import re

import bcrypt
import pytest
import yaml

from lib import ephemeral_os
from lib.ephemeral_os import build_ephemeral_operating_system

_DEBUG_KEY = (
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIB9V6E0000000000000000000000000000000000000 "
    "operator@example"
)


def _authorized_key_lines(user_data: str) -> list[str]:
    """Return the authorized keys cloud-init will actually see.

    Read them out of the parsed document rather than by matching raw lines:
    the keys are YAML-quoted, and a scrape would silently return nothing --
    or, worse, a truncated key -- instead of failing.
    """

    def _find(node: object) -> list[str] | None:
        if isinstance(node, dict):
            for key, value in node.items():
                if key == "ssh_authorized_keys" and isinstance(value, list):
                    return value
                if (found := _find(value)) is not None:
                    return found
        elif isinstance(node, list):
            for item in node:
                if (found := _find(item)) is not None:
                    return found
        return None

    return _find(yaml.safe_load(user_data)) or []


def _authorized_key_blob(user_data: str) -> bytes:
    lines = _authorized_key_lines(user_data)
    assert lines, "no ssh-ed25519 authorized-key line found in user_data"
    return base64.b64decode(lines[0].split()[1], validate=True)


def test_ephemeral_os_embeds_the_public_half_of_its_private_key():
    operating_system = build_ephemeral_operating_system()

    assert operating_system.name.startswith("mlt-os-")
    assert "__MLT_SSH_PUBLIC_KEY__" not in operating_system.user_data
    assert (
        _authorized_key_blob(operating_system.user_data)
        == operating_system.ssh_private_key.asbytes()
    )
    assert "disable_root: true" in operating_system.user_data
    assert "phone_home:" not in operating_system.user_data
    assert operating_system.ipxe_script.startswith("#!ipxe\n")
    # The semicolon must reach the kernel command line unescaped. A backslash
    # here survives into /proc/cmdline; cloud-init then reads the datasource
    # name as "nocloud-net\" and never parses s=<url>.
    assert "ds=nocloud-net;s=${cloudinit-url}" in operating_system.ipxe_script
    assert "\\;" not in operating_system.ipxe_script
    assert "qcow-imager.efi" in operating_system.ipxe_script
    assert (
        "nke-24.04-aarch64-lvm-2026-06-10-950.qcow2"
        in operating_system.ipxe_script
    )
    assert "console=ttyAMA0,115200" in operating_system.ipxe_script
    assert "disk_strategy=${disk-strategy}" in operating_system.ipxe_script


def test_ephemeral_os_uses_a_new_identity_and_name_each_time():
    first = build_ephemeral_operating_system()
    second = build_ephemeral_operating_system()

    assert first.name != second.name
    assert first.ssh_private_key.asbytes() != second.ssh_private_key.asbytes()


def test_ephemeral_os_locks_password_login_by_default():
    operating_system = build_ephemeral_operating_system()

    assert operating_system.console_password is None
    assert "ssh_pwauth: false" in operating_system.user_data
    assert "passwd: '!'" in operating_system.user_data
    assert "lock_passwd: true" in operating_system.user_data
    assert "__MLT_ALLOW_PW__" not in operating_system.user_data
    assert "__MLT_USER_PASSWORD__" not in operating_system.user_data
    assert "__MLT_LOCK_PASSWD__" not in operating_system.user_data


def test_ephemeral_os_authorizes_only_its_own_key_by_default():
    operating_system = build_ephemeral_operating_system()

    assert len(_authorized_key_lines(operating_system.user_data)) == 1


def test_ephemeral_os_keeps_a_debug_key_whose_comment_looks_like_yaml():
    """An operator key is free-form text and must survive rendering intact.

    A bare " #" in the comment starts a YAML comment, so an unquoted key is
    truncated there. It still parses and still counts as one entry, so only
    comparing the key itself catches it.
    """
    awkward_key = f'{_DEBUG_KEY} #2 spare: "primary"'
    operating_system = build_ephemeral_operating_system(
        debug_public_key=awkward_key
    )

    assert _authorized_key_lines(operating_system.user_data)[1] == awkward_key


def test_ephemeral_os_adds_the_operator_debug_key_when_configured():
    operating_system = build_ephemeral_operating_system(debug_public_key=_DEBUG_KEY)

    authorized = _authorized_key_lines(operating_system.user_data)
    assert len(authorized) == 2
    # The run's own key still comes first, so the test's own login is unaffected.
    assert (
        base64.b64decode(authorized[0].split()[1], validate=True)
        == operating_system.ssh_private_key.asbytes()
    )
    assert authorized[1] == _DEBUG_KEY
    # A second key must not disturb the surrounding YAML block.
    assert "      ssh_authorized_keys:" in operating_system.user_data


def test_ephemeral_os_debug_key_does_not_enable_password_login():
    operating_system = build_ephemeral_operating_system(debug_public_key=_DEBUG_KEY)

    assert operating_system.console_password is None
    assert "ssh_pwauth: false" in operating_system.user_data


def test_ephemeral_os_mints_a_console_password_when_enabled():
    operating_system = build_ephemeral_operating_system(
        enable_console_password=True
    )

    password = operating_system.console_password
    assert password is not None and len(password) == 20
    assert "ssh_pwauth: true" in operating_system.user_data
    # The plaintext must never reach the OS definition; only its hash does.
    assert password not in operating_system.user_data
    assert "passwd: '$2b$" in operating_system.user_data
    assert "lock_passwd: false" in operating_system.user_data


def test_console_password_is_not_expired_on_first_login():
    """An expired password meets the debugger with a forced change, not a shell."""
    cloud_config = _rendered(
        build_ephemeral_operating_system(enable_console_password=True).user_data
    )

    assert cloud_config["chpasswd"]["expire"] is False


def test_ephemeral_os_console_password_hash_actually_verifies():
    """Guards against a crypt backend that silently returns a truncated hash.

    The stdlib ``crypt`` does exactly that on macOS: asking for SHA-512 yields
    a 13-character DES hash and no error, so the password would be unusable at
    the console with nothing to indicate why.
    """
    operating_system = build_ephemeral_operating_system(
        enable_console_password=True
    )

    match = re.search(r"passwd: '([^']+)'", operating_system.user_data)
    assert match is not None, "no user password rendered into user_data"
    rendered_hash = match.group(1)

    assert bcrypt.checkpw(
        operating_system.console_password.encode(), rendered_hash.encode()
    )
    assert not bcrypt.checkpw(b"not-the-password", rendered_hash.encode())


def test_ephemeral_os_uses_a_new_console_password_each_time():
    first = build_ephemeral_operating_system(enable_console_password=True)
    second = build_ephemeral_operating_system(enable_console_password=True)

    assert first.console_password != second.console_password


def _rendered(user_data: str) -> dict:
    return yaml.safe_load(user_data)


def _test_user(cloud_config: dict) -> dict:
    return next(
        user
        for user in cloud_config["users"]
        if user["name"] == "machine-lifecycle-test-user"
    )


@pytest.mark.parametrize(
    "kwargs",
    [
        {},
        {"debug_public_key": _DEBUG_KEY},
        {"enable_console_password": True},
        {"debug_public_key": _DEBUG_KEY, "enable_console_password": True},
    ],
    ids=["default", "debug-key", "console-password", "both"],
)
def test_rendered_user_data_is_valid_yaml(kwargs):
    """cloud-init rejects the whole document if rendering breaks the YAML."""
    operating_system = build_ephemeral_operating_system(**kwargs)

    cloud_config = _rendered(operating_system.user_data)

    assert cloud_config["disable_root"] is True
    assert _test_user(cloud_config)["name"] == "machine-lifecycle-test-user"


def test_allow_pw_renders_as_a_real_boolean_not_a_string():
    """A quoted 'false' parses as a truthy string and would enable password auth."""
    disabled = _rendered(build_ephemeral_operating_system().user_data)
    enabled = _rendered(
        build_ephemeral_operating_system(enable_console_password=True).user_data
    )

    assert disabled["ssh_pwauth"] is False
    assert enabled["ssh_pwauth"] is True


def test_locked_password_survives_yaml_as_a_literal_bang():
    """A bare '!' is a YAML tag indicator, so it has to come back as a string."""
    cloud_config = _rendered(build_ephemeral_operating_system().user_data)

    assert _test_user(cloud_config)["passwd"] == "!"
    assert _test_user(cloud_config)["lock_passwd"] is True


def test_the_image_identity_account_is_always_locked():
    """The qcow image's seeded nvidia account must not retain password access."""
    for kwargs in ({}, {"enable_console_password": True}):
        cloud_config = _rendered(
            build_ephemeral_operating_system(**kwargs).user_data
        )
        nvidia_user = next(
            user for user in cloud_config["users"] if user["name"] == "nvidia"
        )
        assert nvidia_user["lock_passwd"] is True


@pytest.mark.parametrize(
    ("nvidia_entry", "expected_error"),
    [
        ("", "expected cloud-init structure"),
        (
            "    - name: nvidia\n      lock_passwd: false\n",
            "rendered nvidia lock_passwd is False, expected True",
        ),
    ],
    ids=["missing", "unlocked"],
)
def test_rendered_user_data_requires_a_locked_nvidia_account(
    monkeypatch, tmp_path, nvidia_entry, expected_error
):
    template = ephemeral_os._USER_DATA_TEMPLATE.read_text(encoding="utf-8")
    template = template.replace(
        "    - name: nvidia\n      lock_passwd: true\n", nvidia_entry
    )
    modified = tmp_path / "modified-user-data.yaml"
    modified.write_text(template, encoding="utf-8")
    monkeypatch.setattr(ephemeral_os, "_USER_DATA_TEMPLATE", modified)

    with pytest.raises(ValueError, match=expected_error):
        build_ephemeral_operating_system()


def test_console_password_unlocks_the_account_it_is_set_on():
    """cloud-init locks the account regardless of hash if lock_passwd stays true."""
    cloud_config = _rendered(
        build_ephemeral_operating_system(enable_console_password=True).user_data
    )

    assert _test_user(cloud_config)["lock_passwd"] is False


def test_password_hash_survives_yaml_intact():
    """bcrypt hashes are full of '$' and '/', which YAML must not reinterpret."""
    operating_system = build_ephemeral_operating_system(
        enable_console_password=True
    )

    parsed = _test_user(_rendered(operating_system.user_data))["passwd"]

    assert parsed.startswith("$2b$")
    assert bcrypt.checkpw(operating_system.console_password.encode(), parsed.encode())


def test_authorized_keys_render_as_a_list_of_the_right_length():
    one = _rendered(build_ephemeral_operating_system().user_data)
    two = _rendered(
        build_ephemeral_operating_system(debug_public_key=_DEBUG_KEY).user_data
    )

    def keys(cloud_config):
        return _test_user(cloud_config)["ssh_authorized_keys"]

    assert len(keys(one)) == 1
    assert len(keys(two)) == 2
    assert keys(two)[1] == _DEBUG_KEY


def test_a_template_that_renders_to_broken_yaml_is_rejected(monkeypatch, tmp_path):
    """The builder must fail here, not an hour later in a provisioning timeout."""
    broken = tmp_path / "broken-user-data.yaml"
    broken.write_text(
        "#cloud-config\n"
        "ssh_pwauth: __MLT_ALLOW_PW__\n"
        "users:\n"
        "    - name: machine-lifecycle-test-user\n"
        "      passwd: __MLT_USER_PASSWORD__\n"
        "     lock_passwd: __MLT_LOCK_PASSWD__\n"  # <- bad indent
        "      ssh_authorized_keys:\n"
        "        - __MLT_SSH_PUBLIC_KEY__\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(ephemeral_os, "_USER_DATA_TEMPLATE", broken)

    with pytest.raises(ValueError, match="invalid YAML"):
        build_ephemeral_operating_system()


def test_a_template_missing_the_expected_structure_is_rejected(monkeypatch, tmp_path):
    """Valid YAML that is not the document cloud-init expects still fails."""
    wrong = tmp_path / "wrong-user-data.yaml"
    wrong.write_text(
        "#cloud-config\n"
        "ssh_pwauth: __MLT_ALLOW_PW__\n"
        "passwd: __MLT_USER_PASSWORD__\n"
        "lock_passwd: __MLT_LOCK_PASSWD__\n"
        "keys:\n"
        "    - __MLT_SSH_PUBLIC_KEY__\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(ephemeral_os, "_USER_DATA_TEMPLATE", wrong)

    with pytest.raises(ValueError, match="expected cloud-init structure"):
        build_ephemeral_operating_system()
