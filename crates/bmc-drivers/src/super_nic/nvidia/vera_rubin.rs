/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, SuperNic};
use nv_redfish::chassis::{NetworkAdapter, NetworkAdapterUpdate};
use nv_redfish::core::Bmc;
use nv_redfish::oem::nvidia::network_adapter::{
    NvidiaNetworkAdapter, NvidiaNetworkAdapterUpdate, NvidiaNetworkAdapterUpdateExt,
};

use crate::resources::RedfishResourcesExt as _;

/// NVIDIA Vera Rubin: each CX9 SuperNIC is chassis `CX_<index>` with one
/// network adapter, `NIC_0`, whose `Oem.Nvidia` carries the east-west control.
pub(crate) struct VeraRubinSuperNic;

const ADAPTER_ID: &str = "NIC_0";

/// SuperNIC `index`'s adapter and its NVIDIA extension.
async fn adapter<B: Bmc>(
    cx: &OpCx<'_, B>,
    index: u8,
) -> Result<(NetworkAdapter<B>, NvidiaNetworkAdapter<B>), PlatformError> {
    let chassis_id = format!("CX_{index}");
    let missing = |what: &str| PlatformError::InvalidResponse {
        message: format!("SuperNIC {index}: {what}"),
    };
    let chassis = cx
        .chassis_by_id(&chassis_id)
        .await?
        .ok_or_else(|| missing(&format!("no chassis {chassis_id}")))?;
    let adapter = chassis
        .network_adapters()
        .await
        .map_err(|error| cx.map_redfish_error(error))?
        .unwrap_or_default()
        .into_iter()
        .find(|adapter| adapter.raw().id == ADAPTER_ID)
        .ok_or_else(|| missing(&format!("chassis {chassis_id} has no adapter {ADAPTER_ID}")))?;
    let nvidia = adapter
        .oem_nvidia()
        .map_err(|error| cx.map_redfish_error(error))?
        .ok_or_else(|| missing(&format!("adapter {ADAPTER_ID} has no Oem.Nvidia")))?;
    Ok((adapter, nvidia))
}

#[async_trait]
impl<B: Bmc> SuperNic<B> for VeraRubinSuperNic {
    async fn east_west_control(
        &self,
        cx: &OpCx<'_, B>,
        index: u8,
    ) -> Result<Option<bool>, PlatformError> {
        let (_, nvidia) = adapter(cx, index).await?;
        Ok(nvidia.raw().east_west_control_enabled.flatten())
    }

    async fn set_east_west_control(
        &self,
        cx: &OpCx<'_, B>,
        index: u8,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError> {
        let (adapter, _) = adapter(cx, index).await?;
        let update = NetworkAdapterUpdate::builder()
            .build()
            .with_oem_nvidia(
                NvidiaNetworkAdapterUpdate::builder()
                    .with_east_west_control_enabled(enabled)
                    .build(),
            )
            .map_err(|error| PlatformError::InvalidResponse {
                message: format!("failed to build the east-west control update: {error}"),
            })?;
        adapter
            .settings()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .unwrap_or(adapter)
            .update(&update)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| cx.map_redfish_error(error))
    }

    async fn mac_address(
        &self,
        cx: &OpCx<'_, B>,
        index: u8,
    ) -> Result<Option<String>, PlatformError> {
        let interface_id = host_interface_id(index);
        let missing = |what: String| PlatformError::InvalidResponse {
            message: format!("SuperNIC {index}: {what}"),
        };
        let interfaces = cx
            .system()
            .await?
            .ethernet_interfaces()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or_else(|| missing("the system lists no Ethernet interfaces".to_string()))?
            .members()
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        let interface = interfaces
            .iter()
            .find(|interface| interface.raw().id == interface_id)
            .ok_or_else(|| missing(format!("no Ethernet interface {interface_id}")))?;
        Ok(interface.mac_address().map(|mac| mac.as_str().to_string()))
    }
}

/// SuperNIC `index`'s host Ethernet interface on the system: NICs pair onto
/// cages 1, 3, 5 and 7, one per MPO, as `NIC_0_C<cage>_<mpo>`.
fn host_interface_id(index: u8) -> String {
    format!("NIC_0_C{}_{}", (index / 2) * 2 + 1, index % 2 + 1)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body, path};

    const CHASSIS: &str = "/redfish/v1/Chassis";
    const CX_0: &str = "/redfish/v1/Chassis/CX_0";
    const ADAPTERS: &str = "/redfish/v1/Chassis/CX_0/NetworkAdapters";
    const NIC_0: &str = "/redfish/v1/Chassis/CX_0/NetworkAdapters/NIC_0";
    const NIC_0_SETTINGS: &str = "/redfish/v1/Chassis/CX_0/NetworkAdapters/NIC_0/Settings";

    fn nvidia(east_west: bool) -> serde_json::Value {
        json!({
            "@odata.type": "#NvidiaNetworkAdapter.v1_2_0.NvidiaNetworkAdapter",
            "EastWestControlEnabled": east_west,
        })
    }

    async fn vera_rubin() -> crate::test_support::FixtureBmc {
        Fixture::new("NVIDIA", "VR NVL72", "System_0", "BMC_0")
            .document(
                CHASSIS,
                json!({
                    "@odata.id": CHASSIS,
                    "@odata.type": "#ChassisCollection.ChassisCollection",
                    "Name": "Chassis",
                    "Members": [{"@odata.id": CX_0}],
                }),
            )
            .document(
                CX_0,
                json!({
                    "@odata.id": CX_0, "Id": "CX_0", "Name": "CX_0", "ChassisType": "Card",
                    "NetworkAdapters": {"@odata.id": ADAPTERS},
                }),
            )
            .document(
                ADAPTERS,
                json!({
                    "@odata.id": ADAPTERS,
                    "@odata.type": "#NetworkAdapterCollection.NetworkAdapterCollection",
                    "Name": "Network Adapters",
                    "Members": [{"@odata.id": NIC_0}],
                }),
            )
            .document(
                NIC_0,
                json!({
                    "@odata.id": NIC_0, "Id": "NIC_0", "Name": "NIC_0",
                    "@Redfish.Settings": {"SettingsObject": {"@odata.id": NIC_0_SETTINGS}},
                    "Oem": {"Nvidia": nvidia(false)},
                }),
            )
            .document(
                NIC_0_SETTINGS,
                json!({
                    "@odata.id": NIC_0_SETTINGS, "Id": "Settings", "Name": "NIC_0 Settings",
                    "Oem": {"Nvidia": nvidia(false)},
                }),
            )
            .build()
            .await
    }

    /// The read comes from the adapter; the change is staged on its settings
    /// object with only the east-west control in the body.
    #[tokio::test]
    async fn east_west_control_reads_the_adapter_and_writes_its_settings() {
        let bmc = vera_rubin().await;
        let cx = bmc.cx().await;

        assert_eq!(
            VeraRubinSuperNic.east_west_control(&cx, 0).await,
            Ok(Some(false))
        );
        assert_eq!(
            VeraRubinSuperNic.set_east_west_control(&cx, 0, true).await,
            Ok(DriverOutcome::complete())
        );
        let writes = bmc.writes();
        assert_eq!(
            writes.iter().map(path).collect::<Vec<_>>(),
            [NIC_0_SETTINGS]
        );
        assert_eq!(
            body(&writes[0]),
            json!({"Oem": {"Nvidia": {"EastWestControlEnabled": true}}})
        );
    }

    #[tokio::test]
    async fn a_nic_without_its_chassis_is_an_error() {
        let bmc = vera_rubin().await;
        let cx = bmc.cx().await;

        assert_eq!(
            VeraRubinSuperNic.east_west_control(&cx, 1).await,
            Err(PlatformError::InvalidResponse {
                message: "SuperNIC 1: no chassis CX_1".to_string(),
            })
        );
    }
}
