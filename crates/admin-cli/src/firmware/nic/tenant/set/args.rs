/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use clap::Parser;
use config_version::ConfigVersion;
use model::nic_firmware::NicFirmwareProfileId;
use model::tenant::TenantOrganizationId;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Select a live NIC firmware profile for a tenant using its complete observed \
tenant version from firmware nic tenant show. The token is shared with \
ordinary tenant updates and identity-configuration updates, not the profile's \
edit version. Every successful write advances the tenant version, even if the \
same profile is already set. A missing tenant or profile returns NOT_FOUND; a \
stale tenant token returns FAILED_PRECONDITION. Core checks that the profile \
exists before checking the tenant or its version. No hardware is bound by \
this operation, so it does not validate device compatibility. Later edits to \
the live profile do not advance the tenant token.

This stores a requirement for future selected SPX attachments and does not \
flash firmware or change the profile definition. Until SPX attachment \
preparation is supported \
(https://github.com/dsx-ai-factory/infra-controller/issues/6686), the selection \
prevents allocations requesting SPX attachments. Allocations without SPX \
attachments are not blocked by this check.

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
all Core replicas before setting a selection; do not roll back to Core versions \
that ignore it.

The CLI neither fetches a fresh token nor retries a failed write. After a \
connection error, inspect firmware nic tenant show before another write; the \
change may have committed. Keep the original token when repeating the same \
request.

Put --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. The saved configuration includes the new tenant \
version: JSON/YAML returns one object with tenant_org_id, profile_id, and \
config_version; table/CSV returns Tenant Org ID, Tenant Version, and Profile ID. \
Output defaults to stdout. --output requires a path that does not exist and \
creates it before the write, even if the write fails. --sort-by and --extended \
have no effect.",
    after_long_help = "\
EXAMPLES:

Select a profile at the tenant version previously returned by show:
    $ nico-admin-cli firmware nic tenant set --tenant-org-id fds34511233a \
    --profile connectx-production --if-version-match V1-T1789080000000000

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
        value_name = "ID",
        help = "Case-sensitive profile ID; nonempty, without surrounding whitespace or controls"
    )]
    pub(super) profile: NicFirmwareProfileId,
    #[arg(
        long,
        value_name = "VERSION",
        help = "Complete observed tenant version, such as V1-T1789080000000000; keep unchanged when repeating this request"
    )]
    pub(super) if_version_match: ConfigVersion,
}
