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

//! `nico-pre-stage`: the staging host can run setup.sh and reach every site controller and BMC.

use crate::harness::{Check, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "stage.tools.versions",
        expected: "kubectl, helm and the other setup.sh tools at the tested versions",
        detail: "Reads: the staging host's PATH: `kubectl` 1.26+, `helm` 3.12+, `helmfile` 0.162+ \
                 with the `helm-diff` plugin, `jq` 1.6+, `ssh-keygen`, `envsubst`, `python3` with \
                 PyYAML; the `staging-host` decision.\n\
                 Pass: every tool present at or above its minimum; observed the versions found.\n\
                 Fail: a tool missing or below its minimum; `helmfile` missing while air-gapped \
                 (setup.sh cannot fetch it); observed names the tool and version; fix: the \
                 install hint.\n\
                 Skip: never.\n\
                 Test: a fixture PATH of stubs; cases: all pass; `helm` 3.11 fails naming the \
                 minimum; `helm-diff` missing fails; a `python3` whose PyYAML import fails fails.",
        run: todo,
    },
    Check {
        id: "stage.registry.pull",
        expected: "the registry answers and the images pull",
        detail: "Reads: `hosts.registryHost`; `REGISTRY_PULL_USERNAME` and the pull secret by \
                 source from `setup.env`; the Core and REST references at `versions`; the BFB and \
                 BF4 ISO URLs when the registry serves them.\n\
                 Pass: the registry answers `/v2/`, its certificate chains to the host trust \
                 store or `caSource`, the two images pull (manifest and one layer), the boot \
                 artifacts answer with the expected sizes; observed `registry.site1: TLS trusted, \
                 2 images pull`.\n\
                 Fail: TLS untrusted, 401 with the pull secret, a missing image, an artifact of \
                 the wrong size; observed names the step; fix per step.\n\
                 Skip: when `registry-host` is `None`.\n\
                 Test: a local registry with a self-signed certificate; cases: CA trusted passes; \
                 CA absent fails naming trust; a wrong secret fails with 401; a missing image \
                 fails by name.",
        run: todo,
    },
    Check {
        id: "stage.reach.bmcs",
        expected: "every site controller BMC answers from the staging host",
        detail: "Reads: `hosts.siteControllers[].bmc` (and `dpuBmc` with a DPU); \
                 `credentials.bmcRoot` by source.\n\
                 Pass: `GET /redfish/v1/Systems` answers 200 on every BMC with the credentials \
                 within the timeout; observed `3 of 3 BMCs answer; 3 of 3 DPU BMCs answer`.\n\
                 Fail: a BMC not answering, 401, or TLS refusing; observed per BMC with address \
                 and status; fix: the network path or the credential.\n\
                 Skip: never.\n\
                 Test: bmc-mock instances; cases: all answer passes; one address unreachable \
                 fails naming it; 401 fails naming the credential.",
        run: todo,
    },
    Check {
        id: "stage.egress.endpoints",
        expected: "the deployment endpoints the decisions require are reachable",
        detail: "Reads: the decisions `staging-host`, `external-registry-access`, `dpf`, \
                 `optional-components`; the endpoints they imply (`nvcr.io`, the chart \
                 repositories setup.sh adds, the DPF repository).\n\
                 Pass: every required endpoint completes a TLS handshake and an HTTP HEAD within \
                 the timeout, directly or through the proxy the decision names; observed `4 \
                 endpoints reachable`.\n\
                 Fail: an endpoint unreachable while the connectivity mode says it should be; \
                 observed per endpoint with the error; fix: the egress rule or the proxy.\n\
                 Skip: when `staging-host` is `Air-gapped with a transfer path` or `Strictly \
                 air-gapped`.\n\
                 Test: the endpoint list derived from the decisions, against a local stub \
                 resolver and HTTP server; cases: `Connected` with all answering passes; one \
                 refused fails by name; air-gapped skips; `Proxied` goes through the proxy \
                 variable.",
        run: todo,
    },
    Check {
        id: "stage.checkouts.state",
        expected: "the infra-controller checkout at the release tag with nv-rms populated at the recorded commit and doca-platform at its pin",
        detail: "Reads: the infra-controller checkout (`helm-prereqs/` present, tag), the nv-rms \
                 submodule (populated, clean, at the recorded commit, or `NICO_RMS_CHART` parsing \
                 as a chart at `versions.nvRms`), the doca-platform clone at \
                 `versions.docaPlatform` with its `.git`; `versions`; the decisions `rms`, \
                 `rms-chart-source`, `dpf`.\n\
                 Pass: all three as stated; observed `infra-controller v2026.10.15; nv-rms at \
                 <sha>; doca-platform v26.4.0`.\n\
                 Warn: a dirty working tree.\n\
                 Fail: a checkout at another tag, the submodule empty or at another commit, the \
                 DPF clone missing; fix: `git checkout` or `git submodule update --init`.\n\
                 Skip: the nv-rms row when `rms` is `Not applicable`; the doca-platform row when \
                 `dpf` is `Skip`.\n\
                 Test: temporary git repositories with a submodule; cases: all pass; an empty \
                 submodule fails naming it; `NICO_RMS_CHART` pointing at a chart of the right \
                 version passes; a wrong doca tag fails naming both.",
        run: todo,
    },
    Check {
        id: "stage.charts.reachable",
        expected: "the prerequisite chart repositories answer from upstream, the mirror or the pre-staged archives per the connectivity mode",
        detail: "Reads: the chart repositories setup.sh and helmfile add (cert-manager, \
                 external-secrets, Vault, postgres-operator, MetalLB, the NICo OCI charts); the \
                 `staging-host` decision; the mirror or archive directory when air-gapped.\n\
                 Pass: `Connected`: each repository index answers; with a transfer path: the \
                 mirror answers; strictly air-gapped: every chart `versions` names present as an \
                 archive at its version; observed `6 repositories answer` or `6 archives \
                 present`.\n\
                 Fail: a repository or archive missing; observed by name and version; fix: mirror \
                 or stage it.\n\
                 Skip: never; the connectivity mode selects which form runs.\n\
                 Test: a local index stub plus an archive directory; cases: `Connected` with all \
                 answering passes; one index 404 fails; air-gapped with one archive missing fails \
                 by name; an archive at another version fails naming both.",
        run: todo,
    },
    Check {
        id: "stage.reach.ssh",
        expected: "SSH to every site controller with the named credentials",
        detail: "Reads: `hosts.siteControllers`, `hosts.stagingHost.sshUser`, the SSH key or \
                 agent; each controller's OS address from the control-plane prefix.\n\
                 Pass: an SSH session runs `true` on every controller within the timeout; \
                 observed `3 of 3 controllers reachable as nico`.\n\
                 Fail: a controller refusing, timing out or rejecting the key; observed per host; \
                 fix: the key or the path.\n\
                 Skip: never. A refused connection does not prove an absent OS (sshd may be off), \
                 so every refusal is reported; before the OS is installed the provider runs the \
                 host checklist, not this one.\n\
                 Test: a local sshd container; cases: key accepted passes; a wrong key fails \
                 naming auth; an unreachable host fails naming the timeout; all refused fails \
                 naming every host.",
        run: todo,
    },
];
