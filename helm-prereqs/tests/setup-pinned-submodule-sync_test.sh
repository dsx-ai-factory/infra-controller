#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SH="${SCRIPT_DIR}/../setup.sh"
PREFLIGHT_SH="${SCRIPT_DIR}/../preflight.sh"

for fn in _pinned_submodule_commit _sync_pinned_submodule _reject_retired_dpf_vars; do
    definition="$(sed -n "/^${fn}()/,/^}/p" "${SETUP_SH}")"
    if [[ -z "${definition}" ]]; then
        echo "could not extract ${fn} from setup.sh" >&2
        exit 1
    fi
    eval "${definition}"
done
eval "$(sed -n '/^_check_pinned_submodule()/,/^}/p' "${PREFLIGHT_SH}")"

# Hermetic fixtures: a local bare repo behind a file:// URL stands in for the
# upstream submodule URL (git ignores --depth for plain paths).
WORK="$(mktemp -d)"
trap 'rm -rf "${WORK}"' EXIT
export GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=protocol.file.allow GIT_CONFIG_VALUE_0=always
export GIT_AUTHOR_NAME=test GIT_AUTHOR_EMAIL=test@example.com
export GIT_COMMITTER_NAME=test GIT_COMMITTER_EMAIL=test@example.com
UPSTREAM_URL="file://${WORK}/upstream.git"

git init -q "${WORK}/upstream"
echo chart > "${WORK}/upstream/Chart.yaml"
git -C "${WORK}/upstream" add . && git -C "${WORK}/upstream" commit -q -m pinned
git clone -q --bare "${WORK}/upstream" "${WORK}/upstream.git"

git init -q "${WORK}/super"
mkdir -p "${WORK}/super/helm-prereqs"
git -C "${WORK}/super" submodule add -q "${UPSTREAM_URL}" helm-prereqs/fake
git -C "${WORK}/super" commit -q -m "pin fake"
PINNED="$(git -C "${WORK}/upstream" rev-parse HEAD)"

# Move the upstream branch tip past the pin so a shallow sync has to fetch the
# pinned commit by sha rather than land on the tip.
echo newer >> "${WORK}/upstream/Chart.yaml"
git -C "${WORK}/upstream" commit -q -am newer
git -C "${WORK}/upstream" push -q "${WORK}/upstream.git" HEAD

# A fresh clone has an uninitialized gitlink; a shallow sync must resolve it.
git clone -q "${WORK}/super" "${WORK}/clone"
SCRIPT_DIR="${WORK}/clone/helm-prereqs"
if ! _sync_pinned_submodule fake "${UPSTREAM_URL}" 'NICO_FAKE_SRC=<clone>' >/dev/null; then
    echo "sync of an uninitialized pinned submodule must succeed" >&2
    exit 1
fi
if [[ "$(git -C "${WORK}/clone/helm-prereqs/fake" rev-parse HEAD)" != "${PINNED}" ]]; then
    echo "synced submodule must sit at the pinned commit, not the upstream tip" >&2
    exit 1
fi
if ! git -C "${WORK}/clone/helm-prereqs/fake" rev-parse --is-shallow-repository | grep -qx true; then
    echo "synced submodule must be a shallow checkout" >&2
    exit 1
fi

# A dirty submodule is refused so the pin stays meaningful.
echo edited >> "${WORK}/clone/helm-prereqs/fake/Chart.yaml"
rc=0
out="$(_sync_pinned_submodule fake "${UPSTREAM_URL}" 'NICO_FAKE_SRC=<clone>' 2>&1)" || rc=$?
if [[ "${rc}" -ne 1 || "${out}" != *"has local modifications"* ]]; then
    echo "dirty submodule must be refused with rc 1 (got rc ${rc}: ${out})" >&2
    exit 1
fi

# A source tarball is neither a git checkout nor a packaged chart with a pin
# file; the hint must say so instead of pointing at git submodule status.
mkdir -p "${WORK}/tarball/helm-prereqs"
SCRIPT_DIR="${WORK}/tarball/helm-prereqs"
rc=0
out="$(_sync_pinned_submodule fake "${UPSTREAM_URL}" 'NICO_FAKE_SRC=<clone>' 2>&1)" || rc=$?
if [[ "${rc}" -ne 1 || "${out}" != *"is not a git checkout and "*"fake.pin is missing"* ]]; then
    echo "non-git checkout without a pin file must be refused with rc 1 (got rc ${rc}: ${out})" >&2
    exit 1
fi

# The packaged chart has no .git or gitlink but ships <name>.pin; the sync must
# clone the pinned commit (not the upstream tip) shallowly, and a re-run at the
# pin must not touch the network.
mkdir -p "${WORK}/chart/helm-prereqs"
printf '# pinned\n%s\n' "${PINNED}" > "${WORK}/chart/helm-prereqs/fake.pin"
SCRIPT_DIR="${WORK}/chart/helm-prereqs"
if [[ "$(_pinned_submodule_commit fake)" != "${PINNED}" ]]; then
    echo "pin file must resolve to the pinned commit outside a git checkout" >&2
    exit 1
fi
if ! _sync_pinned_submodule fake "${UPSTREAM_URL}" 'NICO_FAKE_SRC=<clone>' >/dev/null; then
    echo "sync from a packaged chart with a pin file must succeed" >&2
    exit 1
fi
if [[ "$(git -C "${WORK}/chart/helm-prereqs/fake" rev-parse HEAD)" != "${PINNED}" ]]; then
    echo "packaged-chart clone must sit at the pinned commit, not the upstream tip" >&2
    exit 1
fi
if ! git -C "${WORK}/chart/helm-prereqs/fake" rev-parse --is-shallow-repository | grep -qx true; then
    echo "packaged-chart clone must be a shallow checkout" >&2
    exit 1
fi
out="$(_sync_pinned_submodule fake "file://${WORK}/does-not-exist.git" 'NICO_FAKE_SRC=<clone>' 2>&1)"
if [[ "${out}" != *"already at the pinned commit"* ]]; then
    echo "re-run at the pinned commit must skip the clone (got: ${out})" >&2
    exit 1
fi

# preflight.sh must refuse the tarball before any phase runs, and accept both
# a checkout that records the gitlink and a packaged chart with the pin file.
ERRORS=()
SCRIPT_DIR="${WORK}/tarball/helm-prereqs" _check_pinned_submodule Fake fake 'NICO_FAKE_SRC=<clone>' --skip-fake
if [[ "${#ERRORS[@]}" -ne 1 || "${ERRORS[0]}" != *"a source tarball has neither"* ]]; then
    echo "preflight must reject a non-git checkout without a pin file (got: ${ERRORS[*]:-none})" >&2
    exit 1
fi
for dir in clone chart; do
    ERRORS=()
    SCRIPT_DIR="${WORK}/${dir}/helm-prereqs" _check_pinned_submodule Fake fake 'NICO_FAKE_SRC=<clone>' --skip-fake
    if [[ "${#ERRORS[@]}" -ne 0 ]]; then
        echo "preflight must accept ${dir} (got: ${ERRORS[*]})" >&2
        exit 1
    fi
done

# Retired DPF variables abort only when DPF installs; --skip-dpf runs ignore them.
for var in NICO_DPF_VERSION NICO_DPF_SRC_DIR; do
    rc=0
    (export INSTALL_DPF=true "${var}=v0"; _reject_retired_dpf_vars >/dev/null) || rc=$?
    if [[ "${rc}" -ne 1 ]]; then
        echo "${var} must be rejected when DPF installs (got rc ${rc})" >&2
        exit 1
    fi
    out="$(export INSTALL_DPF=false "${var}=v0"; _reject_retired_dpf_vars 2>&1)"
    if [[ -n "${out}" ]]; then
        echo "${var} must be ignored with --skip-dpf (got: ${out})" >&2
        exit 1
    fi
done
(export INSTALL_DPF=true; unset NICO_DPF_VERSION NICO_DPF_SRC_DIR; _reject_retired_dpf_vars)

guard_call_line="$(grep -nxF '_reject_retired_dpf_vars' "${SETUP_SH}" | cut -d: -f1)"
skip_dpf_line="$(grep -nF -- '--skip-dpf)     INSTALL_DPF=false ;;' "${SETUP_SH}" | cut -d: -f1)"
preflight_line="$(grep -nF 'source "${SCRIPT_DIR}/preflight.sh"' "${SETUP_SH}" | cut -d: -f1)"
if ! (( skip_dpf_line < guard_call_line && guard_call_line < preflight_line )); then
    echo "retired-variable guard must run after --skip-dpf is parsed and before preflight" >&2
    exit 1
fi

echo "setup pinned submodule sync tests passed"
