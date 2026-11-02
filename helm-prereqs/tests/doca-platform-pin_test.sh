#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# helm-prereqs/doca-platform.pin is the DPF source for the packaged chart, which
# has no gitlink; it must name the same commit as the doca-platform submodule.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PIN_FILE="${SCRIPT_DIR}/../doca-platform.pin"
REPO_ROOT="${SCRIPT_DIR}/../.."

pinned="$(grep -E '^[0-9a-f]{40}$' "${PIN_FILE}" || true)"
if [[ "$(printf '%s\n' "${pinned}" | grep -c .)" -ne 1 ]]; then
    echo "doca-platform.pin must contain exactly one 40-char commit sha line (got: ${pinned:-none})" >&2
    exit 1
fi

gitlink="$(git -C "${REPO_ROOT}" ls-files -s -- helm-prereqs/doca-platform | awk '$1 == "160000" { print $2 }')"
if [[ -z "${gitlink}" ]]; then
    echo "helm-prereqs/doca-platform is not recorded as a submodule gitlink" >&2
    exit 1
fi
if [[ "${pinned}" != "${gitlink}" ]]; then
    echo "doca-platform.pin (${pinned}) differs from the helm-prereqs/doca-platform gitlink (${gitlink}); bump both together" >&2
    exit 1
fi

# The packaged chart must carry the pin file: .helmignore ignores the submodule
# directory only, never the pin.
if grep -qE '^(\*\.pin|doca-platform\.pin|doca-platform\*|\*)$' "${SCRIPT_DIR}/../.helmignore"; then
    echo ".helmignore must not exclude doca-platform.pin" >&2
    exit 1
fi

echo "doca-platform pin tests passed"
