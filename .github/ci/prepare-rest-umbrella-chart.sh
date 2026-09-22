#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: prepare-rest-umbrella-chart.sh CHART CHART_VERSION APP_VERSION PREPARED_CHART_DIR

Prepare the NICo REST umbrella chart and align every bundled chart and dependency
to the supplied versions. CHART is never modified.
PREPARED_CHART_DIR must not already exist.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

# Reject incomplete invocations before touching the filesystem.
if [[ $# -ne 4 ]]; then
  usage >&2
  exit 2
fi

chart=$1
chart_version=$2
app_version=$3
prepared_chart_dir=$4

# Validate the tool, output location, and required dynamic version inputs.
command -v yq >/dev/null 2>&1 || die "yq v4 is required"
[[ ! -e "$prepared_chart_dir" ]] || die "prepared chart directory already exists: $prepared_chart_dir"
[[ -n "$chart_version" ]] || die "chart version must not be empty"
[[ -n "$app_version" ]] || die "app version must not be empty"

# Helm accepts some non-strict forms, so enforce the workflow's strict SemVer
# input contract before writing it into every chart.
semver_number='(0|[1-9][0-9]*)'
semver_identifier='(0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)'
semver_prerelease="(${semver_identifier})(\\.${semver_identifier})*"
semver_build='[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*'
semver="^${semver_number}\\.${semver_number}\\.${semver_number}(-${semver_prerelease})?(\\+${semver_build})?$"
[[ "$chart_version" =~ $semver ]] || die "chart version is not strict SemVer: $chart_version"

# Work only on a disposable copy so CI never modifies the checkout.
mkdir -p "$(dirname "$prepared_chart_dir")"
cp -a "$chart" "$prepared_chart_dir"

root_chart="$prepared_chart_dir/Chart.yaml"

# Collect the validated bundled charts that need their metadata rewritten.
shopt -s nullglob
subchart_files=("$prepared_chart_dir"/charts/*/Chart.yaml)
shopt -u nullglob

# Align the prepared umbrella, dependency declarations, and bundled subcharts.
export CHART_VERSION="$chart_version"
export APP_VERSION="$app_version"
yq -i '.version = strenv(CHART_VERSION) | .appVersion = strenv(APP_VERSION) | (.dependencies[].version = strenv(CHART_VERSION))' "$root_chart"

for subchart_file in "${subchart_files[@]}"; do
  yq -i '.version = strenv(CHART_VERSION) | .appVersion = strenv(APP_VERSION)' "$subchart_file"
done

printf 'Prepared chart: %s\n' "$prepared_chart_dir"
