#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Run this focused regression test from the repository root:
#
#   .github/ci/test-package-rest-helm.sh
#
# It requires helm, yq v4, tar, and sha256sum. It only writes under a temporary
# directory and does not publish an artifact or modify the source chart.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
stager="$repo_root/.github/ci/prepare-rest-umbrella-chart.sh"
source_chart="$repo_root/helm/rest/nico-rest"
temp_dir=$(mktemp -d)
trap 'rm -rf "$temp_dir"' EXIT

source_digest() {
  find "$source_chart" -type f -print0 \
    | sort -z \
    | xargs -0 sha256sum \
    | sha256sum \
    | cut -d' ' -f1
}

assert_archive() {
  local archive=$1
  local chart_version=$2
  local app_version=$3
  local extract_dir=$4

  mkdir "$extract_dir"
  tar -xzf "$archive" -C "$extract_dir"
  local packaged="$extract_dir/nico-rest"

  [[ $(yq -r '.version' "$packaged/Chart.yaml") == "$chart_version" ]]
  [[ $(yq -r '.appVersion' "$packaged/Chart.yaml") == "$app_version" ]]

  mapfile -t dependencies < <(yq -r '.dependencies[].name' "$packaged/Chart.yaml")
  [[ ${#dependencies[@]} -eq 6 ]]
  local dependency_name
  for dependency_name in "${dependencies[@]}"; do
    [[ $(NAME="$dependency_name" yq -r '.dependencies[] | select(.name == strenv(NAME)) | .version' "$packaged/Chart.yaml") == "$chart_version" ]]
    [[ $(yq -r '.name' "$packaged/charts/$dependency_name/Chart.yaml") == "$dependency_name" ]]
    [[ $(yq -r '.version' "$packaged/charts/$dependency_name/Chart.yaml") == "$chart_version" ]]
    [[ $(yq -r '.appVersion' "$packaged/charts/$dependency_name/Chart.yaml") == "$app_version" ]]
  done
}

initial_digest=$(source_digest)

# These chart/app pairs are the outputs supplied by rest-prepare-build-info.yml.
# The development case proves packaging consumes its existing last-hyphen Helm
# conversion; this script intentionally does not reimplement that conversion.
cases=(
  'exact release tag|2.2.0|v2.2.0'
  'prerelease tag|2.2.0-rc.1|v2.2.0-rc.1'
  'development git describe v2.2.0-3-gabc1234|2.2.0-3.gabc1234|v2.2.0-3-gabc1234'
)

index=0
for case in "${cases[@]}"; do
  IFS='|' read -r name chart_version app_version <<< "$case"
  stage="$temp_dir/stage-$index"
  packages="$temp_dir/packages-$index"
  "$stager" "$source_chart" "$chart_version" "$app_version" "$stage"
  mkdir "$packages"
  helm package --destination "$packages" --dependency-update --version "$chart_version" --app-version "$app_version" "$stage"
  mapfile -t archives < <(find "$packages" -maxdepth 1 -name '*.tgz' -type f)
  [[ ${#archives[@]} -eq 1 ]] || { echo "case '$name' did not produce exactly one archive" >&2; exit 1; }
  assert_archive "${archives[0]}" "$chart_version" "$app_version" "$temp_dir/extract-$index"
  [[ $(source_digest) == "$initial_digest" ]] || { echo "case '$name' modified the source chart" >&2; exit 1; }
  printf 'PASS: %s\n' "$name"
  index=$((index + 1))
done

expect_helm_validation_failure() {
  local name=$1
  local fixture=$2
  local value_overrides=(
    --set nico-rest-api.config.keycloak.enabled=true
    --set nico-rest-api.config.keycloak.baseURL=http://keycloak:8082
    --set nico-rest-api.config.keycloak.realm=test
    --set nico-rest-api.config.keycloak.clientID=test
  )

  if helm lint "$fixture" "${value_overrides[@]}" >/dev/null 2>&1 &&
    helm dependency build "$fixture" >/dev/null 2>&1; then
    echo "case '$name' unexpectedly succeeded" >&2
    exit 1
  fi
  printf 'PASS: %s\n' "$name"
}

missing_fixture="$temp_dir/missing"
cp -a "$source_chart" "$missing_fixture"
rm -rf "$missing_fixture/charts/nico-rest-db"
expect_helm_validation_failure missing-dependency "$missing_fixture"

undeclared_fixture="$temp_dir/undeclared"
cp -a "$source_chart" "$undeclared_fixture"
NAME=nico-rest-db yq -i 'del(.dependencies[] | select(.name == strenv(NAME)))' "$undeclared_fixture/Chart.yaml"
expect_helm_validation_failure undeclared-subchart "$undeclared_fixture"

name_fixture="$temp_dir/name-mismatch"
cp -a "$source_chart" "$name_fixture"
yq -i '.name = "wrong-name"' "$name_fixture/charts/nico-rest-db/Chart.yaml"
expect_helm_validation_failure name-mismatch "$name_fixture"

[[ $(source_digest) == "$initial_digest" ]] || { echo "packaging tests modified the source chart" >&2; exit 1; }
printf 'PASS: source chart remained unchanged\n'
