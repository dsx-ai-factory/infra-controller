// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! VR NVL72 Delta AHE54V2037A, captured with PMC 3.3.8 and PSU 04320409.
//! The recorded tree uses PMC_0, PowerShelf_0 and Chassis.Reset; it does not
//! advertise legacy PowerShelf.TurnOnPSUs/TurnOffPSUs OEM actions or Systems.

use std::borrow::Cow;

use mac_address::MacAddress;

use crate::{Callbacks, ResourceResetType, redfish};

const CHASSIS_ID: &str = "PowerShelf_0";
const PMC_ID: &str = "PMC_0";

pub(crate) struct DeltaVrnvl72PowerShelf<'a> {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) product_serial_number: Cow<'a, str>,
}

impl DeltaVrnvl72PowerShelf<'_> {
    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: PMC_ID,
                eth_interfaces: Some(vec![
                    redfish::ethernet_interface::builder(
                        &redfish::ethernet_interface::manager_resource(PMC_ID, "eth0"),
                    )
                    .mac_address(self.bmc_mac_address.to_string())
                    .interface_enabled(true)
                    .build(),
                ]),
                host_interfaces: None,
                serial_interfaces: None,
                firmware_version: Some("3.3.8"),
                oem: None,
            }],
        }
    }

    pub(crate) fn system_config<C: Callbacks>(&self) -> redfish::computer_system::Config<C> {
        redfish::computer_system::Config { systems: vec![] }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        redfish::chassis::ChassisConfig {
            chassis: vec![redfish::chassis::SingleChassisConfig {
                id: CHASSIS_ID.into(),
                chassis_type: redfish::schema::chassis::ChassisType::RackMount,
                manufacturer: Some("DELTA".into()),
                model: Some("AHE54V2037A".into()),
                part_number: Some("ECD68000089-B".into()),
                sku: Some("AHE54V2037A".into()),
                serial_number: Some(self.product_serial_number.to_string().into()),
                power_supplies: Some(
                    (1..=6)
                        .map(|bay| {
                            redfish::power_supply::PowerSupply::delta(
                                &redfish::power_supply::resource(CHASSIS_ID, &bay.to_string()),
                                true,
                                18496.0,
                                0,
                                Some("04320409"),
                            )
                        })
                        .collect(),
                ),
                power_control: Some(redfish::chassis::PowerControlConfig {
                    initial_powered_on: true,
                    allowed_reset_types: vec![
                        ResourceResetType::On,
                        ResourceResetType::ForceOn,
                        ResourceResetType::ForceOff,
                    ],
                }),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }],
        }
    }

    pub(crate) fn event_service_config(&self) -> Option<crate::EventServiceConfig> {
        Some(crate::EventServiceConfig::default())
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        let mut firmware = vec![
            ("FW_ERoT_BMC_0".to_string(), "1206"),
            ("FW_PMC_alt".to_string(), "3.3.6"),
            ("FW_PMC_primary".to_string(), "3.3.8"),
        ];
        firmware.extend((1..=6).map(|bay| (format!("FW_PSU_{bay}"), "04320409")));
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: firmware
                .into_iter()
                .map(|(id, version)| {
                    redfish::software_inventory::builder(
                        &redfish::software_inventory::firmware_inventory_resource(&id),
                    )
                    .version(version)
                    .updateable(true)
                    .build()
                })
                .collect(),
            ..Default::default()
        }
    }

    pub(crate) fn power_equipment_config(&self) -> redfish::power_equipment::Config {
        use redfish::schema::power_distribution::{
            PowerDistribution, PowerDistributionRequired, PowerEquipmentType,
        };
        redfish::power_equipment::Config {
            power_shelves: vec![
                PowerDistribution::new(PowerDistributionRequired {
                    odata_id: "/redfish/v1/PowerEquipment/PowerShelves/1".into(),
                    id: "1".into(),
                    name: "Power Shelf Configuration".into(),
                    equipment_type: PowerEquipmentType::PowerShelf,
                })
                .manufacturer("DELTA")
                .model("AHE54V2037A")
                .serial_number(self.product_serial_number.as_ref())
                .firmware_version("3.3.8")
                .power_supplies("/redfish/v1/Chassis/PowerShelf_0/PowerSubsystem/PowerSupplies"),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::test_support::{TestBmcConfig, create_test_bmc, host_info};
    use crate::{HardwareType, MachineRouterOptions};

    async fn get(router: &Router, path: &str) -> (StatusCode, Value) {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn vr_delta_profile_matches_capture_identity_firmware_and_action_layout() {
        let (router, _) = create_test_bmc(
            &host_info(HardwareType::DeltaVrnvl72PowerShelf),
            TestBmcConfig::default(),
            "vr-delta".into(),
            false,
            MachineRouterOptions::default(),
        );
        let captured = crate::test_support::recorded_delta_vrnvl72_3_3_8_router();
        let root = get(&router, "/redfish/v1").await.1;
        assert!(root.get("Systems").is_none());
        assert!(root.get("PowerEquipment").is_some());
        assert_eq!(
            get(&router, "/redfish/v1/Systems").await.0,
            StatusCode::NOT_FOUND
        );
        for path in [
            "/redfish/v1/Managers/PMC_0",
            "/redfish/v1/Chassis/PowerShelf_0",
        ] {
            let (status, actual) = get(&router, path).await;
            assert_eq!(status, StatusCode::OK);
            let recorded = get(&captured, path).await.1;
            for key in if path.contains("Managers") {
                vec!["Id", "FirmwareVersion"]
            } else {
                vec!["Id", "Model", "Manufacturer"]
            } {
                assert_eq!(actual[key], recorded[key], "{path} {key}");
            }
        }
        let inventory = get(&router, "/redfish/v1/UpdateService/FirmwareInventory")
            .await
            .1;
        assert_eq!(inventory["Members@odata.count"], 9);
        for member in inventory["Members"].as_array().unwrap() {
            let path = member["@odata.id"].as_str().unwrap();
            let actual = get(&router, path).await.1;
            let recorded = get(&captured, path).await.1;
            assert_eq!(actual["Version"], recorded["Version"], "{path}");
        }
        let shelf = get(&router, "/redfish/v1/PowerEquipment/PowerShelves/1")
            .await
            .1;
        assert_eq!(shelf["EquipmentType"], "PowerShelf");
        assert_eq!(shelf["Manufacturer"], "DELTA");
        assert_eq!(shelf["FirmwareVersion"], "3.3.8");
        assert!(shelf.pointer("/Oem/deltaenergysystems/Actions").is_none());
        let info = get(&router, "/redfish/v1/Chassis/PowerShelf_0/ResetActionInfo")
            .await
            .1;
        assert_eq!(
            info["Parameters"][0]["AllowableValues"],
            json!(["On", "ForceOn", "ForceOff"])
        );
        assert_eq!(
            get(&router, "/redfish/v1/PowerEquipment/PowerShelves/missing")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn chassis_power_updates_all_psus_and_rejects_unadvertised_resets() {
        let (router, state) = create_test_bmc(
            &host_info(HardwareType::DeltaVrnvl72PowerShelf),
            TestBmcConfig::default(),
            "vr-delta".into(),
            false,
            MachineRouterOptions::default(),
        );
        for (reset, status, on) in [
            ("ForceOff", StatusCode::NO_CONTENT, false),
            ("ForceRestart", StatusCode::BAD_REQUEST, false),
            ("GracefulShutdown", StatusCode::BAD_REQUEST, false),
            ("On", StatusCode::NO_CONTENT, true),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/redfish/v1/Chassis/PowerShelf_0/Actions/Chassis.Reset")
                        .header("Content-Type", "application/json")
                        .body(Body::from(json!({"ResetType": reset}).to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{reset}");
            let chassis = get(&router, "/redfish/v1/Chassis/PowerShelf_0").await.1;
            assert_eq!(chassis["PowerState"], if on { "On" } else { "Off" });
            for bay in 1..=6 {
                let psu = get(
                    &router,
                    &format!("/redfish/v1/Chassis/PowerShelf_0/PowerSubsystem/PowerSupplies/{bay}"),
                )
                .await
                .1;
                assert_eq!(psu["Oem"]["deltaenergysystems"]["Power"], on);
            }
        }
        assert_eq!(
            state
                .callbacks
                .as_ref()
                .unwrap()
                .commands
                .lock()
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn legacy_delta_does_not_gain_vr_power_capabilities() {
        let (router, _) = create_test_bmc(
            &host_info(HardwareType::DeltaPowerShelf),
            TestBmcConfig::default(),
            "legacy-delta".into(),
            false,
            MachineRouterOptions::default(),
        );
        let root = get(&router, "/redfish/v1").await.1;
        assert!(root.get("PowerEquipment").is_none());
        assert_eq!(
            get(&router, "/redfish/v1/PowerEquipment").await.0,
            StatusCode::NOT_FOUND
        );
        let legacy = get(&router, "/redfish/v1/Chassis/chassis").await.1;
        assert_eq!(legacy["Model"], "810");
        assert!(legacy.get("Actions").is_none());
        assert_eq!(
            get(&router, "/redfish/v1/Chassis/chassis/ResetActionInfo")
                .await
                .0,
            StatusCode::NOT_FOUND
        );
    }
}
