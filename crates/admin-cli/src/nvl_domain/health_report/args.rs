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

use carbide_uuid::nvlink::NvLinkDomainId;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List the health report sources for an NVLink domain:
    $ nico-admin-cli nvl-domain health-report show 12345678-1234-5678-90ab-cdef01234567

Remove a health report source (source name from `health-report show`):
    $ nico-admin-cli nvl-domain health-report remove 12345678-1234-5678-90ab-cdef01234567 \
    internal-maintenance

Print an empty health report template:
    $ nico-admin-cli nvl-domain health-report print-empty-template

")]
pub(crate) enum Args {
    #[clap(about = "List health report sources for an NVLink domain")]
    Show { domain_id: NvLinkDomainId },
    #[clap(about = "Print an empty health report template")]
    PrintEmptyTemplate,
    #[clap(about = "Remove a health report source from an NVLink domain")]
    Remove {
        domain_id: NvLinkDomainId,
        report_source: String,
    },
}
