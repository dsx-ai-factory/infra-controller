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

//! Checks and restores the GB200 BMC interface used to communicate with the host.

use http::header::{ETAG, IF_MATCH};
use http::{HeaderMap, Method, StatusCode};
use libredfish::{Redfish, RedfishError};
use serde::{Deserialize, Serialize};

use super::instrumented::REDFISH_BACKEND;

const GB200_HOST_INTERFACE_ID: &str = "hostusb0";
const GB200_HOST_INTERFACE_ADDRESS: &str = "10.0.1.1";
const GB200_HOST_INTERFACE_SUBNET_MASK: &str = "255.255.255.0";
const GB200_HOST_INTERFACE_GATEWAY: &str = "0.0.0.0";

const GET_FOR_REPAIR_OPERATION: &str = "get_manager_host_interface_for_static_ipv4_repair";
const PATCH_OPERATION: &str = "patch_manager_host_interface_static_ipv4";

/// Result of an attempt to repair the GB200 BMC host interface configuration.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum HostInterfaceStaticIpv4Repair {
    /// The expected address was already present, so no request changed the resource.
    AlreadyConfigured,
    /// The missing address was patched and confirmed by reading the resource again.
    Patched,
    /// A nonempty configuration differs from the expected address and was not changed.
    Conflicting,
}

/// Wire representation used to replace the static IPv4 address list.
#[derive(Debug, Serialize)]
struct EthernetInterfacePatch {
    #[serde(rename = "IPv4StaticAddresses")]
    ipv4_static_addresses: Vec<StaticIpv4Address>,
}

/// Static address values required by the GB200 BMC host interface.
#[derive(Debug, Serialize)]
#[serde(rename_all = "PascalCase")]
struct StaticIpv4Address {
    address: &'static str,
    subnet_mask: &'static str,
    gateway: &'static str,
}

/// Minimal manager Ethernet interface response needed for a guarded repair.
#[derive(Debug, Deserialize)]
struct EthernetInterfaceResponse {
    #[serde(rename = "IPv4StaticAddresses")]
    ipv4_static_addresses: Option<Vec<StaticIpv4AddressResponse>>,
}

/// Static IPv4 values returned by the BMC.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StaticIpv4AddressResponse {
    address: Option<String>,
    subnet_mask: Option<String>,
    gateway: Option<String>,
}

/// A manager Ethernet interface observation that retains the response headers
/// needed to guard a subsequent write with the exact resource `ETag`.
struct HostInterfaceObservation {
    static_ipv4_status: StaticIpv4Status,
    headers: Option<HeaderMap>,
}

/// Whether the BMC reports the expected, missing, or conflicting address.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum StaticIpv4Status {
    Present,
    Missing,
    Conflicting,
}

/// Reports whether the GB200 BMC interface has no static IPv4 addresses.
///
/// The pinned libredfish model preserves the number of configured addresses,
/// but does not deserialize their `Address` values. `WaitingForDiscovery` only
/// needs the empty list check; the repair path reads the raw resource so it can
/// verify every value before writing anything.
pub async fn gb200_host_interface_static_ipv4_is_missing(
    redfish_client: &dyn Redfish,
) -> Result<bool, RedfishError> {
    let interface = redfish_client
        .get_manager_ethernet_interface(GB200_HOST_INTERFACE_ID)
        .await?;

    Ok(interface.ipv4_static_addresses.is_empty())
}

/// Restores a missing static IPv4 address on the GB200 BMC host interface.
///
/// An existing conflicting configuration is reported and is never replaced.
/// The repair requires the resource's `ETag` and sends that value unchanged in
/// `If-Match`, so a concurrent update fails instead of being overwritten.
/// Wiwynn firmware returns HTTP 200 for the successful PATCH; HTTP 204 is
/// accepted for Redfish implementations that omit a response body.
pub async fn ensure_gb200_host_interface_static_ipv4(
    redfish_client: &dyn Redfish,
) -> Result<HostInterfaceStaticIpv4Repair, RedfishError> {
    // The typed libredfish operation does not expose response headers. Read the
    // resource directly so the PATCH is conditional on the same representation
    // that was classified immediately before the change.
    let observation = get_gb200_host_interface_for_repair(redfish_client).await?;
    match observation.static_ipv4_status {
        StaticIpv4Status::Present => {
            return Ok(HostInterfaceStaticIpv4Repair::AlreadyConfigured);
        }
        StaticIpv4Status::Conflicting => {
            return Ok(HostInterfaceStaticIpv4Repair::Conflicting);
        }
        StaticIpv4Status::Missing => {}
    }

    let url = gb200_host_interface_url(redfish_client);
    let etag = required_etag(observation.headers.as_ref(), &url)?;

    patch_gb200_host_interface(redfish_client, etag).await?;

    match get_gb200_host_interface_for_repair(redfish_client)
        .await?
        .static_ipv4_status
    {
        StaticIpv4Status::Present => Ok(HostInterfaceStaticIpv4Repair::Patched),
        StaticIpv4Status::Missing => Err(RedfishError::GenericError {
            error: format!(
                "manager interface {GB200_HOST_INTERFACE_ID} accepted the static IPv4 PATCH but the address is still missing"
            ),
        }),
        StaticIpv4Status::Conflicting => Err(RedfishError::GenericError {
            error: format!(
                "manager interface {GB200_HOST_INTERFACE_ID} accepted the static IPv4 PATCH but returned a conflicting configuration"
            ),
        }),
    }
}

/// Reads the raw resource because the typed API does not expose its `ETag`.
async fn get_gb200_host_interface_for_repair(
    redfish_client: &dyn Redfish,
) -> Result<HostInterfaceObservation, RedfishError> {
    let url = gb200_host_interface_url(redfish_client);
    carbide_instrument::red::instrumented(REDFISH_BACKEND, GET_FOR_REPAIR_OPERATION, async {
        let (_, interface, headers) = redfish_client
            .std_redfish()
            .client
            .req::<EthernetInterfaceResponse, String>(
                Method::GET,
                &url,
                None,
                None,
                None,
                Vec::new(),
            )
            .await?;

        let interface = interface.ok_or(RedfishError::NoContent)?;
        let addresses = interface.ipv4_static_addresses.ok_or_else(|| {
            RedfishError::GenericError {
                error: format!(
                    "manager interface {url} did not report IPv4StaticAddresses; refusing to change it"
                ),
            }
        })?;

        Ok(HostInterfaceObservation {
            static_ipv4_status: host_interface_static_ipv4_status(&addresses),
            headers,
        })
    })
    .await
}

/// Writes the expected address only when the resource still has the observed `ETag`.
async fn patch_gb200_host_interface(
    redfish_client: &dyn Redfish,
    etag: String,
) -> Result<(), RedfishError> {
    let url = gb200_host_interface_url(redfish_client);
    let body = EthernetInterfacePatch {
        ipv4_static_addresses: vec![StaticIpv4Address {
            address: GB200_HOST_INTERFACE_ADDRESS,
            subnet_mask: GB200_HOST_INTERFACE_SUBNET_MASK,
            gateway: GB200_HOST_INTERFACE_GATEWAY,
        }],
    };

    carbide_instrument::red::instrumented(REDFISH_BACKEND, PATCH_OPERATION, async {
        let (status_code, _, _) = redfish_client
            .std_redfish()
            .client
            .req::<serde_json::Value, _>(
                Method::PATCH,
                &url,
                Some(body),
                None,
                None,
                vec![(IF_MATCH, etag)],
            )
            .await?;

        // Verification assumes the address was applied synchronously, so an
        // asynchronous success response is not accepted here.
        match status_code {
            StatusCode::OK | StatusCode::NO_CONTENT => Ok(()),
            _ => Err(RedfishError::HTTPErrorCode {
                url,
                status_code,
                response_body: "expected HTTP 200 or 204 while patching the manager host interface"
                    .to_string(),
            }),
        }
    })
    .await
}

/// Extracts an exact resource `ETag`, rejecting missing or wildcard values.
fn required_etag(headers: Option<&HeaderMap>, url: &str) -> Result<String, RedfishError> {
    let etag = headers
        .and_then(|headers| headers.get(ETAG))
        .and_then(|etag| etag.to_str().ok())
        .filter(|etag| !etag.is_empty() && *etag != "*")
        .ok_or_else(|| RedfishError::GenericError {
            error: format!("manager interface {url} response is missing a usable ETag"),
        })?;

    Ok(etag.to_string())
}

/// Builds the manager Ethernet interface path for the current BMC manager.
fn gb200_host_interface_url(redfish_client: &dyn Redfish) -> String {
    let manager_id = redfish_client.std_redfish().manager_id();
    format!("Managers/{manager_id}/EthernetInterfaces/{GB200_HOST_INTERFACE_ID}")
}

/// Classifies the complete static IPv4 address list without discarding conflicts.
fn host_interface_static_ipv4_status(addresses: &[StaticIpv4AddressResponse]) -> StaticIpv4Status {
    match addresses {
        [] => StaticIpv4Status::Missing,
        [address] if is_expected_gb200_host_interface_address(address) => StaticIpv4Status::Present,
        _ => StaticIpv4Status::Conflicting,
    }
}

/// Checks every value that must match before an existing address is accepted.
fn is_expected_gb200_host_interface_address(address: &StaticIpv4AddressResponse) -> bool {
    address.address.as_deref() == Some(GB200_HOST_INTERFACE_ADDRESS)
        && address.subnet_mask.as_deref() == Some(GB200_HOST_INTERFACE_SUBNET_MASK)
        && matches!(
            address.gateway.as_deref(),
            None | Some(GB200_HOST_INTERFACE_GATEWAY)
        )
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    fn static_address(
        address: &str,
        subnet_mask: &str,
        gateway: Option<&str>,
    ) -> StaticIpv4AddressResponse {
        StaticIpv4AddressResponse {
            address: Some(address.to_string()),
            gateway: gateway.map(String::from),
            subnet_mask: Some(subnet_mask.to_string()),
        }
    }

    #[test]
    fn static_ipv4_status_classifies_complete_address_list() {
        value_scenarios!(run = |addresses: Vec<StaticIpv4AddressResponse>| {
            host_interface_static_ipv4_status(&addresses)
        };
            "missing" {
                Vec::new() => StaticIpv4Status::Missing,
            }

            "present" {
                vec![static_address(
                    GB200_HOST_INTERFACE_ADDRESS,
                    GB200_HOST_INTERFACE_SUBNET_MASK,
                    None,
                )] => StaticIpv4Status::Present,
                vec![static_address(
                    GB200_HOST_INTERFACE_ADDRESS,
                    GB200_HOST_INTERFACE_SUBNET_MASK,
                    Some(GB200_HOST_INTERFACE_GATEWAY),
                )] => StaticIpv4Status::Present,
            }

            "conflicting" {
                vec![StaticIpv4AddressResponse {
                    address: None,
                    subnet_mask: None,
                    gateway: None,
                }] => StaticIpv4Status::Conflicting,
                vec![static_address("192.0.2.1", GB200_HOST_INTERFACE_SUBNET_MASK, None)]
                    => StaticIpv4Status::Conflicting,
                vec![static_address(GB200_HOST_INTERFACE_ADDRESS, "255.255.0.0", None)]
                    => StaticIpv4Status::Conflicting,
                vec![static_address(
                    GB200_HOST_INTERFACE_ADDRESS,
                    GB200_HOST_INTERFACE_SUBNET_MASK,
                    Some("10.0.1.254"),
                )] => StaticIpv4Status::Conflicting,
                vec![
                    static_address(
                        GB200_HOST_INTERFACE_ADDRESS,
                        GB200_HOST_INTERFACE_SUBNET_MASK,
                        None,
                    ),
                    static_address("192.0.2.1", GB200_HOST_INTERFACE_SUBNET_MASK, None),
                ] => StaticIpv4Status::Conflicting,
            }
        );
    }
}
