/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use config_version::ConfigVersion;
use model::tenant::TenantOrganizationId;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Clear a tenant's NIC firmware profile selection using its complete observed \
tenant version from firmware nic tenant show. The token is shared with \
ordinary tenant updates and identity-configuration updates, not the profile's \
edit version. Every successful clear advances the tenant version, even if no \
profile is selected. A missing tenant returns NOT_FOUND; a stale token returns \
FAILED_PRECONDITION.

This removes the tenant's requirement for future selected SPX attachments. \
It leaves site, machine, and card selections unchanged and does not flash \
firmware, edit profile definitions, or change existing instances.

The CLI neither fetches a fresh token nor retries a failed write. After a \
connection error, inspect firmware nic tenant show before another write; the \
clear may have committed. Keep the original token when repeating the same \
request.

Put --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON/YAML returns one object with tenant_org_id, a \
null profile_id, and the new config_version. Table/CSV returns Tenant Org ID, Tenant \
Version, and Profile ID, with an empty profile cell. Output defaults to stdout. \
--output requires a path that does not exist and creates it before the clear, \
even if the clear fails. --sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

Clear the selection at the tenant version previously returned by show:
    $ nico-admin-cli firmware nic tenant clear --tenant-org-id fds34511233a \
    --if-version-match V2-T1789080000000001

"
)]
pub(crate) struct Args {
    #[arg(
        long,
        value_name = "ID",
        help = "Case-sensitive tenant organization ID; nonempty ASCII letters, digits, underscores, or hyphens"
    )]
    pub(super) tenant_org_id: TenantOrganizationId,
    #[arg(
        long,
        value_name = "VERSION",
        help = "Complete observed tenant version, such as V2-T1789080000000001; keep unchanged when repeating this request"
    )]
    pub(super) if_version_match: ConfigVersion,
}
