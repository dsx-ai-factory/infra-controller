#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

usage() {
	echo "Usage: check-migration-filenames.sh --base REVISION"
	echo "       check-migration-filenames.sh MIGRATION_FILE..."
}

escape_workflow_data() {
	local value="$1"

	value="${value//'%'/'%25'}"
	value="${value//$'\r'/'%0D'}"
	value="${value//$'\n'/'%0A'}"
	printf '%s' "${value}"
}

migration_files=()

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
	usage
	exit 0
elif [[ "${1:-}" == "--base" ]]; then
	if (( $# != 2 )); then
		usage >&2
		exit 2
	fi

	base_revision="$2"
	migration_list="$(mktemp)"
	trap 'rm -f "${migration_list}"' EXIT

	git diff --no-renames --diff-filter=A --name-only -z \
		"${base_revision}...HEAD" \
		-- ':(top,glob)crates/api-db/migrations/*.sql' >"${migration_list}"

	while IFS= read -r -d '' migration_file; do
		migration_files+=("${migration_file}")
	done <"${migration_list}"
elif (( $# > 0 )); then
	migration_files=("$@")
else
	usage >&2
	exit 2
fi

failed=0

for migration_file in "${migration_files[@]}"; do
	filename="${migration_file##*/}"
	timestamp="${filename%%_*}"

	if [[ ! "${filename}" =~ ^[0-9]{14}_.+\.sql$ ]]; then
		message="New Core database migration ${migration_file} must use the YYYYMMDDhhmmss_description.sql filename format."
		printf '::error title=Invalid migration filename::%s\n' \
			"$(escape_workflow_data "${message}")"
		failed=1
	elif [[ "${timestamp}" =~ ^[0-9]{10}0000$ ]]; then
		message="New Core database migration ${migration_file} must use a fully populated YYYYMMDDhhmmss timestamp; replace the trailing 0000 minute/second placeholder."
		printf '::error title=Placeholder migration timestamp::%s\n' \
			"$(escape_workflow_data "${message}")"
		failed=1
	fi
done

if (( failed )); then
	echo "migration-police rejected one or more new database migrations."
	exit 1
fi

printf 'migration-police checked %d new database migration(s).\n' "${#migration_files[@]}"
