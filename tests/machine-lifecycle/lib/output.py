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

"""Consistent, plain-text lifecycle output for terminals and CI logs."""

import sys
from typing import TextIO


_BANNER_WIDTH = 72
_active_stage = "Startup"


def _print_banner(
    title: str,
    *,
    marker: str,
    stream: TextIO,
    details: tuple[str, ...] = (),
) -> None:
    print(file=stream)
    print(marker * _BANNER_WIDTH, file=stream)
    print(title, file=stream)
    for detail in details:
        print(detail, file=stream)
    print(marker * _BANNER_WIDTH, file=stream, flush=True)


def start_run() -> None:
    """Print the run header and reset the active lifecycle stage."""
    global _active_stage
    _active_stage = "Startup"
    _print_banner("MACHINE LIFECYCLE TEST", marker="=", stream=sys.stdout)


def start_stage(name: str) -> None:
    """Record and print the lifecycle stage that is about to run."""
    global _active_stage
    _active_stage = name
    _print_banner(f"STAGE: {name}", marker="-", stream=sys.stdout)


def print_failure(message: str) -> None:
    """Print a prominent failure summary for the active lifecycle stage."""
    # Keep buffered stdout stages ahead of the stderr failure in combined CI logs.
    sys.stdout.flush()
    _print_banner(
        "MLT FAILED",
        marker="!",
        stream=sys.stderr,
        details=(f"Stage:  {_active_stage}", f"Reason: {message}"),
    )


def print_success(elapsed_seconds: float) -> None:
    """Print a successful run summary."""
    _print_banner(
        "MLT PASSED",
        marker="=",
        stream=sys.stdout,
        details=(f"Duration: {elapsed_seconds:.1f} seconds",),
    )
