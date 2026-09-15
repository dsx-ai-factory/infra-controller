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
use std::collections::HashMap;
use std::net::IpAddr;

use carbide_network::ip::{IdentifyAddressFamily, IpAddressFamily};
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{FromRow, Row};
use uuid::Uuid;

use crate::machine_interface::InterfaceType;
use crate::metadata::Metadata;
use crate::network_segment::NetworkSegmentType;

/// Operator policy for how NICo treats a host's DPU hardware.
///
/// This is distinct from a device's observed operating mode. At the per-host
/// boundary, [`HostDpuPolicy::Manage`] retains the legacy inheritance behavior:
/// it defers to the site-wide policy before falling back to managed DPUs.
///
/// Backed by the Postgres enum `dpu_mode_t`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "dpu_mode_t")]
#[serde(rename_all = "snake_case")]
pub enum HostDpuPolicy {
    /// Manage DPUs normally, including upgrades, networking, and DPA agents.
    #[default]
    #[sqlx(rename = "dpu_mode")]
    #[serde(alias = "dpu_mode")]
    Manage,
    /// Configure physically present DPUs as NICs and manage the host as zero-DPU.
    #[sqlx(rename = "nic_mode")]
    #[serde(alias = "use_as_nic", alias = "nic_mode")]
    Nic,
    /// Do not configure or attach DPU hardware to the managed host.
    #[sqlx(rename = "no_dpu")]
    #[serde(alias = "no_dpu")]
    Ignore,
}

impl HostDpuPolicy {
    /// Resolve per-host and site-wide declarations into a concrete policy.
    ///
    /// Per-host [`HostDpuPolicy::Nic`] and [`HostDpuPolicy::Ignore`]
    /// override the site. Per-host [`HostDpuPolicy::Manage`] or a missing
    /// declaration inherits the site. A missing site declaration resolves to
    /// [`HostDpuPolicy::Manage`].
    pub fn resolve(per_host_policy: Option<Self>, site_policy: Option<Self>) -> Self {
        match per_host_policy {
            Some(Self::Nic) => Self::Nic,
            Some(Self::Ignore) => Self::Ignore,
            Some(Self::Manage) | None => site_policy.unwrap_or_default(),
        }
    }

    /// Whether this policy expects NICo to discover and manage the host's DPUs.
    pub fn expects_managed_dpus(self) -> bool {
        matches!(self, Self::Manage)
    }
}

#[derive(Deserialize)]
struct HostDpuPolicyFields {
    #[serde(default)]
    dpu_policy: Option<HostDpuPolicy>,
    #[serde(default)]
    dpu_mode: Option<HostDpuPolicy>,
}

fn deserialize_host_dpu_policy<'de, D>(deserializer: D) -> Result<HostDpuPolicy, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let fields = HostDpuPolicyFields::deserialize(deserializer)?;

    Ok(fields.dpu_policy.or(fields.dpu_mode).unwrap_or_default())
}

/// Controls how a BMC's IP address is assigned and whether it is retained.
///
/// - `Auto` (default): infer from `bmc_ip_address` -- a configured address is
///   treated as `Fixed`, no address is treated as `Retained`.
/// - `Dynamic`: a normal DHCP lease that may expire and change.
/// - `Fixed`: the operator-specified `bmc_ip_address` (static).
/// - `Retained`: an auto-allocated DHCP address that stays static for the
///   lifetime of its machine-interface record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(type_name = "bmc_ip_allocation_t", rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum BmcIpAllocationType {
    #[default]
    Auto,
    Dynamic,
    Fixed,
    Retained,
}

impl BmcIpAllocationType {
    /// Validate the mode against whether a `bmc_ip_address` is configured.
    pub fn validate(self, has_address: bool) -> Result<(), &'static str> {
        match self {
            BmcIpAllocationType::Fixed if !has_address => {
                Err("bmc_ip_allocation=fixed requires bmc_ip_address")
            }
            BmcIpAllocationType::Dynamic if has_address => {
                Err("bmc_ip_allocation=dynamic cannot be combined with bmc_ip_address")
            }
            BmcIpAllocationType::Retained if has_address => {
                Err("bmc_ip_allocation=retained cannot be combined with bmc_ip_address; use fixed")
            }
            _ => Ok(()),
        }
    }

    /// Whether an auto-allocated BMC IP should be retained (pinned as Static)
    /// instead of left as an expirable DHCP lease. Only meaningful with no address.
    pub fn retains_dynamic_ip(self, has_address: bool) -> bool {
        match self {
            BmcIpAllocationType::Auto => !has_address,
            BmcIpAllocationType::Retained => true,
            BmcIpAllocationType::Dynamic | BmcIpAllocationType::Fixed => false,
        }
    }

    /// Resolve the compatibility BMC policy to the allocation policy used by
    /// an expected interface.
    ///
    /// `Auto` is the only policy whose meaning depends on the address: a
    /// configured address is Fixed, while an addressless BMC is Retained.
    pub fn resolved(self, has_address: bool) -> ExpectedInterfaceIpAllocation {
        match self {
            Self::Auto if has_address => ExpectedInterfaceIpAllocation::Fixed,
            Self::Auto => ExpectedInterfaceIpAllocation::Retained,
            Self::Dynamic => ExpectedInterfaceIpAllocation::Dynamic,
            Self::Fixed => ExpectedInterfaceIpAllocation::Fixed,
            Self::Retained => ExpectedInterfaceIpAllocation::Retained,
        }
    }
}

/// Convert an expected-interface policy to its top-level BMC compatibility
/// equivalent.
impl From<ExpectedInterfaceIpAllocation> for BmcIpAllocationType {
    fn from(policy: ExpectedInterfaceIpAllocation) -> Self {
        match policy {
            ExpectedInterfaceIpAllocation::Dynamic => Self::Dynamic,
            ExpectedInterfaceIpAllocation::Fixed => Self::Fixed,
            ExpectedInterfaceIpAllocation::Retained => Self::Retained,
        }
    }
}

/// Legacy top-level BMC fields explicitly supplied with an ExpectedMachine
/// write.
///
/// The outer `Option` on `ip_address` distinguishes an omitted field from an
/// explicit clear. This is request state only. Nested Host BMC declarations
/// store their effective settings in both `interfaces` and the compatibility
/// columns, while legacy-only declarations remain legacy-shaped for older
/// clients and are resolved in memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LegacyHostBmcOverrides {
    /// Omitted, explicitly cleared, or configured compatibility address.
    pub ip_address: Option<Option<IpAddr>>,
    /// Compatibility policy when the request explicitly supplied it.
    pub ip_allocation: Option<BmcIpAllocationType>,
    /// Whether the incoming `interfaces` list is a complete replacement.
    ///
    /// Older protobuf writers cannot distinguish an omitted repeated field
    /// from an empty one. They leave this false so an unknown nested Host BMC
    /// survives a read-modify-write.
    pub replace_interfaces: bool,
}

/// A request to identify an ExpectedMachine by either ID or MAC address.
#[derive(Debug, Clone)]
pub struct ExpectedMachineRequest {
    pub id: Option<Uuid>,
    pub bmc_mac_address: Option<MacAddress>,
}

/// Identifies which machine endpoint an expected interface belongs to.
///
/// The role determines the resulting [`InterfaceType`] and whether the
/// interface has a role-defined primary state. It does not choose a network
/// segment or change the allocation policy available to that interface.
///
/// Host BMC identity remains on [`ExpectedMachine::bmc_mac_address`], while a
/// `HostBmc` entry configures that endpoint's network allocation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedInterfaceRole {
    /// A host operating-system interface. This preserves the behavior of
    /// declarations created before interface roles were introduced.
    #[default]
    Host,
    /// The DPU operating-system interface.
    DpuOs,
    /// The DPU's Redfish/BMC interface.
    DpuBmc,
    /// The host's Redfish/BMC interface.
    HostBmc,
}

impl std::fmt::Display for ExpectedInterfaceRole {
    /// Write the canonical TOML/JSON spelling used for this role.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Host => "host",
            Self::DpuOs => "dpu_os",
            Self::DpuBmc => "dpu_bmc",
            Self::HostBmc => "host_bmc",
        })
    }
}

impl ExpectedInterfaceRole {
    /// Return whether this role participates in machine-wide Host primary
    /// selection.
    ///
    /// Serde also uses this predicate to omit the default Host role from stored
    /// JSON, keeping legacy Host declarations free of a new role field.
    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }

    /// Return whether this role configures the ExpectedMachine's host BMC.
    pub fn is_host_bmc(&self) -> bool {
        matches!(self, Self::HostBmc)
    }

    /// Return the database interface type produced by this endpoint role.
    ///
    /// Host and DPU OS endpoints both exchange operating-system data. DPU and
    /// host BMC endpoints use the BMC interface type so the existing
    /// BMC-specific routing and address behavior still applies.
    pub fn interface_type(self) -> InterfaceType {
        match self {
            Self::Host | Self::DpuOs => InterfaceType::Data,
            Self::DpuBmc | Self::HostBmc => InterfaceType::Bmc,
        }
    }

    /// Return the primary-interface value fixed by this endpoint role.
    ///
    /// Host primary selection needs all Host declarations on the
    /// `ExpectedMachine`, so `None` tells the caller to use that machine-wide
    /// result. A DPU OS interface is always its DPU's primary data interface,
    /// while DPU and host BMC interfaces are never primary.
    pub fn primary_interface_override(self) -> Option<bool> {
        match self {
            Self::Host => None,
            Self::DpuOs => Some(true),
            Self::DpuBmc | Self::HostBmc => Some(false),
        }
    }
}

/// Controls how an expected interface receives and retains its IP address.
///
/// When the policy is omitted, [`ExpectedInterface::resolved_ip_allocation`]
/// preserves the legacy configuration contracts. An interface with
/// [`ExpectedInterface::fixed_ip`] is fixed. Without one, Host BMC is retained
/// and every other role is dynamic. Explicit `Dynamic` differs from omission
/// because it rejects a simultaneous `fixed_ip` instead of inferring `Fixed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpectedInterfaceIpAllocation {
    /// Allocate a normal DHCP lease that may expire and change. A configured
    /// segment-type guard must match the segment selected by the DHCP relay.
    Dynamic,
    /// Reserve the operator-specified [`ExpectedInterface::fixed_ip`]. An
    /// explicit Fixed policy, either DPU role, or HostBmc with a segment guard
    /// requires a configured managed prefix to contain the address. That
    /// prefix selects the segment, and a configured segment-type guard must
    /// match it.
    Fixed,
    /// Allocate through DHCP and insert the address as Static on that family's
    /// first stateful allocation. The address is not saved in
    /// `ExpectedMachine` for reuse after the interface is deleted and
    /// re-ingested. Later policy changes do not convert existing addresses.
    /// A configured segment-type guard must match the segment selected by the
    /// DHCP relay.
    Retained,
}

impl ExpectedInterfaceIpAllocation {
    /// Validate whether this explicit or resolved policy may be paired with
    /// the interface's `fixed_ip` declaration.
    ///
    /// Only `Fixed` accepts a configured address. `Dynamic` and `Retained`
    /// require DHCP to select one.
    pub fn validate(self, has_fixed_ip: bool) -> Result<(), &'static str> {
        match self {
            Self::Fixed if !has_fixed_ip => Err("ip_allocation=fixed requires fixed_ip"),
            Self::Dynamic if has_fixed_ip => {
                Err("ip_allocation=dynamic cannot be combined with fixed_ip")
            }
            Self::Retained if has_fixed_ip => {
                Err("ip_allocation=retained cannot be combined with fixed_ip; use fixed")
            }
            _ => Ok(()),
        }
    }
}

/// Configures one interface that NICo may encounter while ingesting an
/// `ExpectedMachine`.
///
/// Every role uses the same allocation and optional segment-guard fields. The
/// role only supplies endpoint-specific interface type and primary behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct ExpectedInterface {
    /// MAC address used to match DHCP and discovered interface traffic to this
    /// declaration.
    pub mac_address: MacAddress,
    /// Which machine endpoint owns this interface. Missing values retain the
    /// legacy host-interface behavior.
    #[serde(default, skip_serializing_if = "ExpectedInterfaceRole::is_host")]
    pub role: ExpectedInterfaceRole,
    /// Optional IP allocation policy. Missing declarations infer `Fixed` when
    /// [`Self::fixed_ip`] is configured. Without one, Host BMC uses the legacy
    /// Retained behavior and every other role uses Dynamic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_allocation: Option<ExpectedInterfaceIpAllocation>,
    /// Optional guard for the interface's network segment type.
    ///
    /// For an explicit allocation policy or DPU role, Dynamic and Retained use
    /// this as a guard on the segment selected by the DHCP relay, while Fixed
    /// uses it as a guard on the segment containing the configured address.
    ///
    /// A legacy Host declaration that omits `ip_allocation` keeps the earlier
    /// behavior where this field only narrows initial DHCP segment selection.
    /// This compatibility rule is exposed by [`Self::segment_type_guard`].
    /// `None` (with no legacy [`Self::nic_type`]) leaves DHCP selection
    /// unconstrained. Fixed uses the managed prefix containing the address.
    /// A legacy Host declaration with an omitted policy, or an inferred Host
    /// BMC Fixed policy without a segment guard, may fall back to
    /// `static-assignments` when no configured prefix contains it. Updates
    /// retain the existing full-replacement behavior, so omission clears this
    /// field.
    #[serde(default)]
    pub network_segment_type: Option<NetworkSegmentType>,
    /// Legacy free-form NIC-type segment hint (`bf3`, `onboard`, `oob`, ...).
    /// Kept for backward compatibility; prefer `network_segment_type`.
    pub nic_type: Option<String>,
    pub fixed_ip: Option<IpAddr>,
    pub fixed_mask: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_ip_addr_lossy")]
    pub fixed_gateway: Option<IpAddr>,
    /// Host-role interfaces may set `primary=true` to declare the host's boot
    /// interface. When one Host interface is declared primary, the other Host
    /// interfaces become non-primary. An explicit `false` remains accepted for
    /// compatibility but does not replace that machine-wide selection. DPU OS
    /// interfaces are primary data interfaces and BMC interfaces are
    /// non-primary; those roles must omit this field.
    #[serde(default)]
    pub primary: Option<bool>,
}

/// Compatibility name for callers that still use the original host-only
/// interface vocabulary.
pub type ExpectedHostNic = ExpectedInterface;

impl ExpectedInterface {
    /// Return the configured allocation policy or infer the legacy policy from
    /// the role and whether this interface has a fixed IP.
    ///
    /// Omission must remain distinguishable from explicit `Dynamic`: old
    /// declarations supplied only `fixed_ip`, while explicit `Dynamic` rejects
    /// that same combination during validation. An addressless Host BMC uses
    /// the top-level BMC default of Retained. Inferring `Fixed` here does not
    /// opt a legacy Host declaration into the newer segment-type guard; see
    /// [`Self::segment_type_guard`].
    pub fn resolved_ip_allocation(&self) -> ExpectedInterfaceIpAllocation {
        self.ip_allocation.unwrap_or_else(|| {
            if self.fixed_ip.is_some() {
                ExpectedInterfaceIpAllocation::Fixed
            } else if self.role.is_host_bmc() {
                ExpectedInterfaceIpAllocation::Retained
            } else {
                ExpectedInterfaceIpAllocation::Dynamic
            }
        })
    }

    /// Validate the effective allocation policy against this interface's
    /// fixed-IP declaration.
    pub fn validate_ip_allocation(&self) -> Result<(), &'static str> {
        self.resolved_ip_allocation()
            .validate(self.fixed_ip.is_some())
    }

    /// Validate the declaration and require one resolved allocation policy.
    pub fn require_ip_allocation(
        &self,
        required: ExpectedInterfaceIpAllocation,
    ) -> Result<(), &'static str> {
        self.validate_ip_allocation()?;
        if self.resolved_ip_allocation() == required {
            return Ok(());
        }

        Err(match required {
            ExpectedInterfaceIpAllocation::Dynamic => {
                "expected interface does not use dynamic IP allocation"
            }
            ExpectedInterfaceIpAllocation::Fixed => {
                "expected interface does not use fixed IP allocation"
            }
            ExpectedInterfaceIpAllocation::Retained => {
                "expected interface does not use retained IP allocation"
            }
        })
    }

    /// Return the configured address after validating that this interface uses
    /// Fixed allocation.
    pub fn fixed_reservation_ip(&self) -> Result<IpAddr, &'static str> {
        self.require_ip_allocation(ExpectedInterfaceIpAllocation::Fixed)?;
        self.fixed_ip.ok_or("ip_allocation=fixed requires fixed_ip")
    }

    /// Return whether this declaration keeps the legacy Host allocation
    /// behavior.
    ///
    /// Before roles and allocation policies existed, Host fixed IPs outside
    /// every managed prefix used `static-assignments`, and
    /// `network_segment_type` only narrowed the first DHCP segment selection.
    /// An explicit policy or either DPU role opts into the generalized
    /// ExpectedInterface contract instead.
    pub fn uses_legacy_host_allocation(&self) -> bool {
        self.role.is_host() && self.ip_allocation.is_none()
    }

    /// Return whether an inferred Fixed policy keeps the legacy
    /// `static-assignments` fallback.
    ///
    /// Host declarations had this behavior before allocation policies existed.
    /// Host BMC uses the same omission as the compatibility `Auto` policy, but
    /// a typed segment guard still requires a containing managed prefix.
    pub fn allows_static_assignments_fallback(&self) -> bool {
        self.ip_allocation.is_none()
            && (self.role.is_host()
                || (self.role.is_host_bmc() && self.network_segment_type.is_none()))
    }

    /// Return the typed segment guard for declarations using the generalized
    /// ExpectedInterface policy contract.
    ///
    /// Before roles and allocation policies existed, a Host declaration could
    /// use `network_segment_type` only to narrow initial DHCP selection. Keep
    /// that behavior when both new fields are omitted. An explicit policy or a
    /// DPU role opts into the same guard semantics for every allocation path.
    pub fn segment_type_guard(&self) -> Option<NetworkSegmentType> {
        self.network_segment_type
            .filter(|_| !self.uses_legacy_host_allocation())
    }

    /// Return the network segment type that narrows Dynamic or Retained DHCP
    /// segment selection, if the declaration names one. Prefers the typed
    /// [`Self::network_segment_type`]; otherwise maps the legacy
    /// [`Self::nic_type`] string so machines declared before the typed field
    /// keep their segment. `None` -> selection stays with whatever segment(s)
    /// the relay's prefix matches.
    pub fn resolved_network_segment_type(&self) -> Option<NetworkSegmentType> {
        if let Some(segment_type) = self.network_segment_type {
            return Some(segment_type);
        }
        // Legacy `nic_type` mapping -- droppable once declarations carry the
        // typed field. `bf3`/`dpu`/`onboard` named the admin segment, `bmc`/`oob`
        // the underlay; anything else left selection to the relay.
        match self.nic_type.as_deref()?.to_ascii_lowercase().as_str() {
            "bf3" | "dpu" | "onboard" => Some(NetworkSegmentType::Admin),
            "bmc" | "oob" => Some(NetworkSegmentType::Underlay),
            _ => None,
        }
    }
}

fn deserialize_optional_ip_addr_lossy<'de, D>(deserializer: D) -> Result<Option<IpAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?
        .and_then(|address| address.parse::<IpAddr>().ok()))
}

// Important : new fields for expected machine should be Optional _and_ #[serde(default)],
// unless you want to go update all the files in each production deployment that autoload
// the expected machines on api startup
#[derive(Clone, Deserialize)]
pub struct ExpectedMachine {
    #[serde(default)]
    pub id: Option<Uuid>,
    pub bmc_mac_address: MacAddress,
    #[serde(flatten)]
    pub data: ExpectedMachineData,
}

impl ExpectedMachine {
    /// Build the Host BMC declaration represented by the compatibility
    /// columns.
    ///
    /// An omitted nested policy is intentional here. It preserves the legacy
    /// external-address fallback for Fixed and resolves an addressless BMC to
    /// Retained.
    fn compatibility_host_bmc(&self) -> ExpectedInterface {
        let resolved = self
            .data
            .bmc_ip_allocation
            .resolved(self.data.bmc_ip_address.is_some());
        let fixed_ip = (resolved == ExpectedInterfaceIpAllocation::Fixed)
            .then_some(self.data.bmc_ip_address)
            .flatten();
        let ip_allocation = match self.data.bmc_ip_allocation {
            BmcIpAllocationType::Auto => None,
            BmcIpAllocationType::Fixed if fixed_ip.is_some() => None,
            BmcIpAllocationType::Dynamic => Some(ExpectedInterfaceIpAllocation::Dynamic),
            BmcIpAllocationType::Fixed => Some(ExpectedInterfaceIpAllocation::Fixed),
            BmcIpAllocationType::Retained => Some(ExpectedInterfaceIpAllocation::Retained),
        };

        ExpectedInterface {
            mac_address: self.bmc_mac_address,
            role: ExpectedInterfaceRole::HostBmc,
            ip_allocation,
            fixed_ip,
            ..Default::default()
        }
    }

    /// Return the Host BMC configuration used by readers and runtime paths.
    ///
    /// New writers keep the nested declaration and compatibility columns in
    /// sync. Older writers may update only the columns, so they win when the
    /// two representations disagree. Nested-only fields such as
    /// `network_segment_type` remain in place.
    pub fn effective_host_bmc(&self) -> ExpectedInterface {
        let compatibility = self.compatibility_host_bmc();
        let mut host_bmc = self
            .data
            .interfaces
            .iter()
            .find(|interface| interface.role.is_host_bmc())
            .cloned()
            .unwrap_or_else(|| compatibility.clone());

        let allocation_agrees_with_compatibility = match host_bmc.ip_allocation {
            Some(policy) => self.data.bmc_ip_allocation == policy.into(),
            None => compatibility.ip_allocation.is_none(),
        };
        let agrees_with_compatibility = host_bmc.mac_address == self.bmc_mac_address
            && allocation_agrees_with_compatibility
            && host_bmc.resolved_ip_allocation() == compatibility.resolved_ip_allocation()
            && host_bmc.fixed_ip == compatibility.fixed_ip;
        if !agrees_with_compatibility {
            host_bmc.ip_allocation = compatibility.ip_allocation;
            host_bmc.fixed_ip = compatibility.fixed_ip;
        }

        host_bmc.mac_address = self.bmc_mac_address;
        host_bmc.role = ExpectedInterfaceRole::HostBmc;
        host_bmc.primary = None;
        host_bmc
    }

    /// `interface_for_initial_allocation` selects the declaration for one MAC
    /// and address family without changing its explicit or inferred policy.
    /// Callers must separately check whether this family has already allocated.
    /// An unmatched MAC returns `None`.
    ///
    /// The owning BMC MAC uses [`Self::effective_host_bmc`]. Other MACs prefer
    /// a Fixed address in the requested family, then an addressless declaration,
    /// then a Fixed declaration for the other family. Equally applicable
    /// declarations retain their list order.
    ///
    /// A Fixed address in the other family supplies only interface metadata;
    /// the caller must use normal DHCP for the requested family. Keeping the
    /// original declaration preserves legacy segment and external-IP behavior.
    pub fn interface_for_initial_allocation(
        &self,
        mac_address: MacAddress,
        address_family: IpAddressFamily,
    ) -> Option<ExpectedInterface> {
        if mac_address == self.bmc_mac_address {
            return Some(self.effective_host_bmc());
        }

        let mut interfaces = self
            .data
            .interfaces
            .iter()
            .filter(|interface| interface.mac_address == mac_address);
        interfaces
            .clone()
            .find(|interface| {
                interface
                    .fixed_ip
                    .is_some_and(|address| address.is_address_family(address_family))
            })
            .or_else(|| {
                interfaces
                    .clone()
                    .find(|interface| interface.fixed_ip.is_none())
            })
            .or_else(|| interfaces.next())
            .cloned()
    }

    /// Return the top-level BMC policy that compatibility readers should see.
    ///
    /// Inferred policies remain absent/Auto. Explicit nested policies and
    /// explicit legacy policies retain their concrete wire value.
    pub fn compatibility_bmc_ip_allocation(&self) -> Option<BmcIpAllocationType> {
        self.effective_host_bmc()
            .ip_allocation
            .map(Into::into)
            .or_else(|| {
                (self.data.bmc_ip_allocation != BmcIpAllocationType::Auto)
                    .then_some(self.data.bmc_ip_allocation)
            })
    }

    /// Normalize one Host BMC declaration and its compatibility columns.
    ///
    /// An incoming nested declaration is the baseline. Older clients may omit
    /// it, so updates fall back to the previous effective declaration before
    /// using the incoming compatibility fields. Explicit legacy fields are
    /// applied last as overrides.
    pub fn normalize_host_bmc(
        &mut self,
        previous: Option<&ExpectedMachine>,
        mut overrides: LegacyHostBmcOverrides,
    ) -> Result<(), &'static str> {
        let mut host_bmc_indexes = self
            .data
            .interfaces
            .iter()
            .enumerate()
            .filter(|(_, interface)| interface.role.is_host_bmc())
            .map(|(index, _)| index);
        let host_bmc_index = host_bmc_indexes.next();
        if host_bmc_indexes.next().is_some() {
            return Err("at most one role=host_bmc interface may be configured");
        }

        let incoming_host_bmc = host_bmc_index.map(|index| self.data.interfaces[index].clone());
        let store_host_bmc = incoming_host_bmc.is_some()
            || (!overrides.replace_interfaces
                && previous.is_some_and(|machine| {
                    machine
                        .data
                        .interfaces
                        .iter()
                        .any(|interface| interface.role.is_host_bmc())
                }));
        let previous_host_bmc = previous.map(ExpectedMachine::effective_host_bmc);
        let previous_compatibility_allocation =
            previous.and_then(ExpectedMachine::compatibility_bmc_ip_allocation);
        let mut host_bmc = incoming_host_bmc
            .clone()
            .or_else(|| {
                previous.map(|machine| {
                    if overrides.replace_interfaces {
                        machine.compatibility_host_bmc()
                    } else {
                        machine.effective_host_bmc()
                    }
                })
            })
            .unwrap_or_else(|| self.compatibility_host_bmc());

        if let Some(previous_host_bmc) = previous_host_bmc.as_ref() {
            // Full-update clients commonly echo the compatibility fields from
            // a read. Some also drop interface roles they do not understand.
            // Matching values are projections, not new overrides; ignoring
            // them preserves details such as an explicit Fixed policy's
            // managed-prefix requirement.
            let matching_address_projection = overrides.ip_address
                == Some(previous_host_bmc.fixed_ip)
                && (overrides.ip_allocation.is_none()
                    || overrides.ip_allocation == previous_compatibility_allocation);
            if matching_address_projection {
                overrides.ip_address = None;
            }
            // A different address makes the compatibility fields a new
            // override pair, so keep its allocation even when that one value
            // happens to match the previous policy.
            let matching_allocation_projection = overrides.ip_address.is_none()
                && overrides.ip_allocation.is_some()
                && overrides.ip_allocation == previous_compatibility_allocation;
            if matching_allocation_projection {
                overrides.ip_allocation = None;
            }
        }

        // Preserve the compatibility representation as well as its resolved
        // policy. In particular, Auto-with-an-address and explicit legacy
        // Fixed both resolve to Fixed, but only the latter should remain
        // present on the compatibility wire field.
        let mut compatibility_allocation =
            if let Some(incoming_host_bmc) = incoming_host_bmc.as_ref() {
                if let Some(ip_allocation) = incoming_host_bmc.ip_allocation {
                    ip_allocation.into()
                } else if previous_host_bmc
                    .as_ref()
                    .is_some_and(|previous_host_bmc| previous_host_bmc.ip_allocation.is_none())
                {
                    // An omitted nested policy preserves its compatibility
                    // value even when another nested-only field changes.
                    previous
                        .map(|machine| machine.data.bmc_ip_allocation)
                        .unwrap_or(BmcIpAllocationType::Auto)
                } else if previous_host_bmc.as_ref().is_some_and(|previous_host_bmc| {
                    let mut normalized_incoming = incoming_host_bmc.clone();
                    normalized_incoming.mac_address = self.bmc_mac_address;
                    normalized_incoming.role = ExpectedInterfaceRole::HostBmc;
                    normalized_incoming.primary = None;
                    normalized_incoming == *previous_host_bmc
                }) {
                    previous
                        .map(|machine| machine.data.bmc_ip_allocation)
                        .unwrap_or(BmcIpAllocationType::Auto)
                } else {
                    BmcIpAllocationType::Auto
                }
            } else if let Some(previous) = previous {
                previous.data.bmc_ip_allocation
            } else {
                self.data.bmc_ip_allocation
            };

        if let Some(ip_address) = overrides.ip_address {
            host_bmc.fixed_ip = ip_address;
            if overrides.ip_allocation.is_none() {
                host_bmc.ip_allocation = None;
                compatibility_allocation = BmcIpAllocationType::Auto;
            }
        }

        if let Some(ip_allocation) = overrides.ip_allocation {
            compatibility_allocation = ip_allocation;
            host_bmc.ip_allocation = match ip_allocation {
                BmcIpAllocationType::Auto => None,
                BmcIpAllocationType::Dynamic => {
                    if !matches!(overrides.ip_address, Some(Some(_))) {
                        host_bmc.fixed_ip = None;
                    }
                    Some(ExpectedInterfaceIpAllocation::Dynamic)
                }
                BmcIpAllocationType::Fixed if host_bmc.fixed_ip.is_some() => None,
                BmcIpAllocationType::Fixed => Some(ExpectedInterfaceIpAllocation::Fixed),
                BmcIpAllocationType::Retained => {
                    if !matches!(overrides.ip_address, Some(Some(_))) {
                        host_bmc.fixed_ip = None;
                    }
                    Some(ExpectedInterfaceIpAllocation::Retained)
                }
            };
        }

        host_bmc.mac_address = self.bmc_mac_address;
        host_bmc.role = ExpectedInterfaceRole::HostBmc;
        host_bmc.primary = None;
        if let Err(message) = host_bmc.validate_ip_allocation() {
            if overrides.ip_address.is_some() || overrides.ip_allocation.is_some() {
                let legacy_allocation: BmcIpAllocationType =
                    host_bmc.resolved_ip_allocation().into();
                return legacy_allocation.validate(host_bmc.fixed_ip.is_some());
            }
            return Err(message);
        }

        self.data.bmc_ip_allocation = compatibility_allocation;
        self.data.bmc_ip_address = host_bmc.fixed_ip;

        if let Some(index) = host_bmc_index {
            self.data.interfaces[index] = host_bmc;
        } else if store_host_bmc {
            self.data.interfaces.push(host_bmc);
        }

        Ok(())
    }
}

#[derive(Clone, Default, Deserialize)] // Do not add Debug here, it contains password
pub struct ExpectedMachineData {
    pub bmc_username: String,
    pub bmc_password: String,
    pub serial_number: String,
    #[serde(default)]
    pub fallback_dpu_serial_numbers: Vec<String>,
    #[serde(default)]
    pub sku_id: Option<String>,
    #[serde(default)]
    pub metadata: Metadata,
    /// Interfaces NICo may encounter while ingesting this machine.
    ///
    /// `host_nics` remains accepted so existing configuration files continue
    /// to load without changes.
    #[serde(default, alias = "host_nics")]
    pub interfaces: Vec<ExpectedInterface>,
    pub rack_id: Option<RackId>,
    pub default_pause_ingestion_and_poweron: Option<bool>,
    pub dpf_enabled: Option<bool>,
    /// When set, the API pre-allocates a `machine_interface` for this BMC MAC at this address
    /// (same pattern as expected switches / power shelves) so Site Explorer can reach the BMC
    /// without DHCP. IPs outside Carbide-managed prefixes land on the `static-assignments` segment.
    #[serde(default)]
    pub bmc_ip_address: Option<IpAddr>,
    /// When true, site-explorer skips BMC password rotation and stores the
    /// factory-default credentials in Vault as-is.
    #[serde(default)]
    pub bmc_retain_credentials: Option<bool>,
    /// Per-host DPU policy. The default [`HostDpuPolicy::Manage`] inherits the
    /// site policy. The legacy `dpu_mode` field and its values remain accepted
    /// during deserialization.
    ///
    /// This type is deserialization-only. The nested flattening is intentional
    /// for dual-key compatibility and is incompatible with `deny_unknown_fields`.
    #[serde(flatten, deserialize_with = "deserialize_host_dpu_policy")]
    pub dpu_policy: HostDpuPolicy,
    /// Per-host control over how this BMC's IP is assigned and retained.
    /// Defaults to `BmcIpAllocationType::Auto`, which infers `Fixed` from a
    /// configured `bmc_ip_address` and otherwise `Retained` (pins an
    /// auto-allocated address as Static so it survives DHCP lease expiry).
    #[serde(default)]
    pub bmc_ip_allocation: BmcIpAllocationType,
    /// Per-host profile for settings that affect state-machine progression.
    /// Stored as a JSONB column on `expected_machines`; future state-machine
    /// knobs should be added here rather than as new flat columns.
    #[serde(default)]
    pub host_lifecycle_profile: HostLifecycleProfile,
}
// Important : new fields for expected machine (and data) should be optional _and_ serde(default),
// unless you want to go update all the files in each production deployment that autoload
// the expected machines on api startup

impl ExpectedMachineData {
    /// The MAC the operator declared as this host's boot interface via
    /// `ExpectedInterface.primary`. This is the single source of declared boot
    /// intent the writers consult -- site-explorer ingestion, DHCP, and
    /// prediction promotion -- so they all agree on which NIC wins. The API
    /// enforces at most one `primary` host NIC, so the first match is the
    /// declaration. `None` leaves the boot interface to today's automation
    /// (DPU takeover during ingestion, else the `pick_boot_interface` fallback).
    pub fn declared_primary_mac(&self) -> Option<MacAddress> {
        self.interfaces
            .iter()
            .find(|interface| interface.role.is_host() && interface.primary == Some(true))
            .map(|interface| interface.mac_address)
    }
}

/// Per-host lifecycle profile for settings that affect state-machine progression.
/// `Option<bool>` fields support CLI patch semantics (`None` = not specified,
/// keep existing DB value via `COALESCE`). Converts to the runtime `HostProfile`
/// (plain `bool` fields) at machine discovery time.
#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostLifecycleProfile {
    /// If true, do not lock down the server as part of lifecycle management within the state machine.
    /// If unset or false, preserve the default behavior of locking down the server after configuring the BIOS.
    #[serde(default)]
    pub disable_lockdown: Option<bool>,
}

impl HostLifecycleProfile {
    /// Returns `true` when every field is `None`, meaning the caller did not
    /// specify any profile value. Used by the UPDATE path to send SQL `NULL`
    /// so that `COALESCE` preserves the existing DB row.
    pub fn is_empty(&self) -> bool {
        self.disable_lockdown.is_none()
    }
}

impl<'r> FromRow<'r, PgRow> for ExpectedMachine {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        let labels: sqlx::types::Json<HashMap<String, String>> = row.try_get("metadata_labels")?;
        let metadata = Metadata {
            name: row.try_get("metadata_name")?,
            description: row.try_get("metadata_description")?,
            labels: labels.0,
        };

        let json: sqlx::types::Json<Vec<ExpectedInterface>> = row.try_get("host_nics")?;
        let interfaces: Vec<ExpectedInterface> = json.0;

        Ok(ExpectedMachine {
            id: row.try_get("id")?,
            bmc_mac_address: row.try_get("bmc_mac_address")?,
            data: ExpectedMachineData {
                bmc_username: row.try_get("bmc_username")?,
                serial_number: row.try_get("serial_number")?,
                bmc_password: row.try_get("bmc_password")?,
                fallback_dpu_serial_numbers: row.try_get("fallback_dpu_serial_numbers")?,
                metadata,
                sku_id: row.try_get("sku_id")?,
                rack_id: row.try_get("rack_id")?,
                interfaces,
                default_pause_ingestion_and_poweron: row
                    .try_get("default_pause_ingestion_and_poweron")?,
                dpf_enabled: row.try_get("dpf_enabled")?,
                bmc_ip_address: row.try_get("bmc_ip_address")?,
                bmc_retain_credentials: row.try_get("bmc_retain_credentials")?,
                dpu_policy: row.try_get("dpu_mode")?,
                bmc_ip_allocation: row.try_get("bmc_ip_allocation")?,
                host_lifecycle_profile: row
                    .try_get::<sqlx::types::Json<HostLifecycleProfile>, _>("host_lifecycle_profile")
                    .map(|j| j.0)?,
            },
        })
    }
}

#[derive(FromRow)]
pub struct LinkedExpectedMachine {
    pub serial_number: String,
    pub bmc_mac_address: MacAddress, // from expected_machines table
    pub interface_id: Option<MachineInterfaceId>, // from machine_interfaces table
    pub address: Option<IpAddr>,     // The explored endpoint
    pub machine_id: Option<MachineId>, // The machine
    pub expected_machine_id: Option<Uuid>, // The expected machine ID
}

/// A host BMC endpoint that was explored by Site Explorer but is not listed
/// in any of the `expected_machines`, `expected_power_shelf`, or
/// `expected_switch` tables. DPUs, power shelves, and switches are filtered
/// out of this list; it only contains host BMCs.
pub struct UnexpectedMachine {
    pub address: IpAddr,
    pub bmc_mac_address: MacAddress,
    pub machine_id: Option<MachineId>,
}

// default_uuid removed; ids are optional to support legacy rows with NULL ids

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Check, check_values, scenarios, value_scenarios};

    use super::*;

    #[test]
    fn host_dpu_policy_expects_managed_dpus() {
        value_scenarios!(
            run = HostDpuPolicy::expects_managed_dpus;
            "manage" {
                HostDpuPolicy::Manage => true,
            }
            "use as NIC" {
                HostDpuPolicy::Nic => false,
            }
            "ignore" {
                HostDpuPolicy::Ignore => false,
            }
        );
    }

    #[test]
    fn host_dpu_policy_deserializes_new_and_legacy_values() {
        scenarios!(
            run = |json| serde_json::from_str::<HostDpuPolicy>(json).map_err(drop);
            "canonical policy values" {
                r#""manage""# => Yields(HostDpuPolicy::Manage),
                r#""nic""# => Yields(HostDpuPolicy::Nic),
                r#""ignore""# => Yields(HostDpuPolicy::Ignore),
            }

            "compatibility values" {
                r#""use_as_nic""# => Yields(HostDpuPolicy::Nic),
                r#""dpu_mode""# => Yields(HostDpuPolicy::Manage),
                r#""nic_mode""# => Yields(HostDpuPolicy::Nic),
                r#""no_dpu""# => Yields(HostDpuPolicy::Ignore),
            }
        );
    }

    #[test]
    fn host_dpu_policy_serializes_and_round_trips_canonical_values() {
        scenarios!(
            run = |policy| {
                let json = serde_json::to_string(&policy).map_err(drop)?;
                let recovered = serde_json::from_str::<HostDpuPolicy>(&json).map_err(drop)?;
                Ok::<_, ()>((json, recovered))
            };
            "default/manage" {
                HostDpuPolicy::default() => Yields((
                    r#""manage""#.to_string(),
                    HostDpuPolicy::Manage,
                )),
            }

            "NIC" {
                HostDpuPolicy::Nic => Yields((
                    r#""nic""#.to_string(),
                    HostDpuPolicy::Nic,
                )),
            }

            "ignore" {
                HostDpuPolicy::Ignore => Yields((
                    r#""ignore""#.to_string(),
                    HostDpuPolicy::Ignore,
                )),
            }
        );
    }

    #[test]
    fn expected_machine_deserializes_new_and_legacy_policy_fields() {
        scenarios!(
            run = |json| {
                serde_json::from_str::<ExpectedMachine>(json)
                    .map(|em| em.data.dpu_policy)
                    .map_err(drop)
            };
            "policy omitted" {
                r#"{
                    "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1"
                }"# => Yields(HostDpuPolicy::Manage),
            }

            "canonical field and value" {
                r#"{
                    "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1",
                    "dpu_policy": "nic"
                }"# => Yields(HostDpuPolicy::Nic),
            }

            "previous canonical field value remains accepted" {
                r#"{
                    "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1",
                    "dpu_policy": "use_as_nic"
                }"# => Yields(HostDpuPolicy::Nic),
            }

            "legacy field and value" {
                r#"{
                    "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1",
                    "dpu_mode": "no_dpu"
                }"# => Yields(HostDpuPolicy::Ignore),
            }

            "matching new and legacy fields" {
                r#"{
                    "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1",
                    "dpu_policy": "nic",
                    "dpu_mode": "nic_mode"
                }"# => Yields(HostDpuPolicy::Nic),
            }

            "new field wins over conflicting legacy field" {
                r#"{
                    "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1",
                    "dpu_policy": "ignore",
                    "dpu_mode": "dpu_mode"
                }"# => Yields(HostDpuPolicy::Ignore),
            }

            "explicit manage wins when legacy field comes first" {
                r#"{
                    "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1",
                    "dpu_mode": "nic_mode",
                    "dpu_policy": "manage"
                }"# => Yields(HostDpuPolicy::Manage),
            }
        );
    }

    #[test]
    fn expected_machine_deserializes_new_and_legacy_interface_fields() {
        let expected = || {
            vec![ExpectedInterface {
                mac_address: "02:00:00:00:20:01".parse().unwrap(),
                fixed_ip: Some("192.0.2.10".parse().unwrap()),
                ..Default::default()
            }]
        };

        scenarios!(
            run = |interfaces_json| {
                let json = format!(
                    r#"{{
                        "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                        "bmc_username": "root",
                        "bmc_password": "pass",
                        "serial_number": "SN-1"
                        {interfaces_json}
                    }}"#,
                );
                serde_json::from_str::<ExpectedMachine>(&json)
                    .map(|machine| machine.data.interfaces)
                    .map_err(drop)
            };
            "interfaces omitted" {
                "" => Yields(Vec::new()),
            }

            "canonical interfaces field" {
                r#", "interfaces": [{
                    "mac_address": "02:00:00:00:20:01",
                    "fixed_ip": "192.0.2.10"
                }]"# => Yields(expected()),
            }

            "legacy host_nics field" {
                r#", "host_nics": [{
                    "mac_address": "02:00:00:00:20:01",
                    "fixed_ip": "192.0.2.10"
                }]"# => Yields(expected()),
            }

            "both spellings in one machine are ambiguous" {
                r#", "interfaces": [], "host_nics": []"# => Fails,
            }
        );
    }

    /// One `expected_machines.json` file may migrate entries independently.
    #[test]
    fn expected_machine_list_accepts_mixed_interface_field_names() {
        let machines = serde_json::from_str::<Vec<ExpectedMachine>>(
            r#"[
                {
                    "bmc_mac_address": "AA:BB:CC:DD:EE:01",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-1",
                    "interfaces": [{
                        "mac_address": "02:00:00:00:20:01"
                    }]
                },
                {
                    "bmc_mac_address": "AA:BB:CC:DD:EE:02",
                    "bmc_username": "root",
                    "bmc_password": "pass",
                    "serial_number": "SN-2",
                    "host_nics": [{
                        "mac_address": "02:00:00:00:20:02"
                    }]
                }
            ]"#,
        )
        .unwrap();

        assert_eq!(
            machines
                .iter()
                .map(|machine| machine.data.interfaces[0].mac_address)
                .collect::<Vec<_>>(),
            vec![
                "02:00:00:00:20:01".parse().unwrap(),
                "02:00:00:00:20:02".parse().unwrap(),
            ],
        );
    }

    /// JSON deserialization of `ExpectedMachine`, projecting to the
    /// `host_lifecycle_profile.disable_lockdown` field under test. A missing
    /// `host_lifecycle_profile` defaults to `None` (equivalent to
    /// `HostLifecycleProfile::default()`, whose only field is `disable_lockdown`).
    #[test]
    fn host_lifecycle_profile_deserializes_from_json() {
        scenarios!(
            // serde_json::Error is not PartialEq, so discard it on the error path.
            run = |json| {
                serde_json::from_str::<ExpectedMachine>(json)
                    .map(|em| em.data.host_lifecycle_profile.disable_lockdown)
                    .map_err(drop)
            };
            "missing host_lifecycle_profile defaults to None" {
                r#"{
                            "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                            "bmc_username": "root",
                            "bmc_password": "pass",
                            "serial_number": "SN-1"
                        }"# => Yields(None),
            }

            "present host_lifecycle_profile parses disable_lockdown" {
                r#"{
                            "bmc_mac_address": "AA:BB:CC:DD:EE:FF",
                            "bmc_username": "root",
                            "bmc_password": "pass",
                            "serial_number": "SN-1",
                            "host_lifecycle_profile": {"disable_lockdown": true}
                        }"# => Yields(Some(true)),
            }
        );
    }

    #[test]
    fn expected_interface_deserializes_valid_fixed_gateway() {
        let json = r#"{
            "mac_address": "AA:BB:CC:DD:EE:FF",
            "fixed_gateway": "2001:db8::1"
        }"#;
        let nic: ExpectedInterface = serde_json::from_str(json).unwrap();

        assert_eq!(nic.fixed_gateway, Some("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn expected_interface_drops_invalid_fixed_gateway_on_deserialize() {
        let json = r#"{
            "mac_address": "AA:BB:CC:DD:EE:FF",
            "fixed_gateway": "not-an-ip"
        }"#;
        let nic: ExpectedInterface = serde_json::from_str(json).unwrap();

        assert_eq!(nic.fixed_gateway, None);
    }

    #[test]
    fn expected_host_nic_alias_remains_source_compatible() {
        let legacy: ExpectedHostNic = ExpectedInterface {
            mac_address: "AA:BB:CC:DD:EE:FF".parse().unwrap(),
            ..Default::default()
        };

        assert_eq!(legacy.mac_address.to_string(), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn expected_interface_role_preserves_legacy_json_format() {
        let legacy = r#"{
            "mac_address": "AA:BB:CC:DD:EE:FF",
            "nic_type": "dpu"
        }"#;
        let interface: ExpectedInterface = serde_json::from_str(legacy).unwrap();

        assert_eq!(interface.role, ExpectedInterfaceRole::Host);
        assert_eq!(interface.ip_allocation, None);
        let serialized = serde_json::to_value(interface).unwrap();
        assert_eq!(serialized.get("role"), None);
        assert_eq!(serialized.get("ip_allocation"), None);
    }

    #[test]
    fn expected_interface_roles_use_canonical_names() {
        check_values(
            [
                Check {
                    scenario: "Host remains omitted from stored JSON",
                    input: ExpectedInterfaceRole::Host,
                    expect: ("host".to_string(), None),
                },
                Check {
                    scenario: "DPU OS uses its canonical name",
                    input: ExpectedInterfaceRole::DpuOs,
                    expect: ("dpu_os".to_string(), Some(serde_json::json!("dpu_os"))),
                },
                Check {
                    scenario: "DPU BMC uses its canonical name",
                    input: ExpectedInterfaceRole::DpuBmc,
                    expect: ("dpu_bmc".to_string(), Some(serde_json::json!("dpu_bmc"))),
                },
                Check {
                    scenario: "Host BMC uses its canonical name",
                    input: ExpectedInterfaceRole::HostBmc,
                    expect: ("host_bmc".to_string(), Some(serde_json::json!("host_bmc"))),
                },
            ],
            |role| {
                let serialized = serde_json::to_value(ExpectedInterface {
                    role,
                    ..Default::default()
                })
                .unwrap();
                (role.to_string(), serialized.get("role").cloned())
            },
        );
    }

    #[test]
    fn expected_interface_roles_map_to_interface_behavior() {
        check_values(
            [
                Check {
                    scenario: "legacy host",
                    input: ExpectedInterfaceRole::Host,
                    expect: (true, InterfaceType::Data),
                },
                Check {
                    scenario: "DPU OS",
                    input: ExpectedInterfaceRole::DpuOs,
                    expect: (false, InterfaceType::Data),
                },
                Check {
                    scenario: "DPU BMC",
                    input: ExpectedInterfaceRole::DpuBmc,
                    expect: (false, InterfaceType::Bmc),
                },
                Check {
                    scenario: "Host BMC",
                    input: ExpectedInterfaceRole::HostBmc,
                    expect: (false, InterfaceType::Bmc),
                },
            ],
            |role| (role.is_host(), role.interface_type()),
        );
    }

    #[test]
    fn expected_interface_roles_resolve_role_primary_state() {
        check_values(
            [
                Check {
                    scenario: "legacy Host keeps machine-wide selection",
                    input: ExpectedInterfaceRole::Host,
                    expect: None,
                },
                Check {
                    scenario: "DPU OS is always primary",
                    input: ExpectedInterfaceRole::DpuOs,
                    expect: Some(true),
                },
                Check {
                    scenario: "DPU BMC is never primary",
                    input: ExpectedInterfaceRole::DpuBmc,
                    expect: Some(false),
                },
                Check {
                    scenario: "Host BMC is never primary",
                    input: ExpectedInterfaceRole::HostBmc,
                    expect: Some(false),
                },
            ],
            ExpectedInterfaceRole::primary_interface_override,
        );
    }

    #[test]
    fn expected_interface_ip_allocation_infers_and_validates_policy() {
        struct Declaration {
            policy: Option<ExpectedInterfaceIpAllocation>,
            fixed_ip: Option<IpAddr>,
        }

        let fixed_ip = Some("192.0.2.10".parse().unwrap());
        check_values(
            [
                Check {
                    scenario: "omitted policy without fixed IP infers dynamic",
                    input: Declaration {
                        policy: None,
                        fixed_ip: None,
                    },
                    expect: (ExpectedInterfaceIpAllocation::Dynamic, None),
                },
                Check {
                    scenario: "omitted policy with fixed IP infers fixed",
                    input: Declaration {
                        policy: None,
                        fixed_ip,
                    },
                    expect: (ExpectedInterfaceIpAllocation::Fixed, None),
                },
                Check {
                    scenario: "explicit dynamic without fixed IP is valid",
                    input: Declaration {
                        policy: Some(ExpectedInterfaceIpAllocation::Dynamic),
                        fixed_ip: None,
                    },
                    expect: (ExpectedInterfaceIpAllocation::Dynamic, None),
                },
                Check {
                    scenario: "explicit dynamic with fixed IP is rejected",
                    input: Declaration {
                        policy: Some(ExpectedInterfaceIpAllocation::Dynamic),
                        fixed_ip,
                    },
                    expect: (
                        ExpectedInterfaceIpAllocation::Dynamic,
                        Some("ip_allocation=dynamic cannot be combined with fixed_ip"),
                    ),
                },
                Check {
                    scenario: "explicit fixed with fixed IP is valid",
                    input: Declaration {
                        policy: Some(ExpectedInterfaceIpAllocation::Fixed),
                        fixed_ip,
                    },
                    expect: (ExpectedInterfaceIpAllocation::Fixed, None),
                },
                Check {
                    scenario: "explicit fixed without fixed IP is rejected",
                    input: Declaration {
                        policy: Some(ExpectedInterfaceIpAllocation::Fixed),
                        fixed_ip: None,
                    },
                    expect: (
                        ExpectedInterfaceIpAllocation::Fixed,
                        Some("ip_allocation=fixed requires fixed_ip"),
                    ),
                },
                Check {
                    scenario: "explicit retained without fixed IP is valid",
                    input: Declaration {
                        policy: Some(ExpectedInterfaceIpAllocation::Retained),
                        fixed_ip: None,
                    },
                    expect: (ExpectedInterfaceIpAllocation::Retained, None),
                },
                Check {
                    scenario: "explicit retained with fixed IP is rejected",
                    input: Declaration {
                        policy: Some(ExpectedInterfaceIpAllocation::Retained),
                        fixed_ip,
                    },
                    expect: (
                        ExpectedInterfaceIpAllocation::Retained,
                        Some("ip_allocation=retained cannot be combined with fixed_ip; use fixed"),
                    ),
                },
            ],
            |declaration| {
                let interface = ExpectedInterface {
                    mac_address: "AA:BB:CC:DD:EE:FF".parse().unwrap(),
                    ip_allocation: declaration.policy,
                    fixed_ip: declaration.fixed_ip,
                    ..Default::default()
                };
                (
                    interface.resolved_ip_allocation(),
                    interface.validate_ip_allocation().err(),
                )
            },
        );

        let host_bmc = ExpectedInterface {
            role: ExpectedInterfaceRole::HostBmc,
            ..Default::default()
        };
        assert_eq!(
            host_bmc.resolved_ip_allocation(),
            ExpectedInterfaceIpAllocation::Retained,
        );
    }

    /// Fixed reservation callers share one policy-and-address invariant.
    #[test]
    fn expected_interface_fixed_reservation_ip_validates_policy() {
        let fixed_ip = "192.0.2.10".parse().unwrap();
        check_values(
            [
                Check {
                    scenario: "legacy fixed address",
                    input: (None, Some(fixed_ip)),
                    expect: Ok(fixed_ip),
                },
                Check {
                    scenario: "explicit fixed address",
                    input: (Some(ExpectedInterfaceIpAllocation::Fixed), Some(fixed_ip)),
                    expect: Ok(fixed_ip),
                },
                Check {
                    scenario: "dynamic policy",
                    input: (Some(ExpectedInterfaceIpAllocation::Dynamic), None),
                    expect: Err("expected interface does not use fixed IP allocation"),
                },
                Check {
                    scenario: "fixed policy without an address",
                    input: (Some(ExpectedInterfaceIpAllocation::Fixed), None),
                    expect: Err("ip_allocation=fixed requires fixed_ip"),
                },
                Check {
                    scenario: "retained policy",
                    input: (Some(ExpectedInterfaceIpAllocation::Retained), None),
                    expect: Err("expected interface does not use fixed IP allocation"),
                },
            ],
            |(ip_allocation, fixed_ip)| {
                ExpectedInterface {
                    ip_allocation,
                    fixed_ip,
                    ..Default::default()
                }
                .fixed_reservation_ip()
            },
        );
    }

    #[test]
    fn expected_interface_ip_allocation_json_uses_canonical_names() {
        check_values(
            [
                Check {
                    scenario: "dynamic",
                    input: ExpectedInterfaceIpAllocation::Dynamic,
                    expect: r#""dynamic""#.to_string(),
                },
                Check {
                    scenario: "fixed",
                    input: ExpectedInterfaceIpAllocation::Fixed,
                    expect: r#""fixed""#.to_string(),
                },
                Check {
                    scenario: "retained",
                    input: ExpectedInterfaceIpAllocation::Retained,
                    expect: r#""retained""#.to_string(),
                },
            ],
            |policy| serde_json::to_string(&policy).unwrap(),
        );
    }

    #[test]
    fn host_lifecycle_profile_is_empty_when_all_fields_none() {
        let hlp = HostLifecycleProfile::default();
        assert!(hlp.is_empty());

        let hlp = HostLifecycleProfile {
            disable_lockdown: Some(true),
        };
        assert!(!hlp.is_empty());

        let hlp = HostLifecycleProfile {
            disable_lockdown: Some(false),
        };
        assert!(!hlp.is_empty());
    }

    /// `BmcIpAllocationType::validate` against whether a `bmc_ip_address` is
    /// configured, exhaustively over the four variants x has_address. Only three
    /// combinations are errors: `Fixed` without an address, and `Dynamic` /
    /// `Retained` with an address. `Auto` is always valid.
    #[test]
    fn bmc_ip_allocation_validate_covers_all_combinations() {
        struct Case {
            name: &'static str,
            mode: BmcIpAllocationType,
            has_address: bool,
            ok: bool,
        }

        let cases = [
            Case {
                name: "auto with address is valid",
                mode: BmcIpAllocationType::Auto,
                has_address: true,
                ok: true,
            },
            Case {
                name: "auto without address is valid",
                mode: BmcIpAllocationType::Auto,
                has_address: false,
                ok: true,
            },
            Case {
                name: "dynamic without address is valid",
                mode: BmcIpAllocationType::Dynamic,
                has_address: false,
                ok: true,
            },
            Case {
                name: "dynamic with address is rejected",
                mode: BmcIpAllocationType::Dynamic,
                has_address: true,
                ok: false,
            },
            Case {
                name: "fixed with address is valid",
                mode: BmcIpAllocationType::Fixed,
                has_address: true,
                ok: true,
            },
            Case {
                name: "fixed without address is rejected",
                mode: BmcIpAllocationType::Fixed,
                has_address: false,
                ok: false,
            },
            Case {
                name: "retained without address is valid",
                mode: BmcIpAllocationType::Retained,
                has_address: false,
                ok: true,
            },
            Case {
                name: "retained with address is rejected",
                mode: BmcIpAllocationType::Retained,
                has_address: true,
                ok: false,
            },
        ];

        for case in cases {
            assert_eq!(
                case.mode.validate(case.has_address).is_ok(),
                case.ok,
                "{}",
                case.name
            );
        }
    }

    /// `BmcIpAllocationType::retains_dynamic_ip` exhaustively over the four
    /// variants x has_address. `Retained` always retains; `Auto` retains only
    /// when there's no configured address; `Dynamic` and `Fixed` never retain.
    #[test]
    fn bmc_ip_allocation_retains_dynamic_ip_covers_all_combinations() {
        struct Case {
            name: &'static str,
            mode: BmcIpAllocationType,
            has_address: bool,
            retains: bool,
        }

        let cases = [
            Case {
                name: "auto with address does not retain",
                mode: BmcIpAllocationType::Auto,
                has_address: true,
                retains: false,
            },
            Case {
                name: "auto without address retains",
                mode: BmcIpAllocationType::Auto,
                has_address: false,
                retains: true,
            },
            Case {
                name: "dynamic without address does not retain",
                mode: BmcIpAllocationType::Dynamic,
                has_address: false,
                retains: false,
            },
            Case {
                name: "dynamic with address does not retain",
                mode: BmcIpAllocationType::Dynamic,
                has_address: true,
                retains: false,
            },
            Case {
                name: "fixed without address does not retain",
                mode: BmcIpAllocationType::Fixed,
                has_address: false,
                retains: false,
            },
            Case {
                name: "fixed with address does not retain",
                mode: BmcIpAllocationType::Fixed,
                has_address: true,
                retains: false,
            },
            Case {
                name: "retained without address retains",
                mode: BmcIpAllocationType::Retained,
                has_address: false,
                retains: true,
            },
            Case {
                name: "retained with address retains",
                mode: BmcIpAllocationType::Retained,
                has_address: true,
                retains: true,
            },
        ];

        for case in cases {
            assert_eq!(
                case.mode.retains_dynamic_ip(case.has_address),
                case.retains,
                "{}",
                case.name
            );
        }
    }

    /// The `BmcIpAllocationType` default is `Auto`, which the Unspecified wire
    /// mapping and the "infer from bmc_ip_address" behavior both rely on.
    #[test]
    fn bmc_ip_allocation_default_is_auto() {
        assert_eq!(BmcIpAllocationType::default(), BmcIpAllocationType::Auto);
    }

    #[test]
    fn initial_allocation_selects_family_without_rewriting_declarations() {
        let mac_address = "AA:BB:CC:DD:EE:01".parse().unwrap();
        let legacy_v4 = ExpectedInterface {
            mac_address,
            fixed_ip: Some("192.0.2.10".parse().unwrap()),
            fixed_mask: Some("255.255.255.0".to_string()),
            fixed_gateway: Some("192.0.2.1".parse().unwrap()),
            network_segment_type: Some(NetworkSegmentType::Admin),
            nic_type: Some("onboard".to_string()),
            primary: Some(true),
            ..Default::default()
        };
        let explicit_v6 = ExpectedInterface {
            mac_address,
            ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
            fixed_ip: Some("2001:db8::10".parse().unwrap()),
            network_segment_type: Some(NetworkSegmentType::Admin),
            ..Default::default()
        };
        let retained = ExpectedInterface {
            mac_address,
            ip_allocation: Some(ExpectedInterfaceIpAllocation::Retained),
            ..Default::default()
        };

        check_values(
            [
                Check {
                    scenario: "IPv6 selects the second Fixed declaration",
                    input: (
                        vec![legacy_v4.clone(), explicit_v6.clone()],
                        IpAddressFamily::Ipv6,
                    ),
                    expect: Some(explicit_v6.clone()),
                },
                Check {
                    scenario: "IPv4 selects the legacy declaration after IPv6",
                    input: (vec![explicit_v6, legacy_v4.clone()], IpAddressFamily::Ipv4),
                    expect: Some(legacy_v4.clone()),
                },
                Check {
                    scenario: "Retained applies to the family without a Fixed declaration",
                    input: (
                        vec![legacy_v4.clone(), retained.clone()],
                        IpAddressFamily::Ipv6,
                    ),
                    expect: Some(retained.clone()),
                },
                Check {
                    scenario: "Fixed takes precedence over an addressless declaration",
                    input: (vec![retained, legacy_v4.clone()], IpAddressFamily::Ipv4),
                    expect: Some(legacy_v4.clone()),
                },
                Check {
                    scenario: "Fixed in another family preserves metadata for DHCP fallback",
                    input: (vec![legacy_v4.clone()], IpAddressFamily::Ipv6),
                    expect: Some(legacy_v4),
                },
            ],
            |(interfaces, address_family)| {
                let machine = ExpectedMachine {
                    id: None,
                    bmc_mac_address: "AA:BB:CC:DD:EE:FF".parse().unwrap(),
                    data: ExpectedMachineData {
                        interfaces,
                        ..Default::default()
                    },
                };
                machine.interface_for_initial_allocation(mac_address, address_family)
            },
        );
    }

    #[test]
    fn initial_allocation_uses_effective_host_bmc_before_matching_mac() {
        let mac_address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let fixed_v4 = "192.0.2.10".parse().unwrap();
        check_values(
            [
                Check {
                    scenario: "legacy Dynamic override is authoritative",
                    input: (BmcIpAllocationType::Dynamic, None),
                    expect: ExpectedInterface {
                        mac_address,
                        role: ExpectedInterfaceRole::HostBmc,
                        ip_allocation: Some(ExpectedInterfaceIpAllocation::Dynamic),
                        network_segment_type: Some(NetworkSegmentType::Underlay),
                        ..Default::default()
                    },
                },
                Check {
                    scenario: "legacy Fixed IPv4 remains visible to the IPv6 caller",
                    input: (BmcIpAllocationType::Auto, Some(fixed_v4)),
                    expect: ExpectedInterface {
                        mac_address,
                        role: ExpectedInterfaceRole::HostBmc,
                        fixed_ip: Some(fixed_v4),
                        network_segment_type: Some(NetworkSegmentType::Underlay),
                        ..Default::default()
                    },
                },
            ],
            |(bmc_ip_allocation, bmc_ip_address)| {
                let machine = ExpectedMachine {
                    id: None,
                    bmc_mac_address: mac_address,
                    data: ExpectedMachineData {
                        bmc_ip_allocation,
                        bmc_ip_address,
                        interfaces: vec![
                            ExpectedInterface {
                                mac_address,
                                fixed_ip: Some("2001:db8::10".parse().unwrap()),
                                ..Default::default()
                            },
                            ExpectedInterface {
                                mac_address,
                                role: ExpectedInterfaceRole::HostBmc,
                                ip_allocation: Some(ExpectedInterfaceIpAllocation::Retained),
                                network_segment_type: Some(NetworkSegmentType::Underlay),
                                ..Default::default()
                            },
                        ],
                        ..Default::default()
                    },
                };
                machine
                    .interface_for_initial_allocation(mac_address, IpAddressFamily::Ipv6)
                    .expect("owning BMC should always have an effective declaration")
            },
        );
    }

    /// An authoritative interface replacement removes nested-only Host BMC
    /// settings while keeping the compatibility fields as the legacy baseline.
    #[test]
    fn normalize_host_bmc_honors_authoritative_interface_replacement() {
        let bmc_mac_address = "AA:BB:CC:DD:EE:FF".parse().unwrap();
        let fixed_ip = "192.0.2.20".parse().unwrap();
        let previous = ExpectedMachine {
            id: None,
            bmc_mac_address,
            data: ExpectedMachineData {
                interfaces: vec![ExpectedInterface {
                    mac_address: bmc_mac_address,
                    role: ExpectedInterfaceRole::HostBmc,
                    fixed_ip: Some(fixed_ip),
                    network_segment_type: Some(NetworkSegmentType::Underlay),
                    ..Default::default()
                }],
                bmc_ip_address: Some(fixed_ip),
                ..Default::default()
            },
        };
        let mut replacement = ExpectedMachine {
            id: None,
            bmc_mac_address,
            data: ExpectedMachineData {
                bmc_ip_address: Some(fixed_ip),
                ..Default::default()
            },
        };

        replacement
            .normalize_host_bmc(
                Some(&previous),
                LegacyHostBmcOverrides {
                    replace_interfaces: true,
                    ..Default::default()
                },
            )
            .unwrap();

        assert!(
            replacement
                .data
                .interfaces
                .iter()
                .all(|interface| !interface.role.is_host_bmc()),
        );
        let effective = replacement.effective_host_bmc();
        assert_eq!(effective.fixed_ip, Some(fixed_ip));
        assert_eq!(effective.network_segment_type, None);
    }

    /// `declared_primary_mac` returns the MAC of the one NIC flagged
    /// `primary: Some(true)`, and `None` when nothing is declared. `primary:
    /// Some(false)` is an explicit non-primary, not a declaration.
    #[test]
    fn declared_primary_mac_returns_the_flagged_nic() {
        let mac_a: MacAddress = "AA:BB:CC:00:00:01".parse().unwrap();
        let mac_b: MacAddress = "AA:BB:CC:00:00:02".parse().unwrap();

        let nic = |mac: MacAddress,
                   role: ExpectedInterfaceRole,
                   primary: Option<bool>|
         -> ExpectedInterface {
            ExpectedInterface {
                mac_address: mac,
                role,
                primary,
                ..Default::default()
            }
        };

        // Nothing declared -- empty, or only explicit non-primaries.
        assert_eq!(ExpectedMachineData::default().declared_primary_mac(), None);
        assert_eq!(
            ExpectedMachineData {
                interfaces: vec![
                    nic(mac_a, ExpectedInterfaceRole::Host, None),
                    nic(mac_b, ExpectedInterfaceRole::Host, Some(false)),
                ],
                ..Default::default()
            }
            .declared_primary_mac(),
            None
        );

        // The declared NIC wins.
        assert_eq!(
            ExpectedMachineData {
                interfaces: vec![
                    nic(mac_a, ExpectedInterfaceRole::Host, Some(false)),
                    nic(mac_b, ExpectedInterfaceRole::Host, Some(true)),
                ],
                ..Default::default()
            }
            .declared_primary_mac(),
            Some(mac_b)
        );

        assert_eq!(
            ExpectedMachineData {
                interfaces: vec![nic(mac_a, ExpectedInterfaceRole::DpuBmc, Some(true))],
                ..Default::default()
            }
            .declared_primary_mac(),
            None
        );
    }

    /// `resolved_network_segment_type` prefers the typed `network_segment_type`
    /// and otherwise maps the legacy `nic_type` string (case-insensitively),
    /// returning `None` when neither declaration names a segment type.
    #[test]
    fn resolved_network_segment_type_prefers_typed_field_then_legacy_nic_type() {
        struct Case {
            name: &'static str,
            network_segment_type: Option<NetworkSegmentType>,
            nic_type: Option<&'static str>,
            want: Option<NetworkSegmentType>,
        }

        let cases = [
            Case {
                name: "typed field selects its segment",
                network_segment_type: Some(NetworkSegmentType::Tenant),
                nic_type: None,
                want: Some(NetworkSegmentType::Tenant),
            },
            Case {
                name: "typed field wins over a legacy hint",
                network_segment_type: Some(NetworkSegmentType::Underlay),
                nic_type: Some("onboard"),
                want: Some(NetworkSegmentType::Underlay),
            },
            Case {
                name: "legacy onboard maps to admin",
                network_segment_type: None,
                nic_type: Some("onboard"),
                want: Some(NetworkSegmentType::Admin),
            },
            Case {
                name: "legacy bf3 maps to admin",
                network_segment_type: None,
                nic_type: Some("bf3"),
                want: Some(NetworkSegmentType::Admin),
            },
            Case {
                name: "legacy dpu maps to admin",
                network_segment_type: None,
                nic_type: Some("dpu"),
                want: Some(NetworkSegmentType::Admin),
            },
            Case {
                name: "legacy bmc maps to underlay",
                network_segment_type: None,
                nic_type: Some("bmc"),
                want: Some(NetworkSegmentType::Underlay),
            },
            Case {
                name: "legacy oob maps to underlay",
                network_segment_type: None,
                nic_type: Some("oob"),
                want: Some(NetworkSegmentType::Underlay),
            },
            Case {
                name: "legacy hint is case-insensitive",
                network_segment_type: None,
                nic_type: Some("BF3"),
                want: Some(NetworkSegmentType::Admin),
            },
            Case {
                name: "unknown legacy hint selects nothing",
                network_segment_type: None,
                nic_type: Some("cx8"),
                want: None,
            },
            Case {
                name: "nothing declared selects nothing",
                network_segment_type: None,
                nic_type: None,
                want: None,
            },
        ];

        for case in cases {
            let nic = ExpectedInterface {
                mac_address: "AA:BB:CC:00:00:01".parse().unwrap(),
                network_segment_type: case.network_segment_type,
                nic_type: case.nic_type.map(String::from),
                ..Default::default()
            };
            assert_eq!(
                nic.resolved_network_segment_type(),
                case.want,
                "{}",
                case.name
            );
        }
    }

    /// Explicit policies and DPU roles opt into universal segment guards,
    /// while a legacy Host declaration keeps first-DHCP selection behavior.
    #[test]
    fn segment_type_guard_preserves_legacy_host_compatibility() {
        struct Case {
            name: &'static str,
            role: ExpectedInterfaceRole,
            ip_allocation: Option<ExpectedInterfaceIpAllocation>,
            fixed_ip: Option<IpAddr>,
            network_segment_type: Option<NetworkSegmentType>,
            expected_guard: Option<NetworkSegmentType>,
            uses_legacy_host_allocation: bool,
        }

        for case in [
            Case {
                name: "legacy Host declaration",
                role: ExpectedInterfaceRole::Host,
                ip_allocation: None,
                fixed_ip: None,
                network_segment_type: Some(NetworkSegmentType::Admin),
                expected_guard: None,
                uses_legacy_host_allocation: true,
            },
            Case {
                name: "legacy Host fixed IP inference",
                role: ExpectedInterfaceRole::Host,
                ip_allocation: None,
                fixed_ip: Some("192.0.2.10".parse().unwrap()),
                network_segment_type: Some(NetworkSegmentType::Admin),
                expected_guard: None,
                uses_legacy_host_allocation: true,
            },
            Case {
                name: "explicit Host Dynamic policy",
                role: ExpectedInterfaceRole::Host,
                ip_allocation: Some(ExpectedInterfaceIpAllocation::Dynamic),
                fixed_ip: None,
                network_segment_type: Some(NetworkSegmentType::Admin),
                expected_guard: Some(NetworkSegmentType::Admin),
                uses_legacy_host_allocation: false,
            },
            Case {
                name: "explicit Host Fixed policy",
                role: ExpectedInterfaceRole::Host,
                ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
                fixed_ip: Some("192.0.2.10".parse().unwrap()),
                network_segment_type: Some(NetworkSegmentType::Admin),
                expected_guard: Some(NetworkSegmentType::Admin),
                uses_legacy_host_allocation: false,
            },
            Case {
                name: "explicit Host Retained policy",
                role: ExpectedInterfaceRole::Host,
                ip_allocation: Some(ExpectedInterfaceIpAllocation::Retained),
                fixed_ip: None,
                network_segment_type: Some(NetworkSegmentType::Admin),
                expected_guard: Some(NetworkSegmentType::Admin),
                uses_legacy_host_allocation: false,
            },
            Case {
                name: "DPU OS role",
                role: ExpectedInterfaceRole::DpuOs,
                ip_allocation: None,
                fixed_ip: None,
                network_segment_type: Some(NetworkSegmentType::Admin),
                expected_guard: Some(NetworkSegmentType::Admin),
                uses_legacy_host_allocation: false,
            },
            Case {
                name: "DPU BMC role",
                role: ExpectedInterfaceRole::DpuBmc,
                ip_allocation: None,
                fixed_ip: None,
                network_segment_type: Some(NetworkSegmentType::Underlay),
                expected_guard: Some(NetworkSegmentType::Underlay),
                uses_legacy_host_allocation: false,
            },
            Case {
                name: "Host BMC role",
                role: ExpectedInterfaceRole::HostBmc,
                ip_allocation: None,
                fixed_ip: Some("192.0.2.10".parse().unwrap()),
                network_segment_type: Some(NetworkSegmentType::Underlay),
                expected_guard: Some(NetworkSegmentType::Underlay),
                uses_legacy_host_allocation: false,
            },
            Case {
                name: "no typed segment",
                role: ExpectedInterfaceRole::DpuOs,
                ip_allocation: Some(ExpectedInterfaceIpAllocation::Dynamic),
                fixed_ip: None,
                network_segment_type: None,
                expected_guard: None,
                uses_legacy_host_allocation: false,
            },
        ] {
            let interface = ExpectedInterface {
                role: case.role,
                ip_allocation: case.ip_allocation,
                fixed_ip: case.fixed_ip,
                network_segment_type: case.network_segment_type,
                ..Default::default()
            };
            assert_eq!(
                interface.segment_type_guard(),
                case.expected_guard,
                "{}",
                case.name,
            );
            assert_eq!(
                interface.uses_legacy_host_allocation(),
                case.uses_legacy_host_allocation,
                "{}",
                case.name,
            );
        }
    }

    /// Only compatibility declarations without a Host BMC segment guard retain
    /// the external-address fallback.
    #[test]
    fn static_assignments_fallback_is_limited_to_legacy_fixed_policies() {
        /// One role/policy pair and whether it keeps the fallback.
        struct Case {
            name: &'static str,
            role: ExpectedInterfaceRole,
            ip_allocation: Option<ExpectedInterfaceIpAllocation>,
            network_segment_type: Option<NetworkSegmentType>,
            expected: bool,
        }

        for case in [
            Case {
                name: "inferred Host Fixed",
                role: ExpectedInterfaceRole::Host,
                ip_allocation: None,
                network_segment_type: None,
                expected: true,
            },
            Case {
                name: "explicit Host Fixed",
                role: ExpectedInterfaceRole::Host,
                ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
                network_segment_type: None,
                expected: false,
            },
            Case {
                name: "inferred Host BMC Fixed",
                role: ExpectedInterfaceRole::HostBmc,
                ip_allocation: None,
                network_segment_type: None,
                expected: true,
            },
            Case {
                name: "inferred Host BMC Fixed with a segment guard",
                role: ExpectedInterfaceRole::HostBmc,
                ip_allocation: None,
                network_segment_type: Some(NetworkSegmentType::Underlay),
                expected: false,
            },
            Case {
                name: "explicit Host BMC Fixed",
                role: ExpectedInterfaceRole::HostBmc,
                ip_allocation: Some(ExpectedInterfaceIpAllocation::Fixed),
                network_segment_type: None,
                expected: false,
            },
            Case {
                name: "inferred DPU OS Fixed",
                role: ExpectedInterfaceRole::DpuOs,
                ip_allocation: None,
                network_segment_type: None,
                expected: false,
            },
            Case {
                name: "inferred DPU BMC Fixed",
                role: ExpectedInterfaceRole::DpuBmc,
                ip_allocation: None,
                network_segment_type: None,
                expected: false,
            },
        ] {
            let interface = ExpectedInterface {
                role: case.role,
                ip_allocation: case.ip_allocation,
                fixed_ip: Some("192.0.2.10".parse().unwrap()),
                network_segment_type: case.network_segment_type,
                ..Default::default()
            };
            assert_eq!(
                interface.allows_static_assignments_fallback(),
                case.expected,
                "{}",
                case.name,
            );
        }
    }
}
