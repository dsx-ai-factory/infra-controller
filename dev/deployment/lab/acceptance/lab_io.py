# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Bounded command execution and durable writes for local lab journals."""

import contextlib
import json
import os
from pathlib import Path
import signal
import subprocess


def run(command, timeout=90, env=None, errors="strict", log_path=None):
    with contextlib.ExitStack() as stack:
        log = stack.enter_context(log_path.open('w')) if log_path else None
        process = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log if log_path else subprocess.PIPE,
                                   stderr=subprocess.STDOUT if log_path else subprocess.PIPE, text=True, errors=errors, env=env, start_new_session=True)
        try:
            output, error = process.communicate(timeout=timeout)
            if process.returncode:
                detail = error or (log_path.read_text(errors='replace') if log_path else output)
                raise RuntimeError(f'{Path(command[0]).name} exited {process.returncode}: {detail[-2000:]}')
            return output or ''
        finally:
            process.poll()
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            if process.stdout:
                process.stdout.close()
            if process.stderr:
                process.stderr.close()


def save(path, value):
    temporary = path.with_suffix('.pending')
    with temporary.open('w') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(path)
    descriptor = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
