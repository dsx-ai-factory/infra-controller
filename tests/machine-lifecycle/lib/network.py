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

"""Network helpers for a NICo instance."""

import datetime
import socket
import time

import requests


def wait_for_host_port(
    hostname: str, port: int, max_retries: int = 20, sleep_time: float = 30
) -> None:
    """Wait until `port` on `hostname` is connectable.

    Sleep for `sleep_time` in between attempts, up to `max_retries`.
    :raises TimeoutError: if not successful within `max_retries`
    """
    _time_print(
        f"Attempting to connect to {hostname} port {port} up to {max_retries} times with "
        f"{sleep_time=}"
    )
    retry_count = 0
    while retry_count < max_retries:
        # Check if the port is up on the host: 0 for yes 1 for no
        if _test_host_port(hostname, port) == 0:
            _time_print(
                f"Successfully connected to {hostname} port {port} on attempt {retry_count}"
            )
            return
        retry_count += 1
        _time_print(f"Unable to connect to {hostname} port {port} after {retry_count} attempts")
        time.sleep(sleep_time)

    raise TimeoutError(_time_print(f"Unable to contact {hostname} port {port}"))


def wait_for_redfish_endpoint(
    hostname: str, max_retries: int = 20, sleep_time: float = 10, consecutive_successes: int = 3
) -> None:
    """Wait until (unauthenticated) Redfish API endpoint is responding
    consistently on a given BMC with `hostname`.

    Sleep for `sleep_time` in between attempts, up to `max_retries`.
    Requires `consecutive_successes` successful responses to ensure the
    BMC is stable.

    :raises TimeoutError: if not successful within `max_retries`
    """
    _time_print(
        f"Attempting to connect to Redfish API on {hostname} up to {max_retries} times with "
        f"{sleep_time=}, requiring {consecutive_successes} consecutive successes"
    )
    url = f"https://{hostname}/redfish/v1/"
    retry_count = 0
    success_count = 0

    while retry_count < max_retries:
        try:
            response = requests.get(url, timeout=5, verify=False)
            response.raise_for_status()
            data = response.json()
            if not data.get("Vendor"):
                raise KeyError("The 'Vendor' field is missing or empty.")

            success_count += 1
            _time_print(
                f"Successful response from Redfish API on {hostname} "
                f"({success_count}/{consecutive_successes})"
            )

            if success_count >= consecutive_successes:
                _time_print(
                    f"Redfish API on {hostname} is consistently responding after "
                    f"{consecutive_successes} consecutive successes"
                )
                return

            # Brief pause between consecutive checks
            time.sleep(5)

        except (
            requests.exceptions.RequestException,
            requests.exceptions.JSONDecodeError,
            KeyError,
        ) as e:
            _time_print(
                f"Invalid response from {hostname}: {e} \nRetrying in {sleep_time} seconds..."
            )
            success_count = 0  # Reset success count on any failure
            retry_count += 1
            time.sleep(sleep_time)

    raise TimeoutError(
        _time_print(f"No consistent response from {hostname} within the time limit.")
    )


def _time_print(message) -> str:
    string = f"{datetime.datetime.now(datetime.UTC).strftime('%Y-%m-%d %H:%M:%S')}: {message}"
    print(string)
    return string


def _test_host_port(host: str, port: int) -> int:
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.settimeout(20)
    try:
        result = sock.connect_ex((host, port))
        return result
    finally:
        sock.close()
