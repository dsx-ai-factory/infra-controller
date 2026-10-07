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

use carbide_utils::has_duplicates;
use carbide_uuid::rack::RackId;
use clap::error::ErrorKind;
use clap::{ArgGroup, CommandFactory, Parser};
use mac_address::MacAddress;
use rpc::forge::BmcIpAllocationType;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::expected_machines::common::HostDpuPolicy;

/// Patch an expected machine.
///
/// Select the machine by either BMC MAC address or ID. Supplied fields replace their stored values;
/// omitted fields remain unchanged. Supplied labels replace the whole label collection. An empty
/// metadata name or description clears that field. An empty interfaces array clears the stored list.
/// BMC address and interface updates also reconcile the associated static interface configuration.
///
/// Supply a BMC username, password, or both. Each omitted credential field keeps its stored value.
/// Core PATCH rejects empty selected credentials. A selected chassis serial must contain 4-32
/// ASCII letters, digits, hyphens, or underscores. Legacy fallback uses the validation rules on the older server.
///
/// Supply at least one update field.
///
/// The command first tries Core PATCH, which merges selected fields atomically. It falls back to
/// the legacy update on `Unimplemented` or `PermissionDenied`, or when a MAC lookup returns no ID.
/// The legacy machine update reads the record, merges changes locally, and replaces it. Concurrent
/// changes can be overwritten on that path. The legacy request still requires authorization.
/// Other PATCH errors and failed legacy updates remain errors.
///
/// https://github.com/dsx-ai-factory/infra-controller/pull/6359
#[derive(Parser, Debug, Serialize, Deserialize)]
#[clap(verbatim_doc_comment)]
#[clap(group(ArgGroup::new("group").required(true).multiple(true).args(&[
"bmc_username",
"bmc_password",
"bmc_retain_credentials",
"chassis_serial_number",
"fallback_dpu_serial_numbers",
"meta_name",
"meta_description",
"labels",
"sku_id",
"bmc_ip_address",
"dpu_policy",
"bmc_ip_allocation",
"dpf_enabled",
"default_pause_ingestion_and_poweron",
"interfaces",
"disable_lockdown",
])))]
#[command(after_long_help = "\
EXAMPLES:

Patch only the SKU of a machine, selected by BMC MAC address:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --sku-id DGX-H100-640GB

Patch a machine selected by id:
    $ nico-admin-cli expected-machine patch --id 12345678-1234-5678-90ab-cdef01234567 \
    --sku-id DGX-H100-640GB

Replace labels and clear the description while setting the SKU:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --sku-id DGX-H100-640GB --label env:prod --label team:platform --meta-description \"\"

Correct the BMC password while preserving the username:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-password mynewpassword

Correct the BMC username while preserving the password:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-username admin

Change the per-host DPU policy:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --dpu-policy ignore

Retain the BMC's auto-allocated DHCP address as static for the lifetime of its
machine-interface record:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --bmc-ip-allocation retained

Replace the interface list for a matching stored interface that already has a
fixed IP. The omitted role and allocation policy keep their stored values:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --interfaces '[{\"mac_address\":\"02:00:00:00:20:01\",\"fixed_ip\":\"192.0.2.10\"}]'

Reset that role to Host and infer Fixed allocation from fixed_ip:
    $ nico-admin-cli expected-machine patch --bmc-mac-address 00:11:22:33:44:55 \
    --interfaces '[{\"mac_address\":\"02:00:00:00:20:01\",\"role\":\"unspecified\",\"ip_allocation\":\"unspecified\",\"fixed_ip\":\"192.0.2.10\"}]'

")]
pub(crate) struct Args {
    #[clap(short = 'a', long, help = "BMC MAC Address of the expected machine")]
    pub(super) bmc_mac_address: Option<MacAddress>,

    #[clap(long = "id", help = "ID (UUID) of the expected machine to patch.")]
    #[serde(skip)]
    pub(super) id: Option<Uuid>,
    #[clap(
        short = 'u',
        long,
        group = "group",
        help = "BMC username of the expected machine"
    )]
    pub(super) bmc_username: Option<String>,
    #[clap(
        short = 'p',
        long,
        group = "group",
        help = "BMC password of the expected machine"
    )]
    pub(super) bmc_password: Option<String>,
    #[clap(
        short = 's',
        long,
        group = "group",
        help = "Replace the chassis serial number. Core PATCH requires 4-32 ASCII letters, digits, hyphens, or underscores"
    )]
    pub(super) chassis_serial_number: Option<String>,
    #[clap(
        short = 'd',
        long = "fallback-dpu-serial-number",
        value_name = "DPU_SERIAL_NUMBER",
        group = "group",
        help = "Serial number of the DPU attached to the expected machine. This option should be used only as a last resort for ingesting those servers whose BMC/Redfish do not report serial number of network devices. This option can be repeated.",
        action = clap::ArgAction::Append
    )]
    pub(super) fallback_dpu_serial_numbers: Option<Vec<String>>,

    #[clap(
        long = "meta-name",
        value_name = "META_NAME",
        help = "Replace the metadata name (Core PATCH: ASCII, at most 256 characters). An empty value clears it; omission preserves it"
    )]
    pub(super) meta_name: Option<String>,

    #[clap(
        long = "meta-description",
        value_name = "META_DESCRIPTION",
        help = "Replace the metadata description (Core PATCH: at most 1024 bytes). An empty value clears it; omission preserves it"
    )]
    pub(super) meta_description: Option<String>,

    #[clap(
        long = "label",
        value_name = "LABEL",
        help = "Replace all metadata labels with the supplied key or key:value entries. Repeat for each label. Duplicate keys are rejected; label order is not preserved. Core PATCH allows up to 16 keys. Keys must be nonempty ASCII and at most 255 characters; values allow at most 255 bytes. Whitespace around each key and value is trimmed. Omission preserves labels",
        action = clap::ArgAction::Append
    )]
    pub(super) labels: Option<Vec<String>>,

    #[clap(
        long,
        value_name = "SKU_ID",
        group = "group",
        help = "Replace the expected machine SKU ID. Omission preserves it"
    )]
    pub(super) sku_id: Option<String>,

    #[clap(
        long,
        value_name = "RACK_ID",
        group = "group",
        help = "Replace the expected machine rack ID. Omission preserves it"
    )]
    pub(super) rack_id: Option<RackId>,

    #[clap(
        long = "default_pause_ingestion_and_poweron",
        value_name = "DEFAULT_PAUSE_INGESTION_AND_POWERON",
        help = "Initial pause state applied when the BMC endpoint for this machine is first explored. `true` pauses ingestion and automatic power-on; `false` pauses neither. Omit to preserve the existing Expected Machine value. Changes do not affect an endpoint that has already been explored."
    )]
    pub(super) default_pause_ingestion_and_poweron: Option<bool>,

    #[clap(
        long,
        action = clap::ArgAction::Set,
        value_name = "DPF_ENABLED",
        help = "Whether DPF is enabled for this machine. Omit to preserve the existing value.",
    )]
    pub(super) dpf_enabled: Option<bool>,

    #[clap(
        long = "bmc-ip-address",
        value_name = "BMC_IP_ADDRESS",
        group = "group",
        help = "Static BMC IP (updates pre-allocated machine_interface when safe, same as expected switches)"
    )]
    pub(super) bmc_ip_address: Option<String>,

    #[clap(
        long = "bmc-retain-credentials",
        value_name = "BMC_RETAIN_CREDENTIALS",
        help = "When true, site-explorer skips BMC password rotation and stores factory-default credentials in Vault as-is"
    )]
    pub(super) bmc_retain_credentials: Option<bool>,

    #[clap(
        long = "dpu-policy",
        visible_alias = "dpu-mode",
        value_name = "DPU_POLICY",
        value_enum,
        group = "group",
        help = "Per-host DPU policy. `manage`: inherit the site policy, which defaults to managing DPUs; `nic`: configure DPU hardware as plain NICs; `ignore`: do not configure or attach DPU hardware. Unset preserves the existing per-host value. The previous `use-as-nic` value remains accepted as an alias. The legacy `--dpu-mode` flag also remains accepted: `dpu-mode` maps to `manage`, `nic-mode` to `nic`, and `no-dpu` to `ignore`."
    )]
    pub(super) dpu_policy: Option<HostDpuPolicy>,

    #[clap(
        long = "bmc-ip-allocation",
        value_name = "BMC_IP_ALLOCATION",
        value_enum,
        group = "group",
        help = "Per-host control over IP assignment and retention for this BMC. `auto` (default): infer from `--bmc-ip-address` -- a configured address is `fixed`, no address is `retained`; `dynamic`: a normal DHCP lease that may expire and change; `fixed`: the operator-specified `--bmc-ip-address` (static); `retained`: an auto-allocated DHCP address that stays static for the lifetime of its machine-interface record. Unset preserves the existing per-host value."
    )]
    pub(super) bmc_ip_allocation: Option<BmcIpAllocationType>,

    #[clap(
        long = "interfaces",
        visible_alias = "host_nics",
        value_name = "INTERFACES",
        group = "group",
        help = "Interfaces as a JSON array of ExpectedInterface objects (fields: mac_address, role, ip_allocation, network_segment_type, fixed_ip, fixed_mask, fixed_gateway, primary; legacy: nic_type). Accepted values: role=host|dpu_os|dpu_bmc|host_bmc|unspecified and ip_allocation=dynamic|fixed|retained|unspecified. network_segment_type uses protobuf enum numbers: tenant=0, admin=1, underlay=2, host_inband=3. Replaces the full interface list for the machine. For a matching stored MAC, omitting role preserves the stored role; role=unspecified resets it to host. Omitting ip_allocation preserves the stored policy when the presence of fixed_ip is unchanged; ip_allocation=unspecified resets it to fixed_ip inference. Omitting any other optional interface field, including network_segment_type, clears its stored value."
    )]
    pub(super) interfaces: Option<String>,

    #[clap(
        long = "disable-lockdown",
        value_name = "DISABLE_LOCKDOWN",
        help = "Set true to skip server lockdown during lifecycle management, or false to lock down after BIOS configuration. Omission preserves the stored setting"
    )]
    pub(super) disable_lockdown: Option<bool>,
}

impl Args {
    pub(super) fn validate(&self) -> Result<(), clap::Error> {
        let error = |kind, message: &str| {
            Self::command()
                .bin_name("nico-admin-cli expected-machine patch")
                .error(kind, message)
        };
        match (&self.bmc_mac_address, &self.id) {
            (Some(_), Some(_)) => {
                return Err(error(
                    ErrorKind::ArgumentConflict,
                    "cannot specify both --bmc-mac-address and --id; provide only one",
                ));
            }
            (None, None) => {
                return Err(error(
                    ErrorKind::MissingRequiredArgument,
                    "must specify either --bmc-mac-address or --id",
                ));
            }
            _ => {}
        }
        if self
            .fallback_dpu_serial_numbers
            .as_ref()
            .is_some_and(has_duplicates)
        {
            return Err(error(
                ErrorKind::ValueValidation,
                "duplicate --fallback-dpu-serial-number values; supply each serial number only once",
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::expected_machines) fn validate_for_test(&self) -> Result<(), clap::Error> {
        self.validate()
    }
}
