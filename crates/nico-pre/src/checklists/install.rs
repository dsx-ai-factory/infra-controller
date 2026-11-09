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

//! `nico-pre-install`: NICo as a system after setup.sh, nothing ingested.

use crate::harness::{Check, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "install.pods.ready",
        expected: "every NICo component the decisions tick is running",
        detail: "Reads: the NICo namespaces through the API; the component list from the \
                 decisions (`optional-components`, `dpf`, `rms`, `observability`, \
                 `rest-api-auth`): nico-api, dhcp, dns, pxe, ssh-console, site-agent, REST, \
                 Keycloak, Temporal, RMS, the DPF operator, observability.\n\
                 Pass: every expected Deployment, StatefulSet or DaemonSet has its desired \
                 replicas Ready and no pod is in CrashLoopBackOff; observed `14 workloads ready`.\n\
                 Fail: a workload short of replicas or crash-looping; observed names it with \
                 ready/desired and the last restart reason; fix: the `kubectl logs` hint.\n\
                 Skip: never; a component the decisions do not tick is not expected.\n\
                 Test: fixture workload lists; cases: all ready passes; a Deployment at 0/1 fails \
                 naming it; a pod in CrashLoopBackOff fails; REST unticked with no REST pods \
                 passes.",
        run: todo,
    },
    Check {
        id: "install.api.reachable",
        expected: "the Core API answers on apiHostname with a certificate the site CA signed",
        detail: "Reads: `site.apiHostname` and the API VIP; a DNS lookup, a TLS handshake and a \
                 health call; the site CA from the cluster (the cert-manager site root Secret) or \
                 the supplied `root-ca`.\n\
                 Pass: the name resolves to the API VIP, the certificate chains to the site CA \
                 and lists `apiHostname` among its names, the health call answers; observed \
                 `api.site1 -> 10.10.126.193; cert by <site CA>, SAN ok; healthy`.\n\
                 Fail: resolution elsewhere, a chain to another CA, a SAN mismatch, no health; \
                 observed names the address resolved and the certificate's names; fix per clause.\n\
                 Skip: never.\n\
                 Test: a local TLS server with a test CA and a stub DNS; cases: matching passes; \
                 `apiHostname` resolving elsewhere fails naming both addresses; a certificate \
                 without the SAN fails naming its names; another CA fails.",
        run: todo,
    },
    Check {
        id: "install.dpf.ready",
        expected: "the DPF operator and its DPUCluster are ready when DPF is installed",
        detail: "Reads: the `dpf` decision; the DPF operator Deployment and the DPUCluster \
                 resource's conditions through the API.\n\
                 Pass: the operator Ready and the DPUCluster `Ready=True`; observed `dpf-operator \
                 ready; dpucluster <name> Ready`.\n\
                 Fail: the operator not ready, the DPUCluster absent, or a condition False \
                 (quoted with its reason); fix: the DPF logs hint.\n\
                 Skip: when `dpf` is `Skip`.\n\
                 Test: fixture resources; cases: Ready passes; a condition False fails quoting \
                 the reason; absent fails; `Skip` skips.",
        run: todo,
    },
];
