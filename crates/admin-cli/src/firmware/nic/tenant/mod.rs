/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

mod clear;
mod output;
mod set;
mod show;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
#[command(
    long_about = "\
Read or change the operator-selected NIC firmware profile for a tenant. The \
selection uses the tenant's shared configuration version: ordinary tenant \
updates and identity-configuration updates also invalidate an observed token. \
Every successful set or clear advances that version, including writes that \
leave the selection unchanged. Tenant selections leave site, machine, and \
card selections unchanged.

This stores a requirement for future selected SPX attachments and does not \
flash firmware. Until SPX attachment preparation is supported \
(https://github.com/dsx-ai-factory/infra-controller/issues/6686), a tenant with \
a selection cannot allocate an instance requesting SPX attachments. Allocations \
without SPX attachments are not blocked by this check.

Allocations with saved firmware requirements keep them after tenant selection \
or profile edits. Removals and partition edits are allowed if retained \
attachments keep their saved device bindings. Adding attachments is rejected \
when the saved requirements include a tenant profile.

Instances without saved requirements use the live tenant selection. With a \
profile selected, edits that leave a nonempty SPX attachment list are rejected. \
Unchanged attachments and clearing the list are not blocked. Both saved and \
live firmware-check rejections return FAILED_PRECONDITION.

The live tenant-selection check is read-time: a request already past the \
selection read may complete after a later selection commits. Finish upgrading \
all Core replicas before setting tenant selections; do not roll back to Core \
versions that ignore them.

Show, set, and clear return one object with tenant_org_id, profile_id, and \
config_version in JSON/YAML. An unset profile is null. Table/CSV always returns one \
row with Tenant Org ID, Tenant Version, and Profile ID columns; an unset \
profile has an empty cell. Put --format ascii-table (default), csv, json, or \
yaml and --output PATH before the firmware subcommand. Output defaults to \
stdout. --output requires a path that does not exist and creates it before \
the operation, even if the operation fails. --sort-by and --extended have no \
effect.",
    after_long_help = "\
EXAMPLES:

Read a tenant's selection and shared configuration version:
    $ nico-admin-cli firmware nic tenant show --tenant-org-id fds34511233a

Select a profile using the previously observed tenant version:
    $ nico-admin-cli firmware nic tenant set --tenant-org-id fds34511233a \
    --profile connectx-production --if-version-match V1-T1789080000000000

Clear the selection using the version returned by the last write:
    $ nico-admin-cli firmware nic tenant clear --tenant-org-id fds34511233a \
    --if-version-match V2-T1789080000000001

"
)]
pub(crate) enum Cmd {
    #[command(about = "Show a tenant's NIC firmware selection and shared version")]
    Show(show::Args),
    #[command(about = "Select a NIC firmware profile at an observed tenant version")]
    Set(set::Args),
    #[command(about = "Clear a NIC firmware selection at an observed tenant version")]
    Clear(clear::Args),
}
