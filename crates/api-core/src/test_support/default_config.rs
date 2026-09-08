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

//! Contains fixtures that use the Carbide API for setting up

use std::collections::HashMap;
use std::default::Default;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use carbide_host_support::bootstrap_ca::BootstrapCaSource;
use carbide_ib_fabric::config::IbFabricDefinition;
use carbide_machine_controller::config::{
    BomValidationConfig, FirmwareGlobal, MachineStateControllerConfig, MachineValidationConfig,
    PowerManagerOptions,
};
use carbide_nvlink_manager::config::NvLinkConfig;
use carbide_rack_controller::config::{RackValidationConfig, RmsConfig};
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_state_controller_common::config::StateControllerConfig;
use chrono::Duration;
use model::firmware::{Firmware, FirmwareComponent, FirmwareComponentType, FirmwareEntry};
use model::machine::HostHealthConfig;
use model::resource_pool::{self};
use regex::Regex;

use crate::cfg::file::{
    AuthConfig, CarbideConfig, DpaInterfaceStateControllerConfig, DpuConfig as InitialDpuConfig,
    DsxExchangeEventBusConfig, EwEthersConfig, ExtensionServiceStateControllerConfig, FnnConfig,
    IbPartitionStateControllerConfig, KmsConfig, ListenMode, MachineUpdater,
    MeasuredBootMetricsCollectorConfig, MqttAuthConfig, NetworkSecurityGroupConfig,
    NetworkSegmentStateControllerConfig, NodeAuthConfig, PowerShelfStateControllerConfig,
    RackStateControllerConfig, SecretsConfig, SpdmConfig, SpdmStateControllerConfig, SvpcConfig,
    SwitchStateControllerConfig, TracingConfig, VmaasConfig, VpcPeeringPolicy,
    VpcPrefixStateControllerConfig, default_bmc_session_lockout_threshold,
    default_database_pool_acquire_timeout, default_database_pool_idle_timeout,
    default_database_pool_max_lifetime, default_max_find_by_ids,
    default_max_site_prefixes_per_tenant, default_pxe_public_base_url,
};
#[cfg(test)]
use crate::cfg::file::{
    DpfInterfaceIdentity, HostInterceptBridging, HostRepresentorBridgingConfig,
};

/// Returns the default configuration with one selected PF and the requested sparse DPF VFs.
#[cfg(test)]
pub(crate) fn with_dpf_intercept_topology(selected_vfs: &[u8]) -> CarbideConfig {
    let mut config = get();
    config.dpf.enabled = true;

    let mut interfaces = HashMap::from([(
        "selected-pf".to_string(),
        HostInterceptBridging {
            bridge: "br-pf".to_string(),
            patch_port: "p-pf".to_string(),
            skip_create: false,
            dpf_interface: Some(DpfInterfaceIdentity {
                controller_id: 1,
                pf_id: 0,
                vf_id: None,
            }),
        },
    )]);
    interfaces.extend(selected_vfs.iter().map(|vf_id| {
        (
            format!("selected-vf{vf_id}"),
            HostInterceptBridging {
                bridge: format!("br-vf{vf_id}"),
                patch_port: format!("p-vf{vf_id}"),
                skip_create: false,
                dpf_interface: Some(DpfInterfaceIdentity {
                    controller_id: 1,
                    pf_id: 0,
                    vf_id: Some(*vf_id),
                }),
            },
        )
    }));
    config.vmaas_config = Some(VmaasConfig {
        allow_instance_vf: true,
        hbn_reps: None,
        bridging: Some(HostRepresentorBridgingConfig {
            hbn_bridge: "br-hbn".to_string(),
            host_representor_intercept_bridging: interfaces,
        }),
    });
    config
}

/// [`get`] with every `Option` config section populated. Used by tests that
/// walk the *serialized* config shape — e.g. the admin-UI documentation
/// guards, which can only verify sections and fields that actually serialize.
/// When a new `Option` section is added to [`CarbideConfig`] (the compiler
/// forces it into [`get`]), populate it here too. The
/// `fmds_use_node_tokens` inner `Option` below is populated so its documented
/// row is covered. New `skip_serializing_if` fields need the same explicit
/// treatment when they should be checked; this is not compiler-enforced.
/// `mlxconfig_profiles` is intentionally exempted by `SKIP_SERIALIZING` in
/// `crates/api-web/src/configuration.rs`.
pub fn fully_populated() -> CarbideConfig {
    CarbideConfig {
        auth: Some(AuthConfig {
            permissive_mode: true,
            casbin_policy_file: None,
            cli_certs: None,
            trust: None,
        }),
        ib_config: Some(carbide_ib_fabric::config::IBFabricConfig::default()),
        fnn: Some(FnnConfig {
            admin_vpc: None,
            common_internal_route_target: None,
            additional_route_target_imports: vec![],
            routing_profiles: HashMap::new(),
            use_vpc_vrf_loopback: false,
        }),
        dsx_exchange_event_bus: Some(DsxExchangeEventBusConfig::default()),
        secrets: Some(SecretsConfig {
            kms: KmsConfig {
                active: "local".to_string(),
                providers: HashMap::new(),
            },
            routing: HashMap::from([("/".to_string(), "kek".to_string())]),
            backends: vec![],
            writer: Default::default(),
            import_from: None,
            import_approach: Default::default(),
        }),
        node_auth: NodeAuthConfig {
            fmds_use_node_tokens: Some(false),
            ..Default::default()
        },
        ..get()
    }
}

pub fn get() -> CarbideConfig {
    CarbideConfig {
        default_tenant_routing_profile_type: "EXTERNAL".to_string(),
        enable_admin_ui: true,
        web_ui_sidebar_tools: vec![],
        web_ui_logs_link_template: String::new(),
        log_history: Default::default(),
        node_auth: Default::default(),
        observability: Default::default(),
        bgp_leaf_session_password: None,
        rack_validation_config: RackValidationConfig {
            enabled: true,
            ..Default::default()
        },
        site_global_vpc_vni: None,
        listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 1079),
        metrics_endpoint: None,
        alt_metric_prefix: None,
        database_url: "pgsql:://localhost".to_string(),
        max_database_connections: 1000,
        deny_unknown_fields: false,
        database_pool_acquire_timeout: default_database_pool_acquire_timeout(),
        database_pool_idle_timeout: default_database_pool_idle_timeout(),
        database_pool_max_lifetime: default_database_pool_max_lifetime(),
        api_admission_control: Default::default(),
        compute_allocation_enforcement: Default::default(),
        asn: 0,
        datacenter_asn: 0,
        dhcp_servers: vec![],
        route_servers: vec![],
        enable_route_servers: false,
        deny_prefixes: vec![],
        site_fabric_prefixes: vec![],
        tenant_prefix_overlap_enabled: false,
        max_site_prefixes_per_tenant: default_max_site_prefixes_per_tenant(),
        anycast_site_prefixes: vec![],
        common_tenant_host_asn: None,
        vpc_isolation_behavior: <_ as Default>::default(),
        host_naming_strategy: <_ as Default>::default(),
        tls: Some(crate::cfg::file::TlsConfig {
            root_cafile_path: "Not a real path".to_string(),
            identity_pemfile_path: "Not a real pemfile".to_string(),
            identity_keyfile_path: "Not a real keyfile".to_string(),
            admin_root_cafile_path: "Not a real cafile".to_string(),
        }),
        auth: None,
        pools: None,
        networks: None,
        vpcs: None,
        dpu_ipmi_tool_impl: None,
        dpu_ipmi_reboot_attempts: Some(0),
        bmc_session_lockout_threshold: default_bmc_session_lockout_threshold(),
        bmc_max_sessions_per_caller: crate::cfg::file::default_bmc_max_sessions_per_caller(),
        allow_bmc_basic_auth_fallback: false,
        allow_insecure_discovery: true,
        scout_boot_interface_correction_enabled: false,
        initial_domain_name: Some("test.com".to_string()),
        sitename: Some("testsite".to_string()),
        initial_dpu_agent_upgrade_policy: None,
        max_concurrent_machine_updates: None,
        machine_update_run_interval: Some(1),
        retained_boot_interface_window: None,
        site_explorer: SiteExplorerConfig {
            enabled: Arc::new(false.into()),
            run_interval: std::time::Duration::from_secs(0),
            concurrent_explorations: 0,
            explorations_per_run: 0,
            create_machines: Arc::new(false.into()),
            ..Default::default()
        },
        deprecated_force_dpu_nic_mode: None,
        vpc_peering_policy: Some(VpcPeeringPolicy::Exclusive),
        vpc_peering_policy_on_existing: None,
        attestation_enabled: false,
        bmc_rotation_enabled: false,
        uefi_rotation_enabled: false,
        nic_lockdown_ikm_rotation_enabled: false,
        bmc_factory_reset_on_instance_termination_enabled: false,
        tpm_required: true,
        ib_config: None,
        ib_fabrics: [(
            "default".to_string(),
            IbFabricDefinition {
                // The actual IP is not used and thereby does not matter
                endpoints: vec!["https://127.0.0.1:443".to_string()],
                pkeys: vec![resource_pool::Range {
                    start: "1".to_string(),
                    end: "100".to_string(),
                    auto_assign: true,
                }],
            },
        )]
        .into_iter()
        .collect(),
        machine_state_controller: MachineStateControllerConfig::test_default(),
        network_segment_state_controller: NetworkSegmentStateControllerConfig {
            network_segment_drain_time: Duration::seconds(2),
            controller: StateControllerConfig::default(),
        },
        vpc_prefix_state_controller: VpcPrefixStateControllerConfig {
            vpc_prefix_drain_time: Duration::seconds(2),
            controller: StateControllerConfig::default(),
        },
        extension_service_state_controller: ExtensionServiceStateControllerConfig {
            controller: StateControllerConfig::default(),
        },
        ib_partition_state_controller: IbPartitionStateControllerConfig {
            controller: StateControllerConfig::default(),
        },
        dpa_interface_state_controller: DpaInterfaceStateControllerConfig {
            controller: StateControllerConfig::default(),
        },
        power_shelf_state_controller: PowerShelfStateControllerConfig {
            rack_firmware_reprovisioning_enabled: false,
            controller: StateControllerConfig::default(),
        },
        rack_state_controller: RackStateControllerConfig {
            controller: StateControllerConfig::default(),
            nmx_cluster_switch_mtls_services: vec![],
        },
        switch_state_controller: SwitchStateControllerConfig {
            controller: StateControllerConfig::default(),
            switch_mtls_services: vec![],
        },
        dpu_config: InitialDpuConfig {
            bootstrap_ca_source: BootstrapCaSource::LegacyDownload,
            dpu_nic_firmware_initial_update_enabled: true,
            dpu_nic_firmware_reprovision_update_enabled: true,
            dpu_models: dpu_fw_example(),
            dpu_nic_firmware_update_versions: vec!["24.42.1000".to_string()],
            dpu_enable_secure_boot: true,
            num_of_vfs: crate::cfg::file::DEFAULT_DPU_NUM_OF_VFS,
            service_vpc_slot_count: 0,
            additional_managed_sf: 0,
            restart_ovs_on_use_admin_network_change: false,
        },
        host_models: host_firmware_example(),
        firmware_global: FirmwareGlobal::test_default(),
        machine_updater: MachineUpdater {
            instance_autoreboot_period: None,
            max_concurrent_machine_updates_absolute: Some(10),
            max_concurrent_machine_updates_percent: None,
        },
        max_find_by_ids: default_max_find_by_ids(),
        network_security_group: NetworkSecurityGroupConfig::default(),
        min_dpu_functioning_links: None,
        dpu_network_monitor_pinger_type: None,
        host_health: HostHealthConfig::default(),
        internet_l3_vni: 1337,
        measured_boot_collector: MeasuredBootMetricsCollectorConfig {
            enabled: true,
            run_interval: std::time::Duration::from_secs(10),
        },
        machine_validation_config: MachineValidationConfig {
            enabled: true,
            ..MachineValidationConfig::default()
        },
        bypass_rbac: false,
        fnn: None,
        bios_profiles: HashMap::default(),
        selected_profile: libredfish::BiosProfileType::Performance,
        oem_manager_profiles: HashMap::default(),
        bom_validation: BomValidationConfig::default(),
        listen_mode: ListenMode::Tls,
        listen_only: false,
        nvlink_config: Some(NvLinkConfig::default()),
        ewethers_config: Some(EwEthersConfig {
            enabled: true,
            svpc_enabled: true,
            astra_enabled: false,
            subnet_ip: Ipv4Addr::UNSPECIFIED,
            subnet_mask: 0_i32,
            monitor_run_interval: std::time::Duration::from_secs(10),
            svpc: SvpcConfig {
                mqtt_endpoint: "mqtt.forge".to_string(),
                mqtt_broker_port: 1884_u16,
                hb_interval: Duration::minutes(2),
                auth: MqttAuthConfig::default(),
            },
        }),
        power_manager_options: PowerManagerOptions {
            enabled: false,
            ..PowerManagerOptions::default()
        },
        auto_machine_repair_plugin: Default::default(),
        vmaas_config: Some(VmaasConfig {
            allow_instance_vf: true,
            hbn_reps: None,
            bridging: None,
        }),
        mlxconfig_profiles: None,
        rack_management_enabled: false,
        rms: RmsConfig::default(),
        rack_profiles: Default::default(),
        spdm_state_controller: SpdmStateControllerConfig {
            controller: StateControllerConfig::default(),
        },
        spdm: SpdmConfig {
            enabled: false,
            nras_config: Some(nras::Config::default()),
        },
        machine_identity: crate::cfg::file::MachineIdentityConfig {
            enabled: true,
            current_encryption_key_id: Some("test".to_string()),
            ..Default::default()
        },
        dsx_exchange_event_bus: None,
        dpf: crate::cfg::file::DpfConfig::default(),
        x86_pxe_boot_url_override: None,
        arm_pxe_boot_url_override: None,
        pxe_public_base_url: default_pxe_public_base_url(),
        set_http_boot_uri_for_vendors: vec![],
        external_api_url: None,
        external_pxe_url: None,
        external_static_pxe_url: None,
        supernic_firmware_profiles: HashMap::default(),
        component_manager: None,
        initial_objects_file: None,
        config_ctx: None,
        tracing: TracingConfig::default(),
        ntp_servers: vec![],
        secrets: None,
        credentials: Default::default(),
        dhcp_lease_expiry_handling: false,
        certificates: Default::default(),
    }
}

fn dpu_fw_example() -> HashMap<String, Firmware> {
    HashMap::from([(
        "bluefield3".to_string(),
        Firmware {
            vendor: bmc_vendor::BMCVendor::Nvidia,
            model: "BlueField 3 SmartNIC Main Card".to_string(),
            ordering: vec![FirmwareComponentType::Bmc, FirmwareComponentType::Cec],
            explicit_start_needed: false,
            components: HashMap::from([
                (
                    FirmwareComponentType::Bmc,
                    FirmwareComponent {
                        current_version_reported_as: Some(Regex::new("BMC_Firmware").unwrap()),
                        preingest_upgrade_when_below: None,
                        known_firmware: vec![FirmwareEntry::standard("BF-24.10-17")],
                    },
                ),
                (
                    FirmwareComponentType::Cec,
                    FirmwareComponent {
                        current_version_reported_as: Some(Regex::new("Bluefield_FW_ERoT").unwrap()),
                        preingest_upgrade_when_below: None,
                        known_firmware: vec![FirmwareEntry::standard("00.02.0180.0000")],
                    },
                ),
                (
                    FirmwareComponentType::Nic,
                    FirmwareComponent {
                        current_version_reported_as: Some(Regex::new("DPU_NIC").unwrap()),
                        preingest_upgrade_when_below: None,
                        known_firmware: vec![FirmwareEntry::standard("32.39.2048")],
                    },
                ),
            ]),
        },
    )])
}

fn host_firmware_example() -> HashMap<String, Firmware> {
    HashMap::from([
        (
            "1".to_string(),
            Firmware {
                vendor: bmc_vendor::BMCVendor::Dell,
                model: "PowerEdge R750".to_string(),
                explicit_start_needed: false,
                components: HashMap::from([
                    (
                        FirmwareComponentType::Bmc,
                        FirmwareComponent {
                            current_version_reported_as: Some(
                                Regex::new("^Installed-.*__iDRAC.").unwrap(),
                            ),
                            preingest_upgrade_when_below: Some("5".to_string()),
                            known_firmware: vec![
                                FirmwareEntry::standard_notdefault("6.1"),
                                FirmwareEntry::standard_multiple_filenames("6.00.30.00"),
                                FirmwareEntry::standard_notdefault("5"),
                            ],
                        },
                    ),
                    (
                        FirmwareComponentType::Uefi,
                        FirmwareComponent {
                            current_version_reported_as: Some(
                                Regex::new("^Current-.*__BIOS.Setup.").unwrap(),
                            ),
                            preingest_upgrade_when_below: Some("1.13.2".to_string()),
                            known_firmware: vec![FirmwareEntry::standard("1.13.2")],
                        },
                    ),
                ]),
                ordering: vec![FirmwareComponentType::Uefi, FirmwareComponentType::Bmc],
            },
        ),
        (
            "2".to_string(),
            Firmware {
                vendor: bmc_vendor::BMCVendor::Dell,
                model: "Powercycle Test".to_string(),
                explicit_start_needed: false,
                components: HashMap::from([(
                    FirmwareComponentType::Uefi,
                    FirmwareComponent {
                        current_version_reported_as: Some(
                            Regex::new("^Current-.*__BIOS.Setup.").unwrap(),
                        ),
                        preingest_upgrade_when_below: Some("1.13.2".to_string()),
                        known_firmware: vec![FirmwareEntry::standard_powerdrains("1.13.2", 1002)],
                    },
                )]),
                ordering: vec![FirmwareComponentType::Uefi, FirmwareComponentType::Bmc],
            },
        ),
    ])
}
