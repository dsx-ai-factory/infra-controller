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

use carbide_uuid::vpc::VpcId;
use clap::Parser;
use clap::builder::NonEmptyStringValueParser;
use config_version::ConfigVersion;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Inspect the VPC's persisted routing profile, observed version, active VNI, and \
retained allocation. Each RPC attempt uses the client request timeout (300 \
seconds by default, configurable with FORGE_CLIENT_REQUEST_TIMEOUT_SECS), \
including connection setup and response reads. This is not the effective routing profile or evidence of \
DPU/fabric convergence; site-global VNI overrides are not reflected here.

Use --format ascii-table (default), json, or yaml and --output PATH before vpc. \
CSV output is unsupported.",
    after_long_help = "\
EXAMPLES:

Inspect one VPC's persisted routing state:
    $ nico-admin-cli vpc routing-state 12345678-1234-5678-90ab-cdef01234567

Save routing state as JSON:
    $ nico-admin-cli --format json --output ./routing-state.json \
    vpc routing-state 12345678-1234-5678-90ab-cdef01234567

"
)]
pub(crate) struct Show {
    #[clap(help = "VPC ID to inspect")]
    pub(super) id: VpcId,
}

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Change a supported FNN VPC between configured profiles with opposite internal \
settings. Core validates the destination against the tenant's access tier and \
retains the previous VNI until an explicit release. A Core commit does not prove \
DPU/fabric convergence or guarantee that a later change back will be accepted.

Requires --cloud-unsafe-op USERNAME before vpc. Hold attachment, peering, \
deletion, and routing/profile-definition changes until the target and directly \
peered DPUs and fabric routes have been independently checked and the inactive \
allocation released. Core does not enforce this hold or verify convergence.

Omit --if-version-match for interactive use: the CLI reads the current state, \
shows the proposed action, and asks for confirmation before submitting that \
observed version. Both stdin and stderr must be terminals. Scripts must supply \
--if-version-match. Each invocation without a version approves a new action; it \
does not resume an earlier attempt.

Before mutation, the observed routing state is printed as JSON to stderr. \
Each RPC attempt uses the client request timeout (300 seconds by default, \
configurable with FORGE_CLIENT_REQUEST_TIMEOUT_SECS), including connection \
setup and response reads. This command does not retry mutations. After an ambiguous error, inspect vpc \
routing-state; the change may have committed. If rerunning the same request, \
keep the original --if-version-match and --vni selection, including omission. \
Do not substitute a fresh version automatically.

Use --vni only when every serving Core supports the field: older Core may ignore \
it and choose another VNI. A mismatched or lost response does not undo a commit.

Use --format ascii-table (default), json, or yaml and --output PATH before vpc. \
CSV output is unsupported. EXTERNAL and INTERNAL below are example configured \
profile names, not an exhaustive list.",
    after_long_help = "\
EXAMPLES:

Inspect and confirm a change interactively:
    $ nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile \
    12345678-1234-5678-90ab-cdef01234567 EXTERNAL

Change to a configured external profile, reusing a retained VNI or allocating automatically:
    $ nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile \
    12345678-1234-5678-90ab-cdef01234567 EXTERNAL --if-version-match V1-T1789080000000000

Request an exact destination VNI when every serving Core supports it:
    $ nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile \
    12345678-1234-5678-90ab-cdef01234567 EXTERNAL \
    --if-version-match V1-T1789080000000000 --vni 7000

Change back to a configured internal profile, subject to Core validation:
    $ nico-admin-cli --cloud-unsafe-op admin vpc change-routing-profile \
    12345678-1234-5678-90ab-cdef01234567 INTERNAL --if-version-match V1-T1789080000000000

"
)]
pub(crate) struct ChangeProfile {
    #[clap(help = "VPC ID whose routing profile will change")]
    pub(super) id: VpcId,

    #[clap(
        value_parser = NonEmptyStringValueParser::new(),
        help = "Nonempty destination profile name from the site's Core configuration"
    )]
    pub(super) routing_profile_type: String,

    #[clap(
        long,
        value_name = "VERSION",
        help = "Original observed version, for example V1-T1789080000000000; required for scripts, omitted for interactive confirmation; keep it unchanged when rerunning this request"
    )]
    pub(in crate::vpc) if_version_match: Option<ConfigVersion>,

    #[clap(
        long,
        value_parser = clap::value_parser!(u32).range(1..=16_777_215),
        help = "Exact destination VNI (1..=16777215); omission reuses a retained destination VNI or allocates automatically",
        long_help = "Exact destination VNI (1..=16777215). It must match any retained destination allocation; otherwise it must be a free materialized entry in the destination pool. Core rejects unavailable or mismatched values without falling back. Omission reuses a retained destination VNI or allocates automatically. Use only when every serving Core supports this field: older Core may ignore it and choose another VNI"
    )]
    pub(super) vni: Option<u32>,
}

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Release the exact inactive VNI retained after a routing-profile change. The \
active VNI and VPC configuration remain unchanged; the VPC version advances.

Requires --cloud-unsafe-op USERNAME before vpc and --confirm-convergence. Before \
release, independently verify that DPUs attached to the target VPC and its \
direct peers, plus fabric routes, no longer use the inactive VNI. Keep the \
allocation if any consumer is unreachable unless that consumer has been \
isolated. Hold concurrent attachment, peering, and routing changes through \
cleanup. Core validates allocation ownership, not dataplane convergence; a Core \
commit is not proof of convergence.

Omit --if-version-match for interactive use: the CLI shows the current state \
and asks for fresh confirmation that the displayed inactive allocation is safe \
to release. Both stdin and stderr must be terminals. Scripts must supply \
--if-version-match. Each invocation without a version approves a new action; it \
does not resume an earlier attempt.

Before mutation, the observed routing state is printed as JSON to stderr. \
Each RPC attempt uses the client request timeout (300 seconds by default, \
configurable with FORGE_CLIENT_REQUEST_TIMEOUT_SECS), including connection \
setup and response reads. This command does not retry mutations. After an ambiguous error, inspect vpc \
routing-state; the release may have committed. If rerunning the same request, \
keep the original --if-version-match and --expected-inactive-vni. A stale-version \
error does not prove whether release ran; do not substitute a fresh version \
automatically.

Use --format ascii-table (default), json, or yaml and --output PATH before vpc. \
CSV output is unsupported.",
    after_long_help = "\
EXAMPLES:

Inspect and confirm release interactively after checking convergence:
    $ nico-admin-cli --cloud-unsafe-op admin vpc release-inactive-vni \
    12345678-1234-5678-90ab-cdef01234567 --expected-inactive-vni 7000 --confirm-convergence

Release the observed inactive VNI after checking convergence and holding concurrent changes:
    $ nico-admin-cli --cloud-unsafe-op admin vpc release-inactive-vni \
    12345678-1234-5678-90ab-cdef01234567 --if-version-match V1-T1789080000000000 \
    --expected-inactive-vni 7000 --confirm-convergence

"
)]
pub(crate) struct ReleaseInactiveVni {
    #[clap(help = "VPC ID whose inactive allocation will be released")]
    pub(super) id: VpcId,

    #[clap(
        long,
        value_name = "VERSION",
        help = "Original version observed with the inactive VNI; required for scripts, omitted for interactive confirmation; keep it unchanged when rerunning this request"
    )]
    pub(in crate::vpc) if_version_match: Option<ConfigVersion>,

    #[clap(
        long,
        value_parser = clap::value_parser!(u32).range(1..=16_777_215),
        help = "Exact inactive VNI observed with this version (1..=16777215); keep it unchanged when rerunning this request"
    )]
    pub(super) expected_inactive_vni: u32,

    #[clap(
        long,
        required = true,
        help = "Acknowledge that target and direct-peer DPUs and fabric routes no longer use this VNI and concurrent attachment, peering, and routing changes are held"
    )]
    pub(super) confirm_convergence: bool,
}
