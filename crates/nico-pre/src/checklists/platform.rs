/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! `nico-pre-platform`: the Kubernetes cluster before setup.sh.

use crate::harness::{Check, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "platform.kubernetes.version",
        expected: "the cluster at the Kubernetes baseline",
        detail: "Reads: through the KUBECONFIG in `setup.env`: the server version, each node's \
                 kubelet and containerd versions, the CNI, ingress controller and CoreDNS images; \
                 `versions.kubernetesBaseline` and the supported-versions manifest (the \
                 quick-start baseline until the provider guide lands).\n\
                 Pass: every version within the manifest's range for the release; observed \
                 `server v1.30.4; 3 nodes kubelet v1.30.4; containerd 1.7.x`.\n\
                 Warn: drift from the baseline within a minor version while the manifest is the \
                 placeholder.\n\
                 Fail: a version outside the range; observed names the component, the version and \
                 the range; fix: upgrade.\n\
                 Skip: never.\n\
                 Test: fixture `kubectl version` and node status JSON; cases: the baseline \
                 passes; a kubelet one minor behind fails naming both; patch drift warns.",
        run: todo,
    },
    Check {
        id: "platform.nodes.ready",
        expected: "every site controller a Ready node with the expected labels",
        detail: "Reads: the nodes through the API: Ready condition, labels, taints; \
                 `hosts.siteControllers`.\n\
                 Pass: the API answers; every declared controller is a Ready node carrying the \
                 control-plane label; no extra worker; never fewer than three; observed `3 nodes \
                 Ready, control-plane`.\n\
                 Fail: a declared controller absent or NotReady; a node without the label; fewer \
                 than three; observed names the node; fix: join or repair it.\n\
                 Skip: never.\n\
                 Test: fixture node lists; cases: three Ready passes; one NotReady fails naming \
                 it; a worker present fails; two nodes fails.",
        run: todo,
    },
    Check {
        id: "platform.storage.class",
        expected: "the storage class the prerequisites name exists and provisions",
        detail: "Reads: StorageClasses and the default annotation; the decisions \
                 `custom-default-storageclass` and `custom-storage-classes-for-components`; \
                 `NICO_STORAGE_CLASS` from `setup.env`; the reference class's path and free space \
                 on each controller over SSH; `local-path` when `dpf` is `Install`.\n\
                 Pass: the reference class present with its path on every controller and the \
                 planned free space, and no other default class; or the custom class present and \
                 binding a test PVC (deleted after) with the Vault and Postgres class names \
                 resolving; `local-path` present with DPF; observed `local-path default; 3 nodes \
                 1.2 TiB free; test PVC Bound`.\n\
                 Fail: two default classes, the class missing, the test PVC not Bound within the \
                 timeout, free space below plan; fix per clause.\n\
                 Skip: the custom rows when both decisions are `No`.\n\
                 Test: a kind cluster in CI, fixtures otherwise; cases: one default class passes; \
                 two defaults fails; a provisioner that never binds fails naming Pending; DPF \
                 without `local-path` fails.",
        run: todo,
    },
    Check {
        id: "platform.metallb.ready",
        expected: "when MetalLB is already installed (a rerun or an upgrade), it is healthy and its pools cover the service VIPs",
        detail: "Reads: the MetalLB namespace, IPAddressPools and BGPPeers through the API; the \
                 service VIPs (pinned or derived) and `hosts.siteControllers`. `setup.sh` \
                 installs MetalLB itself, so a fresh cluster has none yet; this row judges a \
                 MetalLB that is already there, the file it will be fed is `config.metallb.rules`' \
                 row, and the installed result is the install checklist's.\n\
                 Pass: controller and speakers running; every VIP inside a pool; one BGPPeer per \
                 controller (or per ToR) with the layout's ASNs; observed `metallb running; 6 \
                 VIPs in pools; 3 peers`.\n\
                 Fail: controller or speakers not running, a VIP outside every pool, a peer \
                 missing or wrong; fix per clause.\n\
                 Skip: when the MetalLB namespace is absent (a fresh cluster before `setup.sh`), \
                 reported as `not installed yet; setup.sh installs it`.\n\
                 Test: fixture CRs; cases: all pass; a VIP outside fails naming it; a peer ASN \
                 wrong fails naming both; no MetalLB namespace skips.",
        run: todo,
    },
    Check {
        id: "platform.nodes.posture",
        expected: "on every node: swap off, IOMMU and NUMA on, chrony present, containerd and kubelet on the data disk, the sysctls set, cluster DNS resolving",
        detail: "Reads: over SSH on every node: `/etc/os-release`, `uname -r`, `swapon`, the \
                 kernel command line (iommu, numa), chrony present, `findmnt` for \
                 `/var/lib/containerd` and `/var/lib/kubelet` (dedicated disk, `noatime`), \
                 `sysctl net.bridge.bridge-nf-call-iptables` and `net.ipv4.ip_forward`; \
                 `kubernetes.default.svc.cluster.local` resolved from a debug pod.\n\
                 Pass: every item as required on every node; observed `3 nodes: Ubuntu 22.04, \
                 swap off, iommu on, chrony, data disk noatime, sysctls 1, DNS ok`.\n\
                 Fail: any item off; observed names the node and the item (`sc2: swap on; \
                 bridge-nf-call-iptables=0`); fix per item (`swapoff -a`, the `sysctl -w` line).\n\
                 Skip: never.\n\
                 Test: fixture command outputs fed to the parsers; cases: all good passes; swap \
                 on fails; a data directory on the root disk fails; a sysctl at 0 fails naming \
                 the key; cluster DNS failing fails.",
        run: todo,
    },
    Check {
        id: "platform.tpm.chain",
        expected: "with attestation on: the endorsement key of every site controller chains to a loaded CA",
        detail: "Reads: the `measured-boot-attestation` decision; over SSH on each controller, \
                 the TPM endorsement certificate (`tpm2_getekcertificate` or the NV index); the \
                 CAs loaded with `nico-admin-cli tpm-ca` or named in the TOML.\n\
                 Pass: each controller's EK certificate chains to a loaded CA; observed `3 \
                 controllers chain to <CA subject>`.\n\
                 Fail: no EK certificate, or a chain to no loaded CA; observed names the \
                 controller and the issuer; fix: `nico-admin-cli tpm-ca add`.\n\
                 Skip: when attestation is `Off`.\n\
                 Test: a test CA and EK certificates generated in the test; cases: chaining \
                 passes; an unknown issuer fails naming it; `Off` skips.",
        run: todo,
    },
    Check {
        id: "platform.releases.collisions",
        expected: "no cert-manager, External Secrets, Vault or postgres-operator release or CRD set that setup.sh would collide with",
        detail: "Reads: Helm release secrets and CRDs through the API; the decisions \
                 `observability`, `credential-store`, `core-postgres`.\n\
                 Pass: no cert-manager, External Secrets, Vault or postgres-operator release or \
                 CRD set that setup.sh would install again; observed `no colliding releases or \
                 CRDs`.\n\
                 Fail: such a release or CRD set present while setup.sh will install that \
                 component; observed names the release, namespace and chart version; fix: \
                 uninstall it or choose the external option.\n\
                 Skip: a component's row when its external decision is chosen (`External \
                 Postgres`, `External stack`).\n\
                 Test: fixture release secrets and CRD lists; cases: an empty cluster passes; a \
                 cert-manager release fails naming it; postgres-operator present with `External \
                 Postgres` passes.",
        run: todo,
    },
    Check {
        id: "platform.sizing.nodes",
        expected: "per-node memory and provisioner space for the host count available after the other components' requests",
        detail: "Reads: `counts.hosts`; the sizing table (a placeholder until the provider guide lands) giving \
                 memory for Postgres, Vault and the NICo services and the provisioner free space; \
                 each node's allocatable memory and the requests of existing pods through the \
                 API; free space on the provisioner path over SSH.\n\
                 Pass: on every node, allocatable minus existing requests at or above the \
                 expectation, and free space at or above the plan; observed `3 nodes: 400 GiB \
                 free of 512, plan 96 GiB; 1.1 TiB free, plan 500 GiB`.\n\
                 Warn: the table is the placeholder; under 20% spare.\n\
                 Fail: a node short; observed names the node and both numbers; fix: memory or \
                 another node.\n\
                 Skip: never.\n\
                 Test: fixture node allocatable and pod requests; cases: ample passes; one node \
                 short fails naming it; 15% spare warns.",
        run: todo,
    },
    Check {
        id: "platform.registry.nodes",
        expected: "from every node: the registry answers, is trusted by the container runtime, and the exact image references pull",
        detail: "Reads: on every node through a debug pod or SSH: `crictl pull` of the exact \
                 Core, REST, RMS and DPF references at `versions` with the pull secret from \
                 `setup.env`; containerd's registry configuration for `hosts.registryHost`.\n\
                 Pass: the registry answers from every node, its CA is in the runtime's trust, \
                 and every reference pulls; observed `3 nodes: 4 images pull`.\n\
                 Fail: a node where a pull fails (TLS, 401, 404); observed names the node, the \
                 image and the error; fix per error.\n\
                 Skip: the RMS and DPF images per their decisions.\n\
                 Test: a kind cluster with a local registry in CI, fixtures otherwise; cases: all \
                 pull passes; an untrusted CA on one node fails naming it; a missing tag fails \
                 naming the image.",
        run: todo,
    },
    Check {
        id: "platform.dpf.prerequisites",
        expected: "with DPF: the DPU cluster VIP free, keepalived interface present, no VRRP router id 126 on the segment, the API endpoint the DPUs use answers",
        detail: "Reads: the `dpf` decision; `NICO_DPF_DPU_CLUSTER_VIP` and \
                 `NICO_DPF_DPU_INTERFACE` from `setup.env`; ARP and ping for the VIP, the \
                 interface on each controller over SSH, a short capture for VRRP router id 126 on \
                 the segment, the Kubernetes API endpoint the DPUs use from a DPU-network vantage \
                 point.\n\
                 Pass: the VIP unanswered (free), the interface present on every controller, no \
                 VRRP router id 126 seen, the API endpoint answers; observed `vip 10.10.126.200 \
                 free; ens1f0np0 on 3 nodes; no VRRP 126; api answers from the DPU segment`.\n\
                 Fail: the VIP answers (taken), the interface missing on a node, a foreign VRRP \
                 126 instance (by source address), the endpoint unreachable; fix per clause.\n\
                 Skip: when `dpf` is `Skip`.\n\
                 Test: fixtures for ARP, interface lists and a capture; cases: all clear passes; \
                 the VIP in use fails; the interface missing on one node fails naming it; a VRRP \
                 packet with id 126 fails naming its source; `Skip` skips.",
        run: todo,
    },
    Check {
        id: "platform.rms.api",
        expected: "with external RMS: [rms] api_url answers over TLS from the cluster",
        detail: "Reads: the `rms` decision; `[rms] api_url` from the TOML; a TLS handshake and \
                 GET from a debug pod with the cluster's trust.\n\
                 Pass: the URL answers 200 or 401 over trusted TLS; observed \
                 `https://rms.example: TLS ok, 200`.\n\
                 Fail: unreachable, an untrusted chain, or 5xx; fix: the URL, the CA or the RMS.\n\
                 Skip: unless `rms` is `External RMS`.\n\
                 Test: a local TLS server; cases: trusted 200 passes; an untrusted chain fails \
                 naming the issuer; refused fails; `In-cluster nv-rms` skips.",
        run: todo,
    },
    Check {
        id: "platform.nvlink.secret",
        expected: "with NVLink: the NVSwitch TLS client certificate Secret or issuer exists",
        detail: "Reads: `optional-components` for `NVLink partitioning`; the Secret, or the \
                 cert-manager Certificate and Issuer, that the TOML's NVSwitch client certificate \
                 settings name, through the API.\n\
                 Pass: the Secret exists with `tls.crt` and `tls.key` (or the issuer is Ready); \
                 observed `secret nvswitch-client in nico: present`.\n\
                 Fail: absent, or a key missing; fix: create it or the Certificate.\n\
                 Skip: when `NVLink partitioning` is unticked.\n\
                 Test: fixture Secrets; cases: present passes; `tls.key` missing fails naming it; \
                 absent fails; unticked skips.",
        run: todo,
    },
];
