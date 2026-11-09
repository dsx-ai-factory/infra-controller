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

//! `nico-pre-build`: the build host can produce the ISO and the images.

use crate::harness::{Check, todo};

pub(crate) const CHECKS: &[Check] = &[
    Check {
        id: "build.tools.versions",
        expected: "the toolchain the builds need at the tested versions",
        detail: "Reads: the build host's PATH: `yq`, `gomplate`, `wget`, `docker`, `curl`, `jq`, \
                 `xxd`, `zip`, `gzip`, a sha256 tool, `mkisofs` or `xorrisofs`; with images built \
                 locally, `rustc` and `cargo` against `rust-toolchain.toml`, `go`, `cargo-make`; \
                 free space in the work directory against the artifact sizes; the `build-host` \
                 decision.\n\
                 Pass: every tool present at or above the tested version and of the right flavour \
                 (`yq` is mikefarah v4, not the Debian wrapper); free space above the artifact \
                 total; observed `11 tools; yq v4.44; 120 GiB free`.\n\
                 Warn: a tool above the tested version.\n\
                 Fail: a tool missing, below the minimum or of the wrong flavour; observed names \
                 the tool and its version string; fix: the install hint for that tool.\n\
                 Skip: when `build-host` is `No`.\n\
                 Test: the version probes against a fixture PATH of stub scripts printing version \
                 strings; cases: all stubs pass; `gomplate` absent fails by name; a stub printing \
                 the Debian `yq` banner fails the flavour; free space below the total fails \
                 naming both sizes.",
        run: todo,
    },
    Check {
        id: "build.checkouts.tags",
        expected: "checkouts at the tags in versions",
        detail: "Reads: the infra-controller, nv-rms and doca-platform checkouts (from the tool's \
                 arguments or the current directory); `git describe --tags --exact-match` and \
                 `git status --porcelain` through the git CLI; `versions`.\n\
                 Pass: each checkout at the tag `versions` names, clean; observed \
                 `infra-controller v2026.10.15, nv-rms v0.8.0, doca-platform v26.4.0`.\n\
                 Warn: a clean checkout at the right commit under another tag name; local \
                 modifications.\n\
                 Fail: a checkout at another tag, or absent; observed `nv-rms: at v0.7.9, \
                 versions says v0.8.0`; fix: `git checkout <tag>`.\n\
                 Skip: when `build-host` is `No`.\n\
                 Test: temporary git repositories with tags created in the test; cases: all at \
                 tag passes; one behind fails naming both tags; a dirty tree warns; a missing \
                 directory fails.",
        run: todo,
    },
    Check {
        id: "build.artifacts.reachable",
        expected: "every artifact URL answers with the named credentials",
        detail: "Reads: the artifact URLs the ISO build needs (BFB, DOCA host package, HBN \
                 container and bundle, rshim, libfuse2), formed from `versions.bfb`, `doca`, \
                 `hbn` and the builder's URL templates; credentials by source; the \
                 supported-combination table (from the provider guide).\n\
                 Pass: every URL answers with the credentials, 200 to a HEAD or 200 or 206 to a \
                 ranged GET; the \
                 DOCA, HBN and BFB versions form one supported combination; observed `6 artifacts \
                 answer; doca 3.2.2 / hbn 3.2.2 / bfb 3.2.2 supported`.\n\
                 Warn: the combination table is still the placeholder.\n\
                 Fail: a URL answering 401, 403, 404 or not at all (per artifact, with the \
                 status); a combination not in the table; fix: the entitlement or the version to \
                 change.\n\
                 Skip: when `build-host` is `No`.\n\
                 Test: a local HTTP stub serving the paths; cases: all 200 passes; one 404 fails \
                 naming the artifact; 401 fails naming the entitlement; a combination not in the \
                 table fails.",
        run: todo,
    },
    Check {
        id: "build.registry.push",
        expected: "registry login and a scratch push succeed",
        detail: "Reads: `hosts.registryHost` (url, caSource), credentials by source; the \
                 `source-of-the-nico-images` and `registry-host` decisions.\n\
                 Pass: login succeeds and a scratch tag under a `nico-pre/` repository pushes and \
                 is deleted; with images pulled from NGC, login to `nvcr.io` and a pull test per \
                 repository at the `versions` tags with the entitlement reported per artifact; \
                 observed `registry.site1: login ok, scratch push ok`.\n\
                 Fail: login refused, push refused (401, 403, 413), TLS not trusted via \
                 `caSource`; observed the step and the status; fix per status.\n\
                 Skip: the local-registry row when `registry-host` is `None` (only the NGC row \
                 runs); everything when `build-host` is `No`.\n\
                 Test: a local `registry:2` container with and without basic auth; cases: \
                 anonymous push passes; wrong credentials fail at login; a read-only registry \
                 fails at push; an untrusted CA fails naming `caSource`.",
        run: todo,
    },
    Check {
        id: "build.registry.manifest",
        expected: "every image the site needs present in the registry",
        detail: "Reads: the image list the site needs: Core and REST at `versions`, RMS when \
                 ticked, DPF images at `versions.docaPlatform`, boot artifacts (BFB, BF4 ISO) \
                 when the registry serves them; `hosts.registryHost`.\n\
                 Pass: every reference answers a manifest request (`HEAD \
                 /v2/<repo>/manifests/<tag>`) at the exact tag; observed `23 images present`.\n\
                 Fail: a reference missing; observed one clause per image \
                 (`nico/nico-api:v2026.10.15 missing`); fix: push it.\n\
                 Skip: when `registry-host` is `None`; the RMS and DPF images per their \
                 decisions.\n\
                 Test: a local registry seeded with tags in the test; cases: all present passes; \
                 one missing fails by name; RMS unticked does not require the RMS images.",
        run: todo,
    },
];
