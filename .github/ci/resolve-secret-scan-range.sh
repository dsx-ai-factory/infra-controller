#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# A green secret scan needs a verified, non-empty range. Pull requests start at
# their merge base, while release tags start at the closest tag behind them on
# the same release line. Emit nothing unless both ends exist in this checkout so
# the scanner cannot report a clean result for an unknown range.

fail() {
	printf '::error::Could not resolve secret-scan range: %s\n' "$1" >&2
	exit 1
}

for variable_name in \
	GITHUB_REF \
	GITHUB_SHA \
	GITHUB_WORKSPACE; do
	[[ -n "${!variable_name:-}" ]] || fail "\`${variable_name}\` is not set"
done

commit_pattern='^[0-9a-fA-F]{40}$'
[[ "$GITHUB_SHA" =~ $commit_pattern ]] \
	|| fail "\`GITHUB_SHA\` is not a full commit SHA"

if [[ "$GITHUB_REF" =~ ^refs/heads/pull-request/([1-9][0-9]*)$ ]]; then
	pull_request_number="${BASH_REMATCH[1]}"
	for variable_name in GH_TOKEN GITHUB_REPOSITORY; do
		[[ -n "${!variable_name:-}" ]] || fail "\`${variable_name}\` is not set"
	done

	if ! pull_request_json=$(curl --disable --fail --silent --show-error \
		--connect-timeout 10 \
		--max-time 30 \
		-H "Authorization: Bearer ${GH_TOKEN}" \
		-H 'Accept: application/vnd.github+json' \
		-H 'X-GitHub-Api-Version: 2022-11-28' \
		"https://api.github.com/repos/${GITHUB_REPOSITORY}/pulls/${pull_request_number}"); then
		fail "could not load pull request #${pull_request_number} from GitHub"
	fi

	if ! pull_request_base=$(jq --exit-status --raw-output \
		'.base.sha | select(type == "string")' \
		<<< "$pull_request_json"); then
		fail "GitHub returned incomplete data for pull request #${pull_request_number}"
	fi

	[[ "$pull_request_base" =~ $commit_pattern ]] \
		|| fail "GitHub returned an invalid pull request base"

	if ! base_sha=$(git -C "$GITHUB_WORKSPACE" \
		merge-base "$pull_request_base" "$GITHUB_SHA"); then
		fail "could not compute a merge base for the pull request base and workflow commit"
	fi
elif [[ "$GITHUB_REF" == refs/tags/* ]] \
	&& git check-ref-format "$GITHUB_REF"; then
	if ! tag_head=$(git -C "$GITHUB_WORKSPACE" \
		rev-parse --verify "${GITHUB_REF}^{commit}" 2>/dev/null); then
		fail "could not verify release tag ${GITHUB_REF#refs/tags/}"
	fi
	[[ "${tag_head,,}" == "${GITHUB_SHA,,}" ]] \
		|| fail "the release tag does not point to this workflow's commit"

	if ! previous_tag=$(git -C "$GITHUB_WORKSPACE" describe \
		--tags \
		--match 'v[0-9]*.[0-9]*.[0-9]*' \
		--abbrev=0 \
		--first-parent \
		"${GITHUB_SHA}^" 2>/dev/null); then
		fail "could not find an earlier tag on this release line"
	fi
	if ! base_sha=$(git -C "$GITHUB_WORKSPACE" \
		rev-parse --verify "refs/tags/${previous_tag}^{commit}" 2>/dev/null); then
		fail "could not verify earlier tag ${previous_tag}"
	fi
else
	fail "\`GITHUB_REF\` is not a pull request or tag ref: ${GITHUB_REF}"
fi

[[ "${base_sha,,}" != "${GITHUB_SHA,,}" ]] \
	|| fail "the secret-scan range is empty"

printf 'base=%s\nhead=%s\n' "${base_sha,,}" "${GITHUB_SHA,,}"
