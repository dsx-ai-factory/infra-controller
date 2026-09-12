#!/usr/bin/env bash

# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"

kubectl kustomize "${repo_root}/deploy/nico-base/pxe" \
  | kubectl patch --local -f - --type=merge --patch '{}' -o json \
  | jq --exit-status --slurp '
      [.[] | select(.kind == "Deployment" and .metadata.name == "nico-pxe")] as $deployments
      | ($deployments | length) == 1
        and ($deployments[0].spec.template.spec as $pod
          | [$pod.containers[] | select(.name == "nico-pxe")] as $main
          | ($main | length) == 1
            and ($pod.securityContext.fsGroup == 10001)
            and ($main[0].args == ["-s", "/forge-boot-artifacts"])
            and ($main[0].securityContext.runAsNonRoot == true)
            and ($main[0].securityContext.runAsUser == 10001)
            and ($main[0].securityContext.runAsGroup == 10001)
            and ([$pod.volumes[] | select(.name == "config" and .configMap.name == "nico-pxe-config")] | length) == 1
            and ([$pod.volumes[] | select(.name == "boot-artifacts" and .emptyDir == {})] | length) == 1
            and ([$main[0].volumeMounts[] | select(.name == "boot-artifacts" and .mountPath == "/forge-boot-artifacts/blobs/internal")] | length) == 1
            and (all($pod.containers[].volumeMounts[]?; .name as $name | any($pod.volumes[]; .name == $name)))
            and ($pod.containers | length) == 1)
    ' >/dev/null

kubectl kustomize "${repo_root}/deploy/tests/pxe-with-boot-artifacts" \
  | kubectl patch --local -f - --type=merge --patch '{}' -o json \
  | jq --exit-status --slurp '
      [.[] | select(.kind == "Deployment" and .metadata.name == "nico-pxe")][0].spec.template.spec as $pxe
      | [.[] | select(.kind == "Deployment" and .metadata.name == "nico-api")][0].spec.template.spec as $api
      | [$pxe.containers[] | select(.name == "nico-pxe")][0] as $pxe_main
      | [$api.containers[] | select(.name == "nico-api")][0] as $api_main
      | [$pxe.containers[] | select(.name != "nico-pxe")] as $pxe_artifacts
      | [$api.containers[] | select(.name != "nico-api")] as $api_artifacts
      | ([
          "boot-artifacts-aarch64",
          "boot-artifacts-x86-64",
          "legacy-boot-artifacts-x86-64",
          "machine-validation-artifacts-config"
        ] | sort) as $artifact_names
      | ($pxe.securityContext.fsGroup == 10001)
        and ($pxe_main.args == ["-s", "/forge-boot-artifacts"])
        and ($pxe_main.securityContext.runAsNonRoot == true)
        and ($pxe_main.securityContext.runAsUser == 10001)
        and ($pxe_main.securityContext.runAsGroup == 10001)
        and ([$pxe.volumes[] | select(.name == "boot-artifacts" and .emptyDir == {})] | length) == 1
        and ([$pxe_main.volumeMounts[] | select(.name == "boot-artifacts" and .mountPath == "/forge-boot-artifacts/blobs/internal")] | length) == 1
        and ([$pxe_artifacts[].name] | sort) == $artifact_names
        and ([$api_artifacts[].name] | sort) == $artifact_names
        and (all($pxe_artifacts[]; . as $artifact | ([$artifact.volumeMounts[] | select(.name == "boot-artifacts" and .mountPath == "/forge-boot-artifacts/blobs/internal")] | length) == 1))
        and (all($api_artifacts[]; . as $artifact | ([$artifact.volumeMounts[] | select(.name == "boot-artifacts" and .mountPath == "/forge-boot-artifacts/blobs/internal")] | length) == 1))
        and (all($pxe_artifacts[], $api_artifacts[]; (.securityContext.runAsUser? == null) and (.securityContext.runAsNonRoot? == null)))
        and ([$api.volumes[] | select(.name == "boot-artifacts" and .emptyDir == {})] | length) == 1
        and ([$api_main.volumeMounts[] | select(.name == "boot-artifacts" and .mountPath == "/forge-boot-artifacts/blobs/internal")] | length) == 1
        and (all($pxe.containers[].volumeMounts[]?; .name as $name | any($pxe.volumes[]; .name == $name)))
        and (all($api.containers[].volumeMounts[]?; .name as $name | any($api.volumes[]; .name == $name)))
    ' >/dev/null
