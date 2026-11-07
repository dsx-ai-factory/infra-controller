#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

usage() {
	echo "Usage: check-migration-immutability.sh --base REVISION"
}

escape_workflow_data() {
	local value="$1"

	value="${value//'%'/'%25'}"
	value="${value//$'\r'/'%0D'}"
	value="${value//$'\n'/'%0A'}"
	printf '%s' "${value}"
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
	usage
	exit 0
elif [[ "${1:-}" != "--base" ]] || (( $# != 2 )); then
	usage >&2
	exit 2
fi

base_revision="$2"
changed_migrations="$(mktemp)"
trap 'rm -f "${changed_migrations}"' EXIT

# Disable rename detection so moving an existing migration is represented as a
# prohibited deletion plus an allowed addition. Additions are intentionally
# excluded: migrations introduced after the merge base have not been deployed.
git diff --no-renames --diff-filter=MDTUXB --name-only -z \
	"${base_revision}...HEAD" \
	-- \
	':(top,glob)crates/api-db/migrations/**' \
	':(top,glob)crates/api-db/migrations.*/**' >"${changed_migrations}"

failed=0

while IFS= read -r -d '' migration_file; do
	message="Core database migration ${migration_file} already exists in ${base_revision}; add a new migration instead."
	printf '::error title=Existing migration changed::%s\n' \
		"$(escape_workflow_data "${message}")"
	failed=1
done <"${changed_migrations}"

if (( failed )); then
	echo "migration-police rejected changes to existing database migrations."
	exit 1
fi

echo "migration-police found no changes to existing database migrations."
