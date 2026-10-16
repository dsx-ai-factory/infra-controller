/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::machine::HostMachineId;
use clap::Parser;
use model::nic_firmware::NicFirmwareProfileId;

#[derive(Parser, Debug)]
#[command(
    long_about = "\
Read desired NIC firmware for a host using Core's stored Scout observation. \
This command does not contact Scout, fetch artifacts, change selections, flash \
firmware, or change readiness. Reported identities and timestamps do not prove \
physical-card identity, observation freshness, or update eligibility.

Use matching v2.4 Core and CLI schemas; finish upgrading all Core replicas before \
relying on saved card selections or these desired-firmware diagnostics.

Core resolves card > machine > allocation > site selections. The optional \
allocation profile previews one requirement against every usable, non-managed-DPU \
device; it does not persist an allocation. Conflicts with card or machine \
selections, invalid selections, and excluded devices remain per-device errors \
without fallback. Exact targets may be downgrades. No selection is Unmanaged.

Targets retain server report order. Core-reported per-device errors do not hide \
other devices or fail an otherwise successful report. A malformed or unsupported \
Core response fails the entire read without a partial report. Warnings identify \
saved card selectors absent from the observation, including when no observation \
exists. Without an observation, \
the report has no targets and does not check an allocation profile's existence.

Use --format ascii-table (default), csv, json, or yaml and --output PATH before \
the firmware subcommand. JSON/YAML contains observed_at, targets (original \
identity, result, and site_selection_version), and warnings. JSON results use \
Firmware or Error objects; YAML uses !Firmware or !Error tags. Both use null \
for unmanaged devices. Firmware results include scope, profile ID/version, and \
the full entry with artifacts. ASCII/CSV includes reported Base MAC and Base GUID \
for comparison with saved card selectors. ASCII prints observation time and \
warnings before the table. CSV repeats Observed At and Warnings columns per \
device, with multiple warnings \
newline-separated in one cell. With no targets, CSV emits one metadata row with Status \
No devices and empty device fields. An absent observation reads Not observed in \
ASCII/CSV and null in JSON/YAML. Missing table values are blank.

Output defaults to stdout. --output requires a path that does not exist; the \
file is created before the operation, even if the operation later fails. \
--sort-by and --extended have no effect.",
    after_long_help = "\
EXAMPLES:

Show stored desired firmware and per-device errors:
    $ nico-admin-cli firmware nic desired fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Preview an allocation requirement against the host's NICs:
    $ nico-admin-cli firmware nic desired fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --allocation-profile connectx-production

Save the complete report, including artifacts and unmatched-selector warnings:
    $ nico-admin-cli --format json --output ./nic-desired.json firmware nic desired \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

"
)]
pub(crate) struct Args {
    #[arg(help = "Stable or predicted host machine ID; DPU IDs are rejected")]
    pub(super) host: HostMachineId,

    #[arg(
        long,
        value_name = "ID",
        help = "Optional case-sensitive profile ID for a machine-wide allocation preview; nonempty without surrounding whitespace or controls; omission uses saved selections"
    )]
    pub(super) allocation_profile: Option<NicFirmwareProfileId>,
}
