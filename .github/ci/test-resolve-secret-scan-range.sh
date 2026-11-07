#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
resolver="${script_dir}/resolve-secret-scan-range.sh"
fixture_dir=$(mktemp -d)
trap 'rm -rf -- "$fixture_dir"' EXIT

repository="${fixture_dir}/repository"
first_tag_repository="${fixture_dir}/first-tag-repository"
mock_bin="${fixture_dir}/bin"
mkdir -p "$repository" "$first_tag_repository" "$mock_bin"

cat > "${mock_bin}/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

[[ "${MOCK_CURL_FAIL:-false}" != true ]] || exit 22
[[ "$*" == *'/repos/NVIDIA/infra-controller/pulls/4786'* ]] || exit 64
printf '%s' "$MOCK_PULL_REQUEST_JSON"
EOF
chmod +x "${mock_bin}/curl"

git -C "$repository" init --quiet --initial-branch=main
git -C "$repository" config user.email ci-test@nvidia.com
git -C "$repository" config user.name 'CI Test'
git -C "$repository" config commit.gpgsign false
git -C "$repository" config core.hooksPath /dev/null

printf 'shared\n' > "${repository}/shared.txt"
git -C "$repository" add shared.txt
git -C "$repository" commit --quiet -m 'shared base'
merge_base=$(git -C "$repository" rev-parse HEAD)

git -C "$repository" switch --quiet -c pull-request
printf 'pull request\n' > "${repository}/pull-request.txt"
git -C "$repository" add pull-request.txt
git -C "$repository" commit --quiet -m 'pull request change'
head_sha=$(git -C "$repository" rev-parse HEAD)

git -C "$repository" switch --quiet main
printf 'main\n' > "${repository}/main.txt"
git -C "$repository" add main.txt
git -C "$repository" commit --quiet -m 'main change'
base_sha=$(git -C "$repository" rev-parse HEAD)
git -C "$repository" switch --quiet pull-request

git -C "$repository" tag v1.0.0 "$base_sha"
git -C "$repository" switch --quiet -c release-side "$base_sha"
printf 'side branch\n' > "${repository}/side-branch.txt"
git -C "$repository" add side-branch.txt
git -C "$repository" commit --quiet -m 'side branch change'
git -C "$repository" tag --annotate v99.0.0 --message 'side branch tag'

git -C "$repository" switch --quiet main
git -C "$repository" merge --quiet --no-ff release-side -m 'merge side branch'
git -C "$repository" tag spi-internal-build
printf 'release\n' > "${repository}/release.txt"
git -C "$repository" add release.txt
git -C "$repository" commit --quiet -m 'release'
release_head=$(git -C "$repository" rev-parse HEAD)
git -C "$repository" tag v1.1.0 "$release_head"
git -C "$repository" tag v1.1.0-alias "$release_head"

git -C "$first_tag_repository" init --quiet --initial-branch=main
git -C "$first_tag_repository" config user.email ci-test@nvidia.com
git -C "$first_tag_repository" config user.name 'CI Test'
git -C "$first_tag_repository" config commit.gpgsign false
git -C "$first_tag_repository" config core.hooksPath /dev/null
printf 'history\n' > "${first_tag_repository}/history.txt"
git -C "$first_tag_repository" add history.txt
git -C "$first_tag_repository" commit --quiet -m 'history before first release'
printf 'first release\n' > "${first_tag_repository}/release.txt"
git -C "$first_tag_repository" add release.txt
git -C "$first_tag_repository" commit --quiet -m 'first release'
first_tag_head=$(git -C "$first_tag_repository" rev-parse HEAD)
git -C "$first_tag_repository" tag v0.1.0

missing_sha=ffffffffffffffffffffffffffffffffffffffff

pull_request_json() {
	local base=$1

	jq --null-input --compact-output \
		--arg base "$base" \
		'{base: {sha: $base}}'
}

run_resolver() {
	local payload=$1
	local ref=$2
	local event_head=$3
	local curl_fail=${4:-false}
	local workspace=${5:-$repository}

	PATH="${mock_bin}:${PATH}" \
	GH_TOKEN='not-a-real-token' \
	GITHUB_REF="$ref" \
	GITHUB_REPOSITORY='NVIDIA/infra-controller' \
	GITHUB_SHA="$event_head" \
	GITHUB_WORKSPACE="$workspace" \
	MOCK_PULL_REQUEST_JSON="$payload" \
	MOCK_CURL_FAIL="$curl_fail" \
		bash "$resolver"
}

expect_failure() {
	local name=$1
	local expected_error=$2
	local payload=$3
	local ref=$4
	local event_head=$5
	local curl_fail=${6:-false}
	local workspace=${7:-$repository}
	local output

	if output=$(run_resolver "$payload" "$ref" "$event_head" "$curl_fail" "$workspace" \
		2> "${fixture_dir}/error"); then
		printf 'Expected failure for %s\n' "$name" >&2
		exit 1
	fi
	[[ -z "$output" ]] || {
		printf 'Failure %s wrote step outputs: %s\n' "$name" "$output" >&2
		exit 1
	}
	if ! grep -Fq "::error::Could not resolve secret-scan range: ${expected_error}" \
		"${fixture_dir}/error"; then
		printf 'Failure %s did not report the expected error: %s\nActual error:\n' \
			"$name" "$expected_error" >&2
		cat "${fixture_dir}/error" >&2
		exit 1
	fi
}

valid_payload=$(pull_request_json "$base_sha")
expected_output=$(printf 'base=%s\nhead=%s' "$merge_base" "$head_sha")
actual_output=$(run_resolver \
	"$valid_payload" \
	refs/heads/pull-request/4786 \
	"$head_sha")
[[ "$actual_output" == "$expected_output" ]] || {
	printf 'Expected:\n%s\nActual:\n%s\n' "$expected_output" "$actual_output" >&2
	exit 1
}

expected_tag_output=$(printf 'base=%s\nhead=%s' "$base_sha" "$release_head")
actual_tag_output=$(run_resolver \
	'{}' \
	refs/tags/v1.1.0-alias \
	"$release_head" \
	true)
[[ "$actual_tag_output" == "$expected_tag_output" ]] || {
	printf 'Expected tag range:\n%s\nActual tag range:\n%s\n' \
		"$expected_tag_output" "$actual_tag_output" >&2
	exit 1
}

expect_failure \
	'malformed PR ref' \
	'`GITHUB_REF` is not a pull request or tag ref' \
	"$valid_payload" \
	refs/heads/main \
	"$head_sha"
expect_failure \
	'malformed tag ref' \
	'`GITHUB_REF` is not a pull request or tag ref' \
	'{}' \
	refs/tags/ \
	"$release_head"
expect_failure \
	'invalid workflow commit' \
	'`GITHUB_SHA` is not a full commit SHA' \
	"$valid_payload" \
	refs/heads/pull-request/4786 \
	invalid
expect_failure \
	'missing base commit' \
	'could not compute a merge base for the pull request base and workflow commit' \
	"$(pull_request_json "$missing_sha")" \
	refs/heads/pull-request/4786 \
	"$head_sha"
expect_failure \
	'invalid API base' \
	'GitHub returned an invalid pull request base' \
	"$(pull_request_json invalid)" \
	refs/heads/pull-request/4786 \
	"$head_sha"
expect_failure \
	'empty scan range' \
	'the secret-scan range is empty' \
	"$(pull_request_json "$head_sha")" \
	refs/heads/pull-request/4786 \
	"$head_sha"
expect_failure \
	'incomplete API response' \
	'GitHub returned incomplete data for pull request #4786' \
	'{}' \
	refs/heads/pull-request/4786 \
	"$head_sha"
expect_failure \
	'API request failure' \
	'could not load pull request #4786 from GitHub' \
	"$valid_payload" \
	refs/heads/pull-request/4786 \
	"$head_sha" \
	true

expect_failure \
	'missing release tag' \
	'could not verify release tag missing' \
	'{}' \
	refs/tags/missing \
	"$release_head"
expect_failure \
	'tag commit mismatch' \
	"the release tag does not point to this workflow's commit" \
	'{}' \
	refs/tags/v1.1.0 \
	"$base_sha"
expect_failure \
	'first release tag' \
	'could not find an earlier tag on this release line' \
	'{}' \
	refs/tags/v0.1.0 \
	"$first_tag_head" \
	false \
	"$first_tag_repository"

printf 'Checked the secret-scan range resolver.\n'
