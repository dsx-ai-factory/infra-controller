# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Backend interface for the NICo unified code generator.

Adding a backend
----------------
Drop ``backends/<name>.py`` and expose a module-level ``generate`` function
with the signature below.  The CLI discovers backends by module name
(``--backend ansible`` → ``backends.ansible``).

The :class:`GeneratorBackend` Protocol documents the required signature for
IDE support and static analysis.  It is not enforced at runtime.
"""

from __future__ import annotations

try:
    from typing import Protocol, runtime_checkable
except ImportError:
    from typing_extensions import Protocol, runtime_checkable  # type: ignore[assignment]


@runtime_checkable
class GeneratorBackend(Protocol):
    """Protocol satisfied by every generator backend module.

    Backend modules expose a module-level ``generate`` function, not a class,
    so this Protocol documents the contract rather than enforcing it at
    runtime.  Structural subtyping via duck-typing applies.
    """

    def generate(
        self,
        spec: dict,
        output_dir: str,
        dry_run: bool = False,
        docs_output: 'str | None' = None,
        root: 'str | None' = None,
        **kwargs,
    ) -> 'list[str]':
        """Generate backend-specific artifacts from the OpenAPI spec.

        Parameters
        ----------
        spec:
            Fully loaded OpenAPI spec dict (pass through ``common.load_spec``).
        output_dir:
            Directory in which to write generated files.
        dry_run:
            When True, print what would be generated without writing files.
        docs_output:
            Optional directory for generated documentation pages.
        root:
            Repository root path.  When provided, the backend may also update
            metadata files (e.g. ``galaxy.yml``, ``runtime.yml``) in place.

        Returns
        -------
        list[str]:
            Sorted list of generated filenames (basenames only, not full paths).
        """
        ...
