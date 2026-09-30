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

import json
from types import SimpleNamespace

import pytest

from lib.reset_drivers import ResetDriverError, ResetTarget
from lib import dell_factory_reset
from lib.reset_drivers import bluefield, dell, gb200, lenovo
from lib.site_vault import BmcCredentials
from tests.lifecycle import machine_lifecycle_test as lifecycle


_CREDENTIALS = BmcCredentials("operator", "secret")


class _Response:
    def __init__(self, status_code: int, payload: dict | None = None, text: str = ""):
        self.status_code = status_code
        self._payload = payload or {}
        self.text = text

    def json(self) -> dict:
        return self._payload


def _target(*, machine_id: str = "machine-id", label: str = "host") -> ResetTarget:
    return ResetTarget(
        machine_id=machine_id,
        bmc_ip="192.0.2.10",
        credentials=_CREDENTIALS,
        label=label,
    )


def _fail_on_wait(call_number):
    calls = 0

    def wait_for_redfish_endpoint(**_kwargs):
        nonlocal calls
        calls += 1
        if calls == call_number:
            raise RuntimeError("endpoint unavailable")

    return wait_for_redfish_endpoint


def test_bluefield_driver_preserves_bios_restart_factory_reset_order(monkeypatch):
    events = []

    def patch(url, **kwargs):
        events.append(("bios", url, kwargs))
        return _Response(200)

    monkeypatch.setattr(bluefield.requests, "patch", patch)
    monkeypatch.setattr(
        bluefield.admin_cli,
        "restart_bmc",
        lambda machine_id: events.append(("restart-bmc", machine_id)),
    )
    monkeypatch.setattr(
        bluefield.admin_cli,
        "factory_reset_bmc",
        lambda *args: events.append(("factory-reset-bmc", *args)),
    )
    monkeypatch.setattr(
        bluefield.network,
        "wait_for_redfish_endpoint",
        lambda **kwargs: events.append(("wait-redfish", kwargs)),
    )
    monkeypatch.setattr(
        bluefield.time,
        "sleep",
        lambda seconds: events.append(("sleep", seconds)),
    )

    bluefield.BlueFieldDpuResetDriver().reset_dpu(
        _target(machine_id="dpu-id", label="DPU1")
    )

    assert events == [
        (
            "bios",
            "https://192.0.2.10/redfish/v1/Systems/Bluefield/Bios/Settings",
            {
                "json": {"Attributes": {"ResetEfiVars": True}},
                "auth": ("operator", "secret"),
                "verify": False,
                "timeout": 60,
            },
        ),
        ("restart-bmc", "dpu-id"),
        ("sleep", 5),
        ("wait-redfish", {"hostname": "192.0.2.10"}),
        ("factory-reset-bmc", "192.0.2.10", "operator", "secret"),
        ("sleep", 5),
        ("wait-redfish", {"hostname": "192.0.2.10", "sleep_time": 10}),
    ]


def test_bluefield_unreachable_bmc_keeps_maintenance_policy_and_stops(monkeypatch):
    def patch(*_args, **_kwargs):
        raise bluefield.requests.ConnectTimeout("no route to BMC")

    monkeypatch.setattr(bluefield.requests, "patch", patch)
    monkeypatch.setattr(
        bluefield.admin_cli,
        "restart_bmc",
        lambda *_args: pytest.fail("BMC restart must not follow an unreachable BMC"),
    )

    with pytest.raises(ResetDriverError, match="DPU1") as raised:
        bluefield.BlueFieldDpuResetDriver().reset_dpu(_target(label="DPU1"))

    assert raised.value.set_maintenance is True


def test_bluefield_bios_failure_keeps_maintenance_policy_and_stops(monkeypatch):
    monkeypatch.setattr(
        bluefield.requests,
        "patch",
        lambda *_args, **_kwargs: _Response(500, text="failed"),
    )
    monkeypatch.setattr(
        bluefield.admin_cli,
        "restart_bmc",
        lambda *_args: pytest.fail("BMC restart must not follow a failed BIOS reset"),
    )

    with pytest.raises(ResetDriverError, match="DPU1") as raised:
        bluefield.BlueFieldDpuResetDriver().reset_dpu(
            _target(machine_id="dpu-id", label="DPU1")
        )

    assert raised.value.set_maintenance is True


@pytest.mark.parametrize(
    ("failing_wait", "message"),
    [
        (1, "waiting for DPU1 BMC restart"),
        (2, "waiting for DPU1 BMC factory reset"),
    ],
)
def test_bluefield_driver_translates_recovery_wait_failures(
    monkeypatch, failing_wait, message
):
    monkeypatch.setattr(
        bluefield.requests,
        "patch",
        lambda *_args, **_kwargs: _Response(200),
    )
    monkeypatch.setattr(bluefield.admin_cli, "restart_bmc", lambda _machine_id: None)
    monkeypatch.setattr(
        bluefield.admin_cli,
        "factory_reset_bmc",
        lambda *_args: None,
    )
    monkeypatch.setattr(bluefield.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(
        bluefield.network,
        "wait_for_redfish_endpoint",
        _fail_on_wait(failing_wait),
    )

    with pytest.raises(ResetDriverError, match=message) as raised:
        bluefield.BlueFieldDpuResetDriver().reset_dpu(_target(label="DPU1"))

    assert raised.value.set_maintenance is True


def test_lenovo_driver_waits_for_bios_task_before_later_steps(monkeypatch):
    events = []
    task_responses = iter(
        [
            _Response(200, {"TaskState": "Running"}),
            _Response(200, {"TaskState": "Completed"}),
        ]
    )

    def post(url, **kwargs):
        events.append(("reset-bios", url, kwargs))
        return _Response(202, {"Id": "task-1"})

    def get(url, **kwargs):
        response = next(task_responses)
        events.append(("poll-task", url, kwargs, response.json()["TaskState"]))
        return response

    monkeypatch.setattr(lenovo.requests, "post", post)
    monkeypatch.setattr(lenovo.requests, "get", get)
    monkeypatch.setattr(
        lenovo.admin_cli,
        "clear_host_bios_password",
        lambda machine_id: events.append(("clear-password", machine_id)),
    )
    monkeypatch.setattr(
        lenovo.admin_cli,
        "restart_machine",
        lambda machine_id: events.append(("restart-host", machine_id)),
    )
    monkeypatch.setattr(
        lenovo.admin_cli,
        "factory_reset_bmc",
        lambda *args: events.append(("factory-reset-bmc", *args)),
    )
    monkeypatch.setattr(
        lenovo.network,
        "wait_for_redfish_endpoint",
        lambda **kwargs: events.append(("wait-redfish", kwargs)),
    )
    monkeypatch.setattr(
        lenovo.time,
        "sleep",
        lambda seconds: events.append(("sleep", seconds)),
    )

    lenovo.LenovoHostResetDriver().reset_host(_target())

    assert events == [
        (
            "reset-bios",
            "https://192.0.2.10/redfish/v1/Systems/1/Bios/Actions/Bios.ResetBios",
            {
                "json": {"ResetType": "default"},
                "auth": ("operator", "secret"),
                "verify": False,
                "timeout": 60,
            },
        ),
        (
            "poll-task",
            "https://192.0.2.10/redfish/v1/TaskService/Tasks/task-1",
            {"auth": ("operator", "secret"), "verify": False, "timeout": 60},
            "Running",
        ),
        ("sleep", 10),
        (
            "poll-task",
            "https://192.0.2.10/redfish/v1/TaskService/Tasks/task-1",
            {"auth": ("operator", "secret"), "verify": False, "timeout": 60},
            "Completed",
        ),
        ("clear-password", "machine-id"),
        ("restart-host", "machine-id"),
        ("sleep", 10),
        ("wait-redfish", {"hostname": "192.0.2.10"}),
        ("factory-reset-bmc", "192.0.2.10", "operator", "secret"),
        ("sleep", 5),
        ("wait-redfish", {"hostname": "192.0.2.10"}),
    ]


def test_lenovo_unreachable_bmc_keeps_maintenance_policy_and_stops(monkeypatch):
    def post(*_args, **_kwargs):
        raise lenovo.requests.ConnectTimeout("no route to BMC")

    monkeypatch.setattr(lenovo.requests, "post", post)
    monkeypatch.setattr(
        lenovo.admin_cli,
        "clear_host_bios_password",
        lambda *_args: pytest.fail("later steps must not follow an unreachable BMC"),
    )

    with pytest.raises(ResetDriverError, match="host") as raised:
        lenovo.LenovoHostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is True


def test_lenovo_lost_bmc_during_task_poll_keeps_maintenance_policy(monkeypatch):
    monkeypatch.setattr(
        lenovo.requests,
        "post",
        lambda *_args, **_kwargs: _Response(202, {"Id": "task-1"}),
    )

    def get(*_args, **_kwargs):
        raise lenovo.requests.ConnectionError("connection reset")

    monkeypatch.setattr(lenovo.requests, "get", get)

    with pytest.raises(ResetDriverError, match="task-1") as raised:
        lenovo.LenovoHostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is True


def test_lenovo_bios_task_timeout_keeps_maintenance_policy(monkeypatch):
    monkeypatch.setattr(
        lenovo.requests,
        "post",
        lambda *_args, **_kwargs: _Response(202, {"Id": "task-1"}),
    )
    timeouts = []

    def get(_url, **kwargs):
        timeouts.append(kwargs["timeout"])
        return _Response(200, {"TaskState": "Running"})

    monkeypatch.setattr(lenovo.requests, "get", get)
    sleeps = []
    monkeypatch.setattr(lenovo.time, "sleep", sleeps.append)
    # Deadline set at t=0. The clock is read before each poll and again to cap
    # the sleep: polls at t=0, 100 and 250 proceed, t=400 raises without another
    # request.
    clock = iter([0.0, 0.0, 5.0, 100.0, 105.0, 250.0, 255.0, 400.0])
    monkeypatch.setattr(lenovo.time, "monotonic", lambda: next(clock))

    with pytest.raises(ResetDriverError, match="did not complete") as raised:
        lenovo.LenovoHostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is True
    # The deadline, not a poll count, ends the wait: three polls, three sleeps,
    # and no fourth request. The last poll had 50s left, so its timeout shrank.
    assert timeouts == [60, 60, 50]
    assert sleeps == [10, 10, 10]


@pytest.mark.parametrize(
    ("failing_wait", "message"),
    [
        (1, "waiting for Lenovo host to recover"),
        (2, "waiting for Lenovo BMC to recover"),
    ],
)
def test_lenovo_driver_translates_recovery_wait_failures(
    monkeypatch, failing_wait, message
):
    monkeypatch.setattr(
        lenovo.requests,
        "post",
        lambda *_args, **_kwargs: _Response(202, {"Id": "task-1"}),
    )
    monkeypatch.setattr(
        lenovo.requests,
        "get",
        lambda *_args, **_kwargs: _Response(200, {"TaskState": "Completed"}),
    )
    monkeypatch.setattr(
        lenovo.admin_cli,
        "clear_host_bios_password",
        lambda _machine_id: None,
    )
    monkeypatch.setattr(lenovo.admin_cli, "restart_machine", lambda _machine_id: None)
    monkeypatch.setattr(
        lenovo.admin_cli,
        "factory_reset_bmc",
        lambda *_args: None,
    )
    monkeypatch.setattr(lenovo.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(
        lenovo.network,
        "wait_for_redfish_endpoint",
        _fail_on_wait(failing_wait),
    )

    with pytest.raises(ResetDriverError, match=message) as raised:
        lenovo.LenovoHostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is True


def _dell_methods() -> dell_factory_reset.DellFactoryResetMethods:
    return dell_factory_reset.DellFactoryResetMethods("192.0.2.10", "root", "secret")


def test_dell_server_status_fails_cleanly_after_exhausting_retries(monkeypatch):
    class _Undecodable:
        status_code = 200

        def json(self):
            raise json.JSONDecodeError("not json", "", 0)

    monkeypatch.setattr(dell_factory_reset.requests, "get", lambda *_args, **_kwargs: _Undecodable())
    monkeypatch.setattr(dell_factory_reset.time, "sleep", lambda _seconds: None)

    with pytest.raises(dell_factory_reset.DellFactoryResetError, match="after 10 attempts"):
        _dell_methods()._get_server_status()


def test_dell_reboot_paces_its_shutdown_polls_and_forces_off_at_the_deadline(monkeypatch):
    # Initial status, two polls still On, then Off after the forced shutdown.
    power_states = iter(["On", "On", "On", "Off"])
    clock = iter([0.0, 100.0, 400.0])
    posts = []
    sleeps = []

    monkeypatch.setattr(
        dell_factory_reset.requests,
        "get",
        lambda *_args, **_kwargs: _Response(200, {"PowerState": next(power_states)}),
    )

    def post(_url, **kwargs):
        posts.append(kwargs["json"]["ResetType"])
        return _Response(204)

    monkeypatch.setattr(dell_factory_reset.requests, "post", post)
    monkeypatch.setattr(dell_factory_reset.time, "sleep", sleeps.append)
    monkeypatch.setattr(dell_factory_reset.time, "monotonic", lambda: next(clock))

    _dell_methods().reboot_server()

    assert posts == ["GracefulShutdown", "ForceOff", "On"]
    # Settle after the graceful request, pause between polls, settle after ForceOff.
    assert sleeps == [15, 15, 15]


def test_dell_reboot_fails_when_the_forced_shutdown_is_rejected(monkeypatch):
    power_states = iter(["On", "On"])
    clock = iter([0.0, 400.0])

    monkeypatch.setattr(
        dell_factory_reset.requests,
        "get",
        lambda *_args, **_kwargs: _Response(200, {"PowerState": next(power_states)}),
    )
    monkeypatch.setattr(
        dell_factory_reset.requests,
        "post",
        lambda _url, **kwargs: _Response(
            204 if kwargs["json"]["ResetType"] == "GracefulShutdown" else 500
        ),
    )
    monkeypatch.setattr(dell_factory_reset.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(dell_factory_reset.time, "monotonic", lambda: next(clock))

    with pytest.raises(dell_factory_reset.DellFactoryResetError, match="forced shutdown failed"):
        _dell_methods().reboot_server()


def test_dell_driver_preserves_existing_method_and_wait_order(monkeypatch):
    events = []

    class Methods:
        def __init__(self, *args):
            events.append(("construct", *args))

        def unlock_idrac(self):
            events.append(("unlock-idrac",))

        def reset_bios(self):
            events.append(("reset-bios",))

        def reboot_server(self):
            events.append(("reboot-server",))

        def disable_host_header_check(self):
            events.append(("disable-host-header-check",))

        def factory_reset_bmc(self, *, level):
            events.append(("factory-reset-bmc", level))

    monkeypatch.setattr(dell, "DellFactoryResetMethods", Methods)
    monkeypatch.setattr(dell.time, "sleep", lambda seconds: events.append(("sleep", seconds)))
    monkeypatch.setattr(
        dell.network,
        "wait_for_redfish_endpoint",
        lambda **kwargs: events.append(("wait-redfish", kwargs)),
    )

    dell.DellHostResetDriver().reset_host(_target())

    assert events == [
        ("construct", "192.0.2.10", "operator", "secret"),
        ("unlock-idrac",),
        ("reset-bios",),
        ("reboot-server",),
        ("sleep", 300),
        ("wait-redfish", {"hostname": "192.0.2.10"}),
        ("disable-host-header-check",),
        ("factory-reset-bmc", "ResetAllWithRootDefaults"),
        ("sleep", 120),
        ("wait-redfish", {"hostname": "192.0.2.10", "max_retries": 40}),
    ]


def test_dell_driver_keeps_non_maintenance_failure_policy(monkeypatch):
    class Methods:
        def __init__(self, *_args):
            pass

        def unlock_idrac(self):
            raise RuntimeError("locked")

    monkeypatch.setattr(dell, "DellFactoryResetMethods", Methods)

    with pytest.raises(ResetDriverError, match="iDRAC unlock") as raised:
        dell.DellHostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is False


@pytest.mark.parametrize(
    ("failing_wait", "message"),
    [
        (1, "waiting for Dell host to recover"),
        (2, "waiting for iDRAC to recover"),
    ],
)
def test_dell_driver_translates_recovery_wait_failures(
    monkeypatch, failing_wait, message
):
    class Methods:
        def __init__(self, *_args):
            pass

        def unlock_idrac(self):
            pass

        def reset_bios(self):
            pass

        def reboot_server(self):
            pass

        def disable_host_header_check(self):
            pass

        def factory_reset_bmc(self, *, level):
            pass

    wait_count = 0

    def wait_for_redfish_endpoint(**_kwargs):
        nonlocal wait_count
        wait_count += 1
        if wait_count == failing_wait:
            raise RuntimeError("endpoint unavailable")

    monkeypatch.setattr(dell, "DellFactoryResetMethods", Methods)
    monkeypatch.setattr(dell.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(
        dell.network,
        "wait_for_redfish_endpoint",
        wait_for_redfish_endpoint,
    )

    with pytest.raises(ResetDriverError, match=message) as raised:
        dell.DellHostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is False


def test_gb200_driver_preserves_bios_then_bmc_order(monkeypatch):
    events = []

    class Methods:
        def __init__(self, *args):
            events.append(("construct", *args))

        def reset_bios(self):
            events.append(("reset-bios",))

        def factory_reset_bmc(self, *, reset_type):
            events.append(("factory-reset-bmc", reset_type))

    monkeypatch.setattr(gb200, "GB200FactoryResetMethods", Methods)
    monkeypatch.setattr(
        gb200.time,
        "sleep",
        lambda seconds: events.append(("sleep", seconds)),
    )
    monkeypatch.setattr(
        gb200.network,
        "wait_for_redfish_endpoint",
        lambda **kwargs: events.append(("wait-redfish", kwargs)),
    )

    gb200.GB200HostResetDriver().reset_host(_target())

    assert events == [
        ("construct", "192.0.2.10", "operator", "secret"),
        ("reset-bios",),
        ("wait-redfish", {"hostname": "192.0.2.10"}),
        ("factory-reset-bmc", "ResetAll"),
        ("sleep", 120),
        ("wait-redfish", {"hostname": "192.0.2.10", "max_retries": 40}),
    ]


def test_gb200_failure_keeps_maintenance_policy(monkeypatch):
    class Methods:
        def __init__(self, *_args):
            pass

        def reset_bios(self):
            raise RuntimeError("rejected")

    monkeypatch.setattr(gb200, "GB200FactoryResetMethods", Methods)

    with pytest.raises(ResetDriverError, match="GB200 BIOS reset") as raised:
        gb200.GB200HostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is True


@pytest.mark.parametrize(
    ("failing_wait", "message"),
    [
        (1, "waiting for GB200 host to recover"),
        (2, "waiting for GB200 BMC to recover"),
    ],
)
def test_gb200_driver_translates_recovery_wait_failures(
    monkeypatch, failing_wait, message
):
    class Methods:
        def __init__(self, *_args):
            pass

        def reset_bios(self):
            pass

        def factory_reset_bmc(self, *, reset_type):
            pass

    monkeypatch.setattr(gb200, "GB200FactoryResetMethods", Methods)
    monkeypatch.setattr(gb200.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(
        gb200.network,
        "wait_for_redfish_endpoint",
        _fail_on_wait(failing_wait),
    )

    with pytest.raises(ResetDriverError, match=message) as raised:
        gb200.GB200HostResetDriver().reset_host(_target())

    assert raised.value.set_maintenance is True


def test_lifecycle_finishes_all_dpu_drivers_before_host_driver(monkeypatch):
    events = []

    class DpuDriver:
        def reset_dpu(self, target):
            events.append(("dpu", target.machine_id, target.label))

    class HostDriver:
        def reset_host(self, target):
            events.append(("host", target.machine_id, target.label))

    monkeypatch.setattr(lifecycle, "DPU_RESET_DRIVER", DpuDriver())
    monkeypatch.setitem(lifecycle.HOST_RESET_DRIVERS, "dell", HostDriver())

    test_config = SimpleNamespace(machine_under_test="host-id")
    site_config = SimpleNamespace(
        host_bmc_credentials=_CREDENTIALS,
        dpu_bmc_credentials={"dpu-1": _CREDENTIALS, "dpu-2": _CREDENTIALS},
    )
    machine_info = SimpleNamespace(
        vendor="dell",
        host_bmc_ip="192.0.2.10",
        dpu_ids=["dpu-1", "dpu-2"],
        dpu_info_map={
            "dpu-1": {"bmc_ip": "192.0.2.11"},
            "dpu-2": {"bmc_ip": "192.0.2.12"},
        },
    )

    lifecycle.perform_factory_reset(test_config, site_config, machine_info)

    assert events == [
        ("dpu", "dpu-1", "DPU1"),
        ("dpu", "dpu-2", "DPU2"),
        ("host", "host-id", "host"),
    ]


def test_lifecycle_translates_driver_maintenance_policy(monkeypatch):
    class FailingDriver:
        def reset_dpu(self, _target):
            raise ResetDriverError("reset failed", set_maintenance=True)

    exits = []

    def fail(message, set_maintenance=False, machine_id=None):
        exits.append((message, set_maintenance, machine_id))
        raise RuntimeError("exited")

    monkeypatch.setattr(lifecycle, "DPU_RESET_DRIVER", FailingDriver())
    monkeypatch.setattr(lifecycle, "_error_and_exit", fail)

    with pytest.raises(RuntimeError, match="exited"):
        lifecycle._factory_reset_dpu(
            SimpleNamespace(machine_under_test="host-id"),
            SimpleNamespace(dpu_bmc_credentials={"dpu-1": _CREDENTIALS}),
            SimpleNamespace(
                dpu_ids=["dpu-1"],
                dpu_info_map={"dpu-1": {"bmc_ip": "192.0.2.11"}},
            ),
        )

    assert exits == [("reset failed", True, "host-id")]
