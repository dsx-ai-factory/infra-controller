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
use std::net::{IpAddr, Ipv4Addr};

use ::rpc::forge as rpc;
use carbide_network::ip::{IdentifyAddressFamily, IpAddressFamily};
use carbide_uuid::machine::HostMachineId;
use db::dhcp_entry::DhcpEntry;
use db::{self, expected_machine, machine_interface};
use mac_address::MacAddress;
use model::allocation_type::AllocationType;
use model::dpa_interface::DpaInterface;
use model::expected_machine::ExpectedInterface;
use model::machine::MachineInterfaceSnapshot;
use model::machine_interface::InterfaceType;
use model::network_segment::{
    AllocationStrategy, NetworkSegment, NetworkSegmentSearchConfig, NetworkSegmentType,
};
use sqlx::PgConnection;
use tonic::{Request, Response};

use crate::CarbideError;
use crate::api::Api;
use crate::dhcp::v6;

// MTU for both the underlay and overlay networks on
// the E/W Fabric
const SPX_MTU: i32 = 9000;

/// Given a desired IP address, compute the relay address by toggling the LSB.
fn get_relay_from_desired(desired: Ipv4Addr) -> Ipv4Addr {
    let ip_u32 = u32::from(desired);
    let relay_u32 = ip_u32 ^ 1;
    Ipv4Addr::from(relay_u32)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DhcpMessageKind {
    V4Discover,
    V6Solicit,
    V6Request,
    V6InfoRequest,
}

/// Validate DHCP protocol fields and return the internal routing values.
fn parse_discovery_protocol(
    address_family: Option<i32>,
    message_kind: Option<i32>,
    inferred_family: IpAddressFamily,
    duid: Option<&[u8]>,
) -> Result<(IpAddressFamily, Option<DhcpMessageKind>), CarbideError> {
    let (address_family, message_kind) = match (address_family, message_kind) {
        // Legacy callers omit both fields and are IPv4-only.
        (None, None) => {
            if inferred_family == IpAddressFamily::Ipv6 {
                return Err(CarbideError::InvalidArgument(
                    "address_family and message_kind are required for DHCPv6".to_string(),
                ));
            }
            if duid.is_some() {
                return Err(CarbideError::InvalidArgument(
                    "duid is invalid for DHCPv4 requests".to_string(),
                ));
            }
            return Ok((inferred_family, None));
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(CarbideError::InvalidArgument(
                "address_family and message_kind must be provided together".to_string(),
            ));
        }
        (Some(address_family), Some(message_kind)) => (address_family, message_kind),
    };

    let address_family = rpc::AddressFamily::try_from(address_family).map_err(|_| {
        CarbideError::InvalidArgument("unknown address_family or message_kind".to_string())
    })?;
    let message_kind = rpc::MessageKind::try_from(message_kind).map_err(|_| {
        CarbideError::InvalidArgument("unknown address_family or message_kind".to_string())
    })?;

    // Explicit unspecified values are invalid; callers should omit both fields for legacy v4.
    if address_family == rpc::AddressFamily::Unspecified
        || message_kind == rpc::MessageKind::Unspecified
    {
        return Err(CarbideError::InvalidArgument(
            "address_family and message_kind must be specified".to_string(),
        ));
    }

    let declared_family = match address_family {
        rpc::AddressFamily::V4 => IpAddressFamily::Ipv4,
        rpc::AddressFamily::V6 => IpAddressFamily::Ipv6,
        _ => {
            return Err(CarbideError::InvalidArgument(
                "unknown address_family or message_kind".to_string(),
            ));
        }
    };

    if declared_family != inferred_family {
        return Err(CarbideError::InvalidArgument(
            "address_family must match relay/link-address family".to_string(),
        ));
    }

    // Route only compatible family/kind pairs.
    match (address_family, message_kind) {
        (rpc::AddressFamily::V4, rpc::MessageKind::V4Discover) => {
            if duid.is_some() {
                return Err(CarbideError::InvalidArgument(
                    "duid is invalid for DHCPv4 requests".to_string(),
                ));
            }
            Ok((IpAddressFamily::Ipv4, Some(DhcpMessageKind::V4Discover)))
        }
        (rpc::AddressFamily::V4, _) => Err(CarbideError::InvalidArgument(
            "ADDRESS_FAMILY_V4 requires MESSAGE_KIND_V4_DISCOVER".to_string(),
        )),
        (rpc::AddressFamily::V6, rpc::MessageKind::V6Solicit) => {
            require_dhcpv6_duid(duid)?;
            Ok((IpAddressFamily::Ipv6, Some(DhcpMessageKind::V6Solicit)))
        }
        (rpc::AddressFamily::V6, rpc::MessageKind::V6Request) => {
            require_dhcpv6_duid(duid)?;
            Ok((IpAddressFamily::Ipv6, Some(DhcpMessageKind::V6Request)))
        }
        (rpc::AddressFamily::V6, rpc::MessageKind::V6InfoRequest) => {
            require_dhcpv6_duid(duid)?;
            Ok((IpAddressFamily::Ipv6, Some(DhcpMessageKind::V6InfoRequest)))
        }
        (rpc::AddressFamily::V6, _) => Err(CarbideError::InvalidArgument(
            "ADDRESS_FAMILY_V6 requires a DHCPv6 message_kind".to_string(),
        )),
        _ => Err(CarbideError::InvalidArgument(
            "unknown address_family or message_kind".to_string(),
        )),
    }
}

/// Ensure a DHCPv6 request carries a non-empty DUID.
fn require_dhcpv6_duid(duid: Option<&[u8]>) -> Result<(), CarbideError> {
    if duid.is_some_and(|duid| !duid.is_empty()) {
        Ok(())
    } else {
        Err(CarbideError::MissingArgument("duid"))
    }
}

/// Ensure the selected segment is enabled for DHCPv6.
fn ensure_dhcpv6_enabled(segment: &NetworkSegment) -> Result<(), CarbideError> {
    // A segment must carry at least one IPv6 prefix to serve DHCPv6 options.
    if segment
        .prefixes
        .iter()
        .any(|prefix| prefix.prefix.is_ipv6())
    {
        Ok(())
    } else {
        Err(CarbideError::FailedPrecondition(format!(
            "DHCPv6 request received for network segment {} without an IPv6 prefix",
            segment.id
        )))
    }
}

/// Build an options-only DHCPv6 response for a segment without observing an interface.
async fn options_only_dhcpv6_record_from_segment(
    txn: &mut PgConnection,
    mac_address: MacAddress,
    segment: &NetworkSegment,
    ntp_servers: &[Ipv4Addr],
) -> Result<rpc::DhcpRecord, CarbideError> {
    // Preserve the cache invalidation marker returned by normal DHCP records.
    let last_invalidation_time = db::dhcp_record::last_invalidation_time(&mut *txn).await?;

    Ok(rpc::DhcpRecord {
        machine_id: None,
        machine_interface_id: None,
        segment_id: Some(segment.id),
        subdomain_id: segment.config.subdomain_id,
        fqdn: String::new(),
        mac_address: mac_address.to_string(),
        address: String::new(),
        mtu: segment.config.mtu,
        prefix: String::new(),
        gateway: None,
        booturl: None,
        last_invalidation_time: Some(last_invalidation_time.into()),
        ntp_servers: ntp_servers.iter().map(ToString::to_string).collect(),
    })
}

/// Build an options-only DHCPv6 response from interface metadata.
async fn options_only_dhcpv6_record_from_interface(
    txn: &mut PgConnection,
    machine_interface: &MachineInterfaceSnapshot,
    segment: &NetworkSegment,
    ntp_servers: &[Ipv4Addr],
) -> Result<rpc::DhcpRecord, CarbideError> {
    // Resolve FQDN metadata from the interface domain when one is attached.
    let fqdn_domain_id = machine_interface.domain_id.or(segment.config.subdomain_id);
    let fqdn = if let Some(domain_id) = fqdn_domain_id {
        let domain = db::dns::domain::find_by_uuid(&mut *txn, domain_id)
            .await?
            .ok_or_else(|| CarbideError::NotFoundError {
                kind: "domain",
                id: domain_id.to_string(),
            })?;
        format!("{}.{}", machine_interface.hostname, domain.name)
    } else {
        String::new()
    };

    // Preserve the cache invalidation marker returned by normal DHCP records.
    let last_invalidation_time = db::dhcp_record::last_invalidation_time(&mut *txn).await?;

    Ok(rpc::DhcpRecord {
        machine_id: machine_interface.machine_id,
        machine_interface_id: Some(machine_interface.id),
        segment_id: Some(segment.id),
        subdomain_id: segment.config.subdomain_id,
        fqdn,
        mac_address: machine_interface.mac_address.to_string(),
        address: String::new(),
        mtu: segment.config.mtu,
        prefix: String::new(),
        gateway: None,
        booturl: None,
        last_invalidation_time: Some(last_invalidation_time.into()),
        ntp_servers: ntp_servers.iter().map(ToString::to_string).collect(),
    })
}

/// Ensure a stateful DHCP allocation exists for the requested family.
///
/// DHCPv6 stateful allocations are authoritative over prior inferred SLAAC
/// addresses:
/// both are IPv6 rows on the same interface, so the existing unique family index
/// requires replacing the inferred row before allocating the DHCP lease.
async fn ensure_dhcp_address_for_family(
    txn: &mut PgConnection,
    machine_interface: &MachineInterfaceSnapshot,
    segment: &NetworkSegment,
    parsed_mac: MacAddress,
    address_family: IpAddressFamily,
) -> Result<(), CarbideError> {
    let existing_allocation_type = db::machine_interface_address::find_allocation_type_for_family(
        &mut *txn,
        machine_interface.id,
        address_family,
    )
    .await?;

    match existing_allocation_type {
        None => {}
        Some(AllocationType::Slaac) if address_family == IpAddressFamily::Ipv6 => {
            // Take the segment lock before dropping the SLAAC row so the
            // delete-then-allocate pair holds locks in the allocator order
            // (segment advisory lock first, then address rows).
            db::machine_interface::lock_network_segments_exclusive(
                &mut *txn,
                std::slice::from_ref(&segment.id),
            )
            .await?;
            db::machine_interface_address::delete_by_interface_family(
                &mut *txn,
                machine_interface.id,
                address_family,
                AllocationType::Slaac,
            )
            .await?;
        }
        Some(_) => return Ok(()),
    }

    tracing::info!(
        machine_interface_id = %machine_interface.id,
        client_mac_address = %parsed_mac,
        ?address_family,
        "Interface missing DHCP address for family, allocating from segment"
    );

    // If the segment only allows static reservations, don't dynamically
    // allocate. The device has no reservation.
    if segment.config.allocation_strategy == AllocationStrategy::Reserved {
        return Err(CarbideError::internal(format!(
            "segment {} configured for static DHCP leases only; no static reservation for MAC {parsed_mac}",
            segment.config.name,
        )));
    }

    db::machine_interface::allocate_address_for_family(
        txn,
        machine_interface.id,
        segment,
        address_family,
    )
    .await?;

    Ok(())
}

// Overlay IP address request from DPA. DPA tells us
// what IP address it wants (calculated algorithmically
// from the underlay IP address). So we just allocate
// that desired address and update the DB.
async fn handle_overlay_from_dpa(
    txn: &mut PgConnection,
    dpa_if: &mut DpaInterface,
    macaddr: MacAddress,
    desired_addr: IpAddr,
    ntp_servers: &[Ipv4Addr],
) -> Result<Option<Response<rpc::DhcpRecord>>, CarbideError> {
    let IpAddr::V4(ip_v4_addr) = desired_addr else {
        return Err(CarbideError::internal(
            "IPv6 not supported for DPA overlay".to_string(),
        ));
    };

    let relay_addr = get_relay_from_desired(ip_v4_addr);

    let prefix = format!("{relay_addr}/31");

    dpa_if.overlay_ip = Some(desired_addr);

    db::dpa_interface::update_ip(dpa_if.clone(), false, txn).await?;

    Ok(Some(Response::new(rpc::DhcpRecord {
        machine_id: Some(dpa_if.get_machine_id().into()),
        machine_interface_id: None,
        segment_id: None,
        subdomain_id: None,
        address: desired_addr.to_string(),
        mac_address: macaddr.to_string(),
        booturl: None,
        last_invalidation_time: None,
        gateway: Some(relay_addr.to_string()),
        mtu: SPX_MTU,
        fqdn: String::new(),
        prefix,
        ntp_servers: ntp_servers.iter().map(ToString::to_string).collect(),
    })))
}

// DPA is asking for an underlay IP address. The underlay IP
// address is just the relay address with the LSB toggled.
async fn handle_underlay_from_dpa(
    txn: &mut PgConnection,
    dpa_if: &mut DpaInterface,
    macaddr: MacAddress,
    relay_address: String,
    ntp_servers: &[Ipv4Addr],
) -> Result<Option<Response<rpc::DhcpRecord>>, CarbideError> {
    // The relay address and the mac address should differ only in bit 0
    let relay_addr = relay_address.parse::<Ipv4Addr>()?;

    let ip_u32 = u32::from(relay_addr);

    let retaddr = ip_u32 ^ 1;

    let ret_addr = Ipv4Addr::from(retaddr);

    let prefix = format!("{relay_addr}/31");

    dpa_if.underlay_ip = Some(IpAddr::from(ret_addr));

    db::dpa_interface::update_ip(dpa_if.clone(), true, txn).await?;

    Ok(Some(Response::new(rpc::DhcpRecord {
        machine_id: Some(dpa_if.get_machine_id().into()),
        machine_interface_id: None,
        segment_id: None,
        subdomain_id: None,
        address: ret_addr.to_string(),
        mac_address: macaddr.to_string(),
        booturl: None,
        last_invalidation_time: None,
        gateway: Some(relay_address),
        mtu: SPX_MTU,
        fqdn: String::new(),
        prefix,
        ntp_servers: ntp_servers.iter().map(ToString::to_string).collect(),
    })))
}

// See if this is a underlay/overlay IP allocation request
// from a DPA. If the specified macaddr belongs to any DPA
// object, we know it's a request from a DPA. And the presence
// of desired ip (option 50) means it's overlay request, and
// the absence of option 50 means it's an underlay request.
async fn handle_dhcp_from_dpa(
    api: &Api,
    txn: &mut PgConnection,
    macaddr: MacAddress,
    relay_address: String,
    desired_address: Option<IpAddr>,
) -> Result<Option<Response<rpc::DhcpRecord>>, CarbideError> {
    if !api.runtime_config.is_ewethers_enabled() {
        return Ok(None);
    }

    let mut dpa_ifs = db::dpa_interface::find_by_mac_addr(&mut *txn, &macaddr).await?;

    if dpa_ifs.len() != 1 {
        // If the MAC address does not belong to any DPA object, len will be 0.
        // Log cases where len is neither 0 nor 1.
        if !dpa_ifs.is_empty() {
            tracing::error!(
                mac_address = %macaddr,
                dpa_interface_count = dpa_ifs.len(),
                "Unexpected number of DPA interfaces found",
            );
        }
        return Ok(None);
    }

    let mut dpa_if = dpa_ifs.remove(0);

    if let Some(addr) = desired_address {
        return handle_overlay_from_dpa(
            txn,
            &mut dpa_if,
            macaddr,
            addr,
            &api.runtime_config.ntp_servers,
        )
        .await;
    }

    handle_underlay_from_dpa(
        txn,
        &mut dpa_if,
        macaddr,
        relay_address,
        &api.runtime_config.ntp_servers,
    )
    .await
}

pub(crate) async fn discover_dhcp(
    api: &Api,
    request: Request<rpc::DhcpDiscovery>,
) -> Result<Response<rpc::DhcpRecord>, CarbideError> {
    // Admission permit BEFORE the first transaction: the interface/address
    // lookups below can wait on segment advisory locks, so a waiter here
    // pins a pool connection exactly like the allocation path does.
    let _admin_admission = db::machine_interface::admin_lock_admission().await;
    let mut txn = api.txn_begin().await?;

    let rpc::DhcpDiscovery {
        mac_address,
        relay_address,
        link_address,
        vendor_string,
        desired_address,
        address_family,
        message_kind,
        duid,
        ..
    } = request.into_inner();

    // Select the segment lookup key once. DHCPv6 uses Relay-Forward link-address
    // when present, so all segment lookups and predicted promotion use this value.
    let address_to_use_for_dhcp = link_address.as_ref().unwrap_or(&relay_address);
    let parsed_relay: IpAddr = address_to_use_for_dhcp.parse()?;
    let (address_family, message_kind) = parse_discovery_protocol(
        address_family,
        message_kind,
        parsed_relay.address_family(),
        duid.as_deref(),
    )?;
    let is_v6_observation = address_family == IpAddressFamily::Ipv6
        && message_kind == Some(DhcpMessageKind::V6InfoRequest);
    let mut expected_interface: Option<ExpectedInterface> = None;
    // `host_primary_declaration` is intentionally Host-only. A DPU OS
    // interface is always primary, while DPU and Host BMC interfaces are never
    // primary; api-db derives those values from the matched interface role.
    let mut host_primary_declaration: Option<bool> = None;

    let parsed_mac: MacAddress = mac_address.parse()?;

    // If DHCP is suppressed for this BMC MAC, acknowledge and refuse.
    // The decommission workflow polls acknowledged_at to confirm the BMC
    // DHCP client has returned to the INIT state.
    if db::bmc_suppression::acknowledge(
        &mut txn,
        parsed_mac,
        model::bmc_suppression::BmcSuppressionSubsystem::Dhcp,
    )
    .await?
    {
        txn.commit().await?;
        return Err(CarbideError::FailedPrecondition(format!(
            "dhcp suppressed for bmc mac {parsed_mac}"
        )));
    }

    let mut predicted_interface_for_observation = None;

    let desired_address_ip: Option<IpAddr> = if is_v6_observation {
        None
    } else {
        desired_address.map(|addr| addr.parse()).transpose()?
    };

    let existing_machine_id =
        match db::machine::find_existing_machine(&mut txn, parsed_mac, parsed_relay).await? {
            Some(existing_machine) => Some(existing_machine),
            None => {
                if let Some(predicted_interface) =
                    db::predicted_machine_interface::find_by_mac_address(&mut txn, parsed_mac)
                        .await?
                {
                    if is_v6_observation {
                        predicted_interface_for_observation = Some(predicted_interface);
                        None
                    } else {
                        // remember expected machine id for later rack update
                        machine_interface::move_predicted_machine_interface_to_machine(
                            &mut txn,
                            &predicted_interface,
                            parsed_relay,
                            api.runtime_config.retained_boot_interface_window,
                        )
                        .await?;
                        Some(predicted_interface.machine_id)
                    }
                } else {
                    // DPA allocation is currently IPv4-only. The overlay
                    // uses u32 arithmetic (LSB toggle) and /31 linknets,
                    // and the underlay parses relay_address as Ipv4Addr.
                    // Skip the DPA path entirely for IPv6 relays.
                    if address_family == IpAddressFamily::Ipv4
                        && let Some(resp) = handle_dhcp_from_dpa(
                            api,
                            &mut txn,
                            parsed_mac,
                            relay_address,
                            desired_address_ip,
                        )
                        .await?
                    {
                        txn.commit().await?;
                        return Ok(resp);
                    }

                    // ExpectedMachine is the ingestion template for an unknown
                    // interface, so this is where its role, segment guard, Host
                    // primary declaration, and allocation policy join the normal
                    // DHCP path.
                    //
                    // Why also materialize a fixed reservation here? An operator
                    // may force-delete a machine and all of its interfaces. The
                    // next DHCP request is then the first point where we see that
                    // MAC again, so the idempotent preallocation rebuilds the
                    // reservation before the common find-or-create path runs.
                    // The top-level BMC MAC is the ExpectedMachine alternate
                    // key, so resolve it before searching the nested JSON
                    // list. Otherwise another declaration using the same MAC
                    // could make DHCP treat a host BMC as a data interface.
                    if let Some(m) = expected_machine::find_by_bmc_mac_address(&mut txn, parsed_mac)
                        .await
                        .map_err(CarbideError::from)?
                    {
                        let interface = m.effective_host_bmc();
                        if interface
                            .fixed_ip
                            .is_some_and(|fixed_ip| fixed_ip.is_address_family(address_family))
                        {
                            db::machine_interface::preallocate_expected_machine_interface(
                                &mut txn,
                                &interface,
                                api.runtime_config.retained_boot_interface_window,
                            )
                            .await?;
                        }
                        expected_interface = Some(interface);
                    } else if let Some(m) =
                        expected_machine::find_by_interface_mac_address(&mut txn, parsed_mac)
                            .await
                            .map_err(CarbideError::from)?
                    {
                        expected_interface = m
                            .data
                            .interfaces
                            .iter()
                            .find(|interface| interface.mac_address == parsed_mac)
                            .cloned();
                        if let Some(interface) = expected_interface.as_ref() {
                            if interface.role.is_host()
                                && let Some(declared_primary_mac) = m.data.declared_primary_mac()
                            {
                                host_primary_declaration = Some(declared_primary_mac == parsed_mac);
                            }
                            if interface
                                .fixed_ip
                                .is_some_and(|fixed_ip| fixed_ip.is_address_family(address_family))
                            {
                                // The fixed reservation must exist before the common
                                // path looks up this MAC. This call also applies the
                                // interface's role and segment guard while the row is
                                // still unassociated.
                                db::machine_interface::preallocate_expected_machine_interface(
                                    &mut txn,
                                    interface,
                                    api.runtime_config.retained_boot_interface_window,
                                )
                                .await?;
                            }
                        }
                    } else if let Some(s) =
                        db::expected_switch::find_by_nvos_mac_address(&mut txn, parsed_mac)
                            .await
                            .map_err(CarbideError::from)?
                        && let Some(nvos_ip) = s.nvos_ip_address
                        && nvos_ip.is_address_family(address_family)
                    {
                        // The parsed MAC matches the single wired NVOS port of an expected
                        // switch with a configured static IP. Mirrors the ExpectedInterface
                        // fixed_ip path: ensure the (mac, nvos_ip) row exists so the static
                        // reservation gets served by the find_or_create_machine_interface
                        // step below. Data variant (NVOS is a data interface, not a BMC).
                        // Races against site-explorer's reconciliation pass are handled
                        // inside preallocate.
                        db::machine_interface::preallocate_machine_interface(
                            &mut txn,
                            parsed_mac,
                            nvos_ip,
                            api.runtime_config.retained_boot_interface_window,
                        )
                        .await?;
                    }
                    None
                }
            }
        };

    if is_v6_observation {
        let network_segments = db::machine_interface::network_segments_for_dhcp_relays(
            &mut txn,
            std::slice::from_ref(&parsed_relay),
            expected_interface.as_ref(),
        )
        .await?;
        let exact_link_address_match = |segment: &NetworkSegment| {
            segment
                .prefixes
                .iter()
                .any(|prefix| prefix.dhcpv6_link_address == Some(parsed_relay))
        };
        let reserved_segment = |segment: &NetworkSegment| {
            segment.config.allocation_strategy == AllocationStrategy::Reserved
        };

        // Exact DHCPv6 link-address matches are authoritative. Only prefer
        // reserved segments within that exact-match subset; prefix candidates
        // are fallback routing context.
        let segment = network_segments
            .iter()
            .filter(|&segment| exact_link_address_match(segment))
            .find(|&segment| reserved_segment(segment))
            .or_else(|| {
                network_segments
                    .iter()
                    .find(|&segment| exact_link_address_match(segment))
            })
            .or_else(|| {
                // Prefix-overlap routing is intentionally not resolved here. If
                // no exact DHCPv6 link-address match exists, prefer a reserved
                // candidate so anonymous INFO_REQUESTs can receive options-only
                // metadata instead of creating an observed row on an ambiguous
                // dynamic prefix.
                network_segments
                    .iter()
                    .find(|&segment| reserved_segment(segment))
            })
            .or_else(|| network_segments.first())
            .ok_or_else(|| {
                CarbideError::internal(format!(
                    "no network segment defined for DHCPv6 relay address {parsed_relay}"
                ))
            })?;
        ensure_dhcpv6_enabled(segment)?;

        let interfaces = db::machine_interface::find_by_mac_address(&mut txn, parsed_mac).await?;
        let has_cross_segment_interface = interfaces
            .iter()
            .any(|interface| interface.segment_id != segment.id);
        if has_cross_segment_interface {
            // Do not turn a wrong-segment known MAC into config-only success.
            // Fall through so the existing global MAC guard rejects or handles
            // static-assignment moves exactly as IPv4/stateful DHCP does.
            tracing::debug!(
                client_mac_address = %parsed_mac,
                network_segment_id = %segment.id,
                "DHCPv6 options request will use global MAC segment reconciliation"
            );
        } else if segment.config.allocation_strategy == AllocationStrategy::Reserved
            && interfaces.is_empty()
            && predicted_interface_for_observation.is_none()
        {
            // Reserved requests without an existing or predicted interface can
            // receive segment options without creating an observed row. Known
            // or predicted interfaces must continue through common safety
            // checks and DHCP bookkeeping.
            let record = options_only_dhcpv6_record_from_segment(
                &mut txn,
                parsed_mac,
                segment,
                &api.runtime_config.ntp_servers,
            )
            .await?;
            txn.commit().await?;
            return Ok(Response::new(record));
        }
    }

    if is_v6_observation && predicted_interface_for_observation.is_some() {
        let interfaces = db::machine_interface::find_by_mac_address(&mut txn, parsed_mac).await?;
        if interfaces.is_empty()
            && let Some(predicted_interface) = predicted_interface_for_observation.take()
        {
            // Reserved segments reject anonymous observed-row creation, but a
            // prediction is explicit host identity. Promote it before the observed
            // helper so the common safety checks and DHCP bookkeeping still run.
            machine_interface::move_predicted_machine_interface_to_machine(
                &mut txn,
                &predicted_interface,
                parsed_relay,
                api.runtime_config.retained_boot_interface_window,
            )
            .await?;
        }
    }

    let mut machine_interface = if is_v6_observation {
        // INFORMATION-REQUEST observes identity only; it must not consume a DHCP lease.
        db::machine_interface::find_or_create_observed_machine_interface(
            &mut txn,
            existing_machine_id,
            parsed_mac,
            std::slice::from_ref(&parsed_relay),
            expected_interface,
            host_primary_declaration,
            api.runtime_config.retained_boot_interface_window,
        )
        .await?
    } else {
        // First-contact stateful DHCP needs candidate-segment fallback, but only
        // for the requested address family.
        db::machine_interface::find_or_create_machine_interface_for_family(
            &mut txn,
            existing_machine_id,
            parsed_mac,
            std::slice::from_ref(&parsed_relay),
            machine_interface::FindOrCreateMachineInterfaceOptions {
                expected_interface,
                is_primary: host_primary_declaration,
                retained_window: api.runtime_config.retained_boot_interface_window,
            },
            address_family,
        )
        .await?
    };

    if let Some(predicted_interface) = predicted_interface_for_observation {
        machine_interface::move_predicted_machine_interface_to_machine(
            &mut txn,
            &predicted_interface,
            parsed_relay,
            api.runtime_config.retained_boot_interface_window,
        )
        .await?;
        machine_interface = db::machine_interface::find_one(&mut txn, machine_interface.id).await?;
    }

    // Use the interface's actual segment, not only relay context, so
    // dormant admin interfaces cannot keep serving stale DHCP leases.
    let segment = db::network_segment::find_by(
        &mut txn,
        db::ObjectColumnFilter::One(db::network_segment::IdColumn, &machine_interface.segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?
    .pop()
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "network_segment",
        id: machine_interface.segment_id.to_string(),
    })?;

    if address_family == IpAddressFamily::Ipv6 {
        ensure_dhcpv6_enabled(&segment)?;
    }

    // Only DPU-backed host admin links are dormant when non-primary. Other non-primary admin
    // interfaces can be valid operator-declared host NICs and must still be allowed to DHCP.
    let is_dpu_backed_host_admin_interface = machine_interface.attached_dpu_machine_id.is_some()
        && machine_interface.attached_dpu_machine_id.map(Into::into)
            != machine_interface.machine_id;
    if is_dpu_backed_host_admin_interface
        && !machine_interface.primary_interface
        && segment.config.segment_type == NetworkSegmentType::Admin
    {
        return Err(CarbideError::FailedPrecondition(format!(
            "DHCP request received on dormant non-primary admin interface {}. ignoring",
            machine_interface.id
        )));
    }

    if is_v6_observation {
        v6::infer_slaac_eui64_address(&mut txn, machine_interface.id, &segment, &parsed_mac)
            .await?;
    } else {
        ensure_dhcp_address_for_family(
            &mut txn,
            &machine_interface,
            &segment,
            parsed_mac,
            address_family,
        )
        .await?;
    }

    if machine_interface.interface_type != InterfaceType::Bmc
        && let Some(machine_id) = machine_interface.machine_id
        && machine_id.machine_type().is_host()
        && let Some(instance_id) =
            db::instance::find_id_by_machine_id(&mut txn, &machine_id).await?
    {
        // An instance is associated with this host. If the host has DPUs,
        // the DPUs proxy DHCP on its behalf, so we reject the host's direct
        // DHCP request. Zero-DPU hosts have no such intermediary, so let
        // their DHCP proceed.
        let host_machine_id = HostMachineId::try_from(machine_id)?;
        let dpus = db::machine::find_dpus_by_host_machine_id(&mut txn, &host_machine_id).await?;
        if !dpus.is_empty() {
            return Err(CarbideError::internal(format!(
                "DHCP request received for instance: {instance_id}. ignoring"
            )));
        }
    }

    // Save vendor string, this is allowed to fail due to dhcp happening more than once on the same machine/vendor string
    if let Some(vendor) = vendor_string {
        let res = db::dhcp_entry::persist(
            DhcpEntry {
                machine_interface_id: machine_interface.id,
                vendor_string: vendor,
            },
            &mut txn,
        )
        .await;
        match res {
            Ok(()) => {} // do nothing on ok result
            Err(error) => {
                tracing::error!(%error, "Could not persist dhcp entry")
            } // This should not fail the discover call, dhcp happens many times
        }
    }

    db::machine_interface::update_last_dhcp(&mut txn, machine_interface.id, None).await?;

    let options_only_record = if is_v6_observation {
        machine_interface = db::machine_interface::find_one(&mut txn, machine_interface.id).await?;
        Some(
            options_only_dhcpv6_record_from_interface(
                &mut txn,
                &machine_interface,
                &segment,
                &api.runtime_config.ntp_servers,
            )
            .await?,
        )
    } else {
        None
    };

    txn.commit().await?;

    if let Some(record) = options_only_record {
        return Ok(Response::new(record));
    }

    // Permit from the top of discover_dhcp is still held here; the earlier
    // transaction committed, but the allocation path below takes the same
    // segment locks, so one permit covers the whole request.
    let mut txn = api.txn_begin().await?;

    let record = db::dhcp_record::find_by_mac_address(
        &mut txn,
        &parsed_mac,
        &machine_interface.segment_id,
        address_family,
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "DHCP record",
        id: format!(
            "{parsed_mac} (segment {}, {:?})",
            machine_interface.segment_id, address_family
        ),
    })?;
    let mut record: rpc::DhcpRecord = record.into();

    txn.commit().await?;

    record.ntp_servers = api
        .runtime_config
        .ntp_servers
        .iter()
        .map(ToString::to_string)
        .collect();

    Ok(Response::new(record))
}
