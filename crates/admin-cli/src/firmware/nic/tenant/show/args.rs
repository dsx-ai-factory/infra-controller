/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use model::tenant::TenantOrganizationId;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Read one tenant's saved NIC firmware profile selection and complete shared \
tenant version. An existing tenant without a selection still returns its \
configuration. Tenant selections leave site, machine, and card selections \
unchanged. A missing tenant returns NOT_FOUND. This command does not inspect \
hardware, fetch artifacts, or flash firmware.

Use the returned version with tenant set or clear. Ordinary tenant updates \
and identity-configuration updates also change this token; it is independent \
of the profile's edit version.

Put --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON/YAML returns one object with tenant_org_id, \
profile_id, and config_version; an unset profile is null. Table/CSV returns Tenant \
Org ID, Tenant Version, and Profile ID, with an empty profile cell when unset. \
Output defaults to stdout. --output requires a path that does not exist and \
creates it before the read, even if the read fails. --sort-by and --extended \
have no effect.",
    after_long_help = "\
EXAMPLES:

Read a tenant's selection and version before a change:
    $ nico-admin-cli firmware nic tenant show --tenant-org-id fds34511233a

Read the selection as JSON, including null when no profile is selected:
    $ nico-admin-cli --format json firmware nic tenant show --tenant-org-id fds34511233a

"
)]
pub(crate) struct Args {
    #[arg(
        long,
        value_name = "ID",
        help = "Case-sensitive tenant organization ID; nonempty ASCII letters, digits, underscores, or hyphens"
    )]
    pub(super) tenant_org_id: TenantOrganizationId,
}
