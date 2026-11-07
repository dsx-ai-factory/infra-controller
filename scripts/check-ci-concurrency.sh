#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

usage() {
	echo "Usage: check-ci-concurrency.sh [all|core|rest]"
}

if (( $# > 1 )); then
	usage >&2
	exit 2
fi

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
	usage
	exit 0
fi

mode="${1:-all}"
if [[ "${mode}" != "all" && "${mode}" != "core" && "${mode}" != "rest" ]]; then
	usage >&2
	exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

assert_concurrency_policy() {
	local workflow_path="$1"
	local expected_group="$2"
	local expected_cancel="$3"
	local blocks
	local groups
	local cancels

	read -r blocks groups cancels < <(
		awk -v expected_group="${expected_group}" -v expected_cancel="${expected_cancel}" '
			$0 == "concurrency:" { blocks++; in_concurrency = 1; next }
			in_concurrency && /^[^[:space:]#]/ { in_concurrency = 0 }
			in_concurrency && $0 == expected_group { groups++ }
			in_concurrency && $0 == expected_cancel { cancels++ }
			END { print blocks + 0, groups + 0, cancels + 0 }
		' "${workflow_path}"
	)

	if (( blocks != 1 || groups != 1 || cancels != 1 )); then
		printf 'Expected one root concurrency policy in %s; found blocks=%d groups=%d cancel-in-progress=%d.\n' \
			"${workflow_path#"${repo_root}/"}" "${blocks}" "${groups}" "${cancels}" >&2
		return 1
	fi
}

assert_workflow_policy() {
	local workflow="$1"
	local workflow_path
	local expected_group
	local expected_cancel

	case "${workflow}" in
	core)
		workflow_path="${repo_root}/.github/workflows/ci.yaml"
		expected_group="  group: \${{ github.event_name == 'push' && github.run_attempt == '1' && startsWith(github.ref, 'refs/heads/pull-request/') && format('core-pr-{0}', github.ref_name) || format('core-run-{0}-{1}', github.run_id, github.run_attempt) }}"
		;;
	rest)
		workflow_path="${repo_root}/.github/workflows/rest-ci.yml"
		expected_group="  group: \${{ github.event_name == 'push' && github.run_attempt == '1' && startsWith(github.ref, 'refs/heads/pull-request/') && format('rest-pr-{0}', github.ref_name) || format('rest-run-{0}-{1}', github.run_id, github.run_attempt) }}"
		;;
	esac

	expected_cancel="  cancel-in-progress: \${{ github.event_name == 'push' && github.run_attempt == '1' && startsWith(github.ref, 'refs/heads/pull-request/') }}"
	assert_concurrency_policy "${workflow_path}" "${expected_group}" "${expected_cancel}"
}

resolve_policy() {
	local workflow="$1"
	local event_name="$2"
	local ref="$3"
	local run_id="$4"
	local run_attempt="$5"
	local ref_name

	resolved_cancel=false
	if [[ "${event_name}" == "push" && "${run_attempt}" == "1" && "${ref}" == refs/heads/pull-request/* ]]; then
		ref_name="${ref#refs/heads/}"
		resolved_group="${workflow}-pr-${ref_name}"
		resolved_cancel=true
	else
		resolved_group="${workflow}-run-${run_id}-${run_attempt}"
	fi
}

fixtures=(
	'core|first Core PR run|push|refs/heads/pull-request/4575|1001|1|core-pr-pull-request/4575|true'
	'core|newer run for the same Core PR|push|refs/heads/pull-request/4575|1002|1|core-pr-pull-request/4575|true'
	'core|different Core PR|push|refs/heads/pull-request/4576|1003|1|core-pr-pull-request/4576|true'
	'core|Core main push|push|refs/heads/main|2001|1|core-run-2001-1|false'
	'core|Core tag push|push|refs/tags/v2.2.0|2002|1|core-run-2002-1|false'
	'core|Core merge group|merge_group|refs/heads/gh-readonly-queue/main/pr-4575|2003|1|core-run-2003-1|false'
	'core|rerun of an older Core PR run|push|refs/heads/pull-request/4575|1001|2|core-run-1001-2|false'
	'rest|first REST PR run|push|refs/heads/pull-request/4575|3001|1|rest-pr-pull-request/4575|true'
	'rest|newer run for the same REST PR|push|refs/heads/pull-request/4575|3002|1|rest-pr-pull-request/4575|true'
	'rest|different REST PR|push|refs/heads/pull-request/4576|3003|1|rest-pr-pull-request/4576|true'
	'rest|REST main push|push|refs/heads/main|4001|1|rest-run-4001-1|false'
	'rest|REST tag push|push|refs/tags/v2.2.0|4002|1|rest-run-4002-1|false'
	'rest|REST manual run on main|workflow_dispatch|refs/heads/main|4003|1|rest-run-4003-1|false'
	'rest|REST manual run on a PR mirror|workflow_dispatch|refs/heads/pull-request/4575|4004|1|rest-run-4004-1|false'
	'rest|REST merge group|merge_group|refs/heads/gh-readonly-queue/main/pr-4575|4005|1|rest-run-4005-1|false'
	'rest|rerun of an older REST PR run|push|refs/heads/pull-request/4575|3001|2|rest-run-3001-2|false'
)

if [[ "${mode}" == "all" || "${mode}" == "core" ]]; then
	assert_workflow_policy "core"
fi
if [[ "${mode}" == "all" || "${mode}" == "rest" ]]; then
	assert_workflow_policy "rest"
fi

failed=0
checked=0
for fixture in "${fixtures[@]}"; do
	IFS='|' read -r fixture_workflow fixture_name event_name ref run_id run_attempt expected_group expected_cancel <<<"${fixture}"
	if [[ "${mode}" != "all" && "${mode}" != "${fixture_workflow}" ]]; then
		continue
	fi

	resolve_policy "${fixture_workflow}" "${event_name}" "${ref}" "${run_id}" "${run_attempt}"
	checked=$((checked + 1))
	if [[ "${resolved_group}" != "${expected_group}" || "${resolved_cancel}" != "${expected_cancel}" ]]; then
		printf '%s failed: expected group=%s cancel=%s, got group=%s cancel=%s\n' \
			"${fixture_name}" "${expected_group}" "${expected_cancel}" \
			"${resolved_group}" "${resolved_cancel}" >&2
		failed=1
	fi
done

if (( failed )); then
	exit 1
fi

printf 'Checked %d %s CI concurrency fixture(s).\n' "${checked}" "${mode}"
