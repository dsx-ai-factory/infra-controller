#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Generate a CycloneDX source SBOM from a Git revision (HEAD by default).
#
# The SBOM restates the dependency lockfiles (Cargo.lock, go.mod files, uv.lock)
# with license data. It does not describe built artifacts.
#
# Requires git, syft, jq and a Rust toolchain (cargo).
#
# Requires network access; `cargo metadata` queries crates.io for uncached
# dependencies, and Syft queries Go module licenses from proxy.golang.org.
#
# Syft caches data in `${SYFT_CACHE_DIR-$HOME/.cache/syft}`, and `cargo
# metadata` will cache data in `${CARGO_HOME:-$HOME/.cargo}/registry/` and
# `${CARGO_HOME:-$HOME/.cargo}/git/`.

set -euo pipefail

usage() {
	echo "Usage: generate-source-sbom.sh [--revision REVISION] [--version VERSION] [--output FILE] [--repo-root PATH]"
	echo "Default revision: HEAD in the selected repository."
	echo "Default root component version: output of 'git describe' on the selected commit."
	echo "Default output: 'nico-<version>.cyclonedx.json'"
}

revision="HEAD"
version=""
output=""
# The repository comes from REPO_ROOT unless --repo-root overrides it.
repo="${REPO_ROOT:-}"

while (( $# )); do
	case "$1" in
		-h|--help)
			usage
			exit 0
			;;
		--revision|--version|--output|--repo-root)
			if (( $# < 2 )) || [[ -z "$2" ]]; then
				printf 'Missing value for %s\n' "$1" >&2
				usage >&2
				exit 2
			fi
			case "$1" in
				--revision) revision="$2" ;;
				--version) version="$2" ;;
				--output) output="$2" ;;
				--repo-root) repo="$2" ;;
			esac
			shift 2
			;;
		*)
			printf 'Unknown argument: %s\n' "$1" >&2
			usage >&2
			exit 2
			;;
	esac
done

if [[ -z "$repo" ]]; then
	echo 'REPO_ROOT is not set; set it or pass --repo-root PATH' >&2
	exit 2
fi

# Resolve to a full commit ID before doing any work, so a bad revision fails immediately.
if ! commit="$(git -C "$repo" rev-parse --verify --quiet --end-of-options "${revision}^{commit}")"; then
	printf 'Revision does not resolve to a commit: %s\n' "$revision" >&2
	exit 1
fi

# Use 'git describe' to determine the version unless was manually specified
# with '--version'.
if [[ -z "$version" ]]; then
	version="$(git -C "$repo" describe --tags --first-parent --always "$commit")"
fi
repo="$(cd -P "$(git -C "$repo" rev-parse --show-toplevel)" && pwd)"

if [[ -z "$output" ]]; then
	if [[ "$version" == */* ]]; then
		echo 'Version contains /; specify --output FILE' >&2
		exit 2
	fi
	output="nico-${version}.cyclonedx.json"
fi

if [[ -L "$output" || ( -e "$output" && ! -f "$output" ) ]]; then
	echo 'Output file already existed but was not a normal file' >&2
	exit 2
fi

temp="$(mktemp -d)"
# Resolve the temp directory to its real location; Syft behaves badly if we
# don't do this.
temp="$(realpath "$temp")"
trap 'rm -rf "$temp"' EXIT

# We'll work from a 'git archive' export of the repository so we don't have to
# worry about untracked/uncommitted/ignored files.
mkdir "$temp/source"
git -C "$repo" archive "$commit" | tar -x -C "$temp/source"

# lints/ holds a separate lint-plugin workspace and a test fixture, each with
# its own Cargo.lock. They do not ship, so they are excluded. --enrich golang
# fetches Go module licenses over the network.
SYFT_CHECK_FOR_APP_UPDATE=false syft scan "dir:$temp/source" \
	--source-name NICo \
	--source-version "$version" \
	--exclude './lints/**' \
	--enrich golang \
	-o "cyclonedx-json=$temp/syft.json"

# Filter out anything that didn't come from a lockfile (e.g. GitHub Actions
# references, random executables, etc).
jq '
	def from_lockfile:
		any(.properties[]?; .name == "syft:package:foundBy" and
			(.value == "rust-cargo-lock-cataloger" or
			 .value == "go-module-file-cataloger" or
			 .value == "python-package-cataloger"));
	.components |= map(
		select(from_lockfile and (.purl // "" | test("^pkg:(cargo|golang|pypi)/"))
			and .version != "UNKNOWN" and .name != "machine-lifecycle-test")
	)
' "$temp/syft.json" >"$temp/filtered.json"

# If Syft couldn't retrieve the Go licenses because of a network issue, it
# will have proceeded anyway, which could give us an SBOM missing a lot of
# information. Check that we have at least one; it's difficult to be more strict
# than this because some of the Go modules don't store their licenses in a
# standard way.
if ! jq -e '
	[.components[] | select((.purl // "" | startswith("pkg:golang/")))] as $go_components
	| ($go_components | length == 0) or any($go_components[]; (.licenses // [] | length) > 0)
' "$temp/filtered.json" >/dev/null; then
	echo 'Go components have no license data; Syft enrichment may have failed' >&2
	exit 1
fi

# Syft reads Cargo.lock, which carries no license data, so we extract the Rust
# crate licenses using 'cargo metadata'. Run it from the exported source so
# rustup uses that revision's toolchain.
pushd "$temp/source" >/dev/null
cargo metadata --format-version 1 --all-features --locked >"$temp/cargo.json"
cargo_version="$(cargo --version)"
popd >/dev/null

# Add Rust crate licenses to the components retained from Syft.
jq --slurpfile cargo "$temp/cargo.json" '
	# Third-party crates by name@version. First-party workspace crates have no source, so they
	# are left out. Keying on name and version avoids purl encoding differences (a + in a
	# version appears as %2B in a purl).
	INDEX($cargo[0].packages[] | select(.source != null); .name + "@" + .version) as $crates
	| .components |= map(
		# Copy each crate license across, rewriting the deprecated "A/B" form as "A OR B".
		if (.purl // "" | startswith("pkg:cargo/")) then
			($crates[.name + "@" + .version].license) as $license
			| if $license then .licenses = [{expression: ($license | gsub(" */ *"; " OR "))}] else . end
		  else . end
	)
' "$temp/filtered.json" >"$temp/licensed.json"

# Remove dependency entries and links that point at components dropped above.
jq '
	(.components | map(."bom-ref") | INDEX(.)) as $refs
	| .dependencies |= map(select($refs[.ref]) | .dependsOn |= map(select($refs[.])))
' "$temp/licensed.json" >"$temp/linked.json"

jq --arg cargo_version "$cargo_version" '
	.metadata.component.type = "application"
	| .metadata.component.supplier = {name: "NVIDIA"}
	| .metadata.tools.components += [
		{type: "application", name: "cargo", version: ($cargo_version | split(" ")[1])},
		{type: "application", name: "generate-source-sbom.sh"}
	]
' "$temp/linked.json" >"$temp/output.json"
mv "$temp/output.json" "$output"
printf 'Wrote %s\n' "$output"
