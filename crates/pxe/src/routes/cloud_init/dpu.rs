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

//! The BlueField kickstart served to a DPU during provisioning.
//!
//! The DPU reaches this from `bfks=` on its kernel command line and fetches
//! `user-data` and nothing else, so this prefix carries only that document.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum_template::TemplateEngine;
use base64::Engine as _;
use carbide_host_support::agent_config;
use carbide_host_support::bootstrap_ca::BootstrapCaSource;
use carbide_instrument::emit;
use carbide_libmlx_model::nvconfig::DpuNvConfigProfile;
use carbide_uuid::machine::MachineInterfaceId;
use rpc::forge;
use rpc::forge::PxeDomain;

use super::log_and_generate_generic_error;
use crate::common::{AppState, Machine};
use crate::metrics::{BootEndpoint, CloudInitConsumer, OutcomeReason, PxeBootOutcome};

/// Every outcome on this route carries the same endpoint label.
const ENDPOINT: BootEndpoint = BootEndpoint::CloudInit(CloudInitConsumer::Dpu);

const DEFAULT_NUM_OF_VFS: u32 = 16;
const DEFAULT_HBN_BRIDGE: &str = "br-hbn";

fn parse_bootstrap_ca_source(value: i32) -> Result<BootstrapCaSource, String> {
    forge::BootstrapCaSource::try_from(value)
        .map(BootstrapCaSource::from)
        .map_err(|_| format!("unknown bootstrap CA source value {value}"))
}

fn parse_dpu_nvconfig_profile(value: i32) -> Result<Option<DpuNvConfigProfile>, String> {
    let profile = forge::DpuNvConfigProfile::try_from(value)
        .map_err(|_| format!("unknown DPU NVConfig profile value {value}"))?;
    match profile {
        forge::DpuNvConfigProfile::Unspecified => Ok(None),
        forge::DpuNvConfigProfile::Gb200B3240V1 => Ok(Some(DpuNvConfigProfile::Gb200B3240V1)),
    }
}

fn dpu_nvconfig_parameters(profile: Option<DpuNvConfigProfile>) -> String {
    profile
        .map(|profile| profile.parameters().join(" "))
        .unwrap_or_default()
}

/// Generates the content of the /etc/forge/config.toml file.
///
/// When `api_url_override` is provided (for external hosts on the
/// static-assignments segment), it's written into the `[forge-system]`
/// section so the DPU agent connects to the correct API endpoint
/// instead of defaulting to `carbide-api.forge`.
fn generate_forge_agent_config(
    machine_interface_id: MachineInterfaceId,
    api_url_override: Option<&str>,
) -> String {
    let config = agent_config::AgentConfigFromPxe {
        forge_system: api_url_override.map(|url| agent_config::ForgeSystemConfigFromPxe {
            api_server: url.to_string(),
        }),
        machine: agent_config::MachineConfigFromPxe {
            interface_id: machine_interface_id,
        },
    };

    toml::to_string(&config).unwrap_or_else(|e| format!("# serialization error: {e}"))
}

#[allow(clippy::too_many_arguments)]
fn render_user_data(
    machine_interface_id: MachineInterfaceId,
    machine_interface: forge::MachineInterface,
    domain: PxeDomain,
    hbn_reps: Option<String>,
    num_of_vfs: Option<u32>,
    host_representor_intercept_bridging: Option<String>,
    hbn_bridge: Option<String>,
    api_url_override: Option<String>,
    pxe_url_override: Option<String>,
    bootstrap_ca_source: BootstrapCaSource,
    dpu_nvconfig_profile: Option<DpuNvConfigProfile>,
    state: State<AppState>,
) -> (String, HashMap<String, String>) {
    let config = state.runtime_config.clone();
    let forge_agent_config =
        generate_forge_agent_config(machine_interface_id, api_url_override.as_deref());

    let mut context: HashMap<String, String> = HashMap::new();
    context.insert("mac_address".to_string(), machine_interface.mac_address);

    if let Some(domain_oneof) = domain.domain {
        let domain_name = match domain_oneof {
            forge::pxe_domain::Domain::LegacyDomain(domain) => domain.name,
            forge::pxe_domain::Domain::NewDomain(domain) => domain.name,
        };
        context.insert(
            "hostname".to_string(),
            format!("{}.{}", machine_interface.hostname, domain_name),
        );
    }
    context.insert("interface_id".to_string(), machine_interface_id.to_string());
    context.insert(
        "api_url".to_string(),
        api_url_override.unwrap_or(config.client_facing_api_url),
    );
    context.insert(
        "pxe_url".to_string(),
        pxe_url_override.unwrap_or(config.pxe_url),
    );
    context.insert(
        "forge_agent_config_b64".to_string(),
        base64::engine::general_purpose::STANDARD.encode(forge_agent_config),
    );
    context.insert(
        "bootstrap_ca_source".to_string(),
        bootstrap_ca_source.to_string(),
    );
    context.insert(
        "forge_dpu_nvconfig_parameters".to_string(),
        dpu_nvconfig_parameters(dpu_nvconfig_profile),
    );

    let bmc_fw_update = state
        .engine
        .render("bmc_fw_update", HashMap::<String, String>::new())
        .unwrap_or("".to_string());
    context.insert("forge_bmc_fw_update".to_string(), bmc_fw_update);

    let seconds_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_secs();

    context.insert(
        "seconds_since_epoch".to_string(),
        seconds_since_epoch.to_string(),
    );

    if let Some(hbn_reps) = hbn_reps {
        context.insert("forge_hbn_reps".to_string(), hbn_reps);
    }

    let num_of_vfs = num_of_vfs.unwrap_or(DEFAULT_NUM_OF_VFS);
    context.insert("num_of_vfs".to_string(), num_of_vfs.to_string());
    context.insert(
        "forge_hbn_bridge".to_string(),
        hbn_bridge.unwrap_or_else(|| DEFAULT_HBN_BRIDGE.to_string()),
    );

    if let Some(host_representor_intercept_bridging) = host_representor_intercept_bridging {
        context.insert(
            "forge_host_representor_intercept_bridging".to_string(),
            host_representor_intercept_bridging,
        );
    }

    ("user-data".to_string(), context)
}

/// Serves the DPU's kickstart: a per-machine boot override when one is set,
/// otherwise the provisioning script rendered from the machine's discovery
/// instructions.
///
/// The override is checked first and is not a tenant document despite sharing
/// the `user-data-assigned` template: it is set per machine interface and
/// exists to test one-off changes against a single DPU.
async fn user_data(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let (template_key, template_data) = match (
        machine.instructions.custom_cloud_init,
        machine.instructions.discovery_instructions,
    ) {
        // Machine boot overrides replace the complete generated payload,
        // including platform setup performed by the discovery template.
        (Some(custom_cloud_init), _) => {
            let mut template_data: HashMap<String, String> = HashMap::new();
            template_data.insert("user_data".to_string(), custom_cloud_init);
            emit(PxeBootOutcome {
                endpoint: ENDPOINT,
                reason: OutcomeReason::Ok,
            });
            ("user-data-assigned".to_string(), template_data)
        }
        (None, Some(discovery_instructions)) => {
            match (
                discovery_instructions.machine_interface,
                discovery_instructions.domain,
            ) {
                (Some(interface), Some(domain)) => match interface.id {
                    Some(machine_interface_id) => {
                        let bootstrap_ca_source =
                            parse_bootstrap_ca_source(discovery_instructions.bootstrap_ca_source);
                        let dpu_nvconfig_profile =
                            parse_dpu_nvconfig_profile(discovery_instructions.dpu_nvconfig_profile);

                        match (bootstrap_ca_source, dpu_nvconfig_profile) {
                            (Ok(bootstrap_ca_source), Ok(dpu_nvconfig_profile)) => {
                                emit(PxeBootOutcome {
                                    endpoint: ENDPOINT,
                                    reason: OutcomeReason::Ok,
                                });
                                render_user_data(
                                    machine_interface_id,
                                    interface,
                                    domain,
                                    discovery_instructions.hbn_reps,
                                    discovery_instructions.num_of_vfs,
                                    discovery_instructions.host_representor_intercept_bridging,
                                    discovery_instructions.hbn_bridge,
                                    machine.instructions.api_url_override,
                                    machine.instructions.pxe_url_override,
                                    bootstrap_ca_source,
                                    dpu_nvconfig_profile,
                                    state.clone(),
                                )
                            }
                            (Err(error), _) | (_, Err(error)) => log_and_generate_generic_error(
                                error,
                                OutcomeReason::InstructionsInvalid,
                                ENDPOINT,
                            ),
                        }
                    }
                    None => log_and_generate_generic_error(
                        format!("The interface ID should not be null: {interface:?}"),
                        OutcomeReason::InterfaceNotFound,
                        ENDPOINT,
                    ),
                },
                (interface, domain) => log_and_generate_generic_error(
                    format!("The interface and domain were not found: {interface:?}, {domain:?}"),
                    OutcomeReason::InterfaceNotFound,
                    ENDPOINT,
                ),
            }
        }
        // Reaching this prefix at all means the machine booted as a DPU, so
        // there is nothing sensible to serve without instructions -- unlike
        // the tenant path, an empty document would leave it unprovisioned.
        (None, None) => log_and_generate_generic_error(
            "No discovery instructions were found for this DPU".to_string(),
            OutcomeReason::InstructionsEmpty,
            ENDPOINT,
        ),
    };

    axum_template::Render(template_key, state.engine.clone(), template_data)
}

/// Builds the DPU's route table under `path_prefix`.
pub(crate) fn get_router(path_prefix: &str) -> Router<AppState> {
    Router::new().route(
        format!("{}/{}", path_prefix, "user-data").as_str(),
        get(user_data),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use carbide_instrument::testing::MetricsCapture;
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::common::test_app_state;

    const TEST_DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pxe/test_data");

    /// Renders the `user-data` template from a hand-built context.
    fn render_user_data_test_template(
        bootstrap_ca_source: BootstrapCaSource,
        dpu_nvconfig_profile: Option<DpuNvConfigProfile>,
    ) -> String {
        let template_glob = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pxe/templates/**/*");
        let tera = tera::Tera::new(template_glob).unwrap();
        let context = HashMap::from([
            (
                "bootstrap_ca_source".to_string(),
                bootstrap_ca_source.to_string(),
            ),
            (
                "api_url".to_string(),
                "https://carbide-api.forge".to_string(),
            ),
            (
                "forge_agent_config_b64".to_string(),
                "W21hY2hpbmVdCg==".to_string(),
            ),
            ("forge_bmc_fw_update".to_string(), String::new()),
            (
                "forge_dpu_nvconfig_parameters".to_string(),
                dpu_nvconfig_parameters(dpu_nvconfig_profile),
            ),
            ("forge_hbn_reps".to_string(), String::new()),
            ("forge_hbn_bridge".to_string(), "br-hbn".to_string()),
            ("hostname".to_string(), "test-host".to_string()),
            (
                "interface_id".to_string(),
                "91609f10-c91d-470d-a260-6293ea0c1234".to_string(),
            ),
            ("num_of_vfs".to_string(), "3".to_string()),
            (
                "pxe_url".to_string(),
                "http://carbide-pxe.forge".to_string(),
            ),
            ("seconds_since_epoch".to_string(), "0".to_string()),
        ]);

        tera.render(
            "user-data",
            &tera::Context::from_serialize(context).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn bootstrap_ca_source_protobuf_values_fail_closed() {
        check_values(
            [
                Check {
                    scenario: "legacy download preserves historical command without local validation",
                    input: forge::BootstrapCaSource::LegacyDownload as i32,
                    expect: Ok(BootstrapCaSource::LegacyDownload),
                },
                Check {
                    scenario: "embedded",
                    input: forge::BootstrapCaSource::Embedded as i32,
                    expect: Ok(BootstrapCaSource::Embedded),
                },
                Check {
                    scenario: "mounted",
                    input: forge::BootstrapCaSource::Mounted as i32,
                    expect: Ok(BootstrapCaSource::Mounted),
                },
                Check {
                    scenario: "unknown",
                    input: 99,
                    expect: Err("unknown bootstrap CA source value 99".to_string()),
                },
            ],
            parse_bootstrap_ca_source,
        );
    }

    #[test]
    fn dpu_nvconfig_profile_protobuf_values_treat_unspecified_as_absent_and_reject_unknown() {
        check_values(
            [
                Check {
                    scenario: "unspecified",
                    input: forge::DpuNvConfigProfile::Unspecified as i32,
                    expect: Ok(None),
                },
                Check {
                    scenario: "GB200 B3240 version 1",
                    input: forge::DpuNvConfigProfile::Gb200B3240V1 as i32,
                    expect: Ok(Some(DpuNvConfigProfile::Gb200B3240V1)),
                },
                Check {
                    scenario: "unknown",
                    input: 99,
                    expect: Err("unknown DPU NVConfig profile value 99".to_string()),
                },
            ],
            parse_dpu_nvconfig_profile,
        );
    }

    #[test]
    fn user_data_template_applies_bootstrap_ca_policy() {
        check_values(
            [
                Check {
                    scenario: "legacy download",
                    input: BootstrapCaSource::LegacyDownload,
                    expect: (1, false, false, false, false),
                },
                Check {
                    scenario: "embedded",
                    input: BootstrapCaSource::Embedded,
                    expect: (0, true, true, false, true),
                },
                Check {
                    scenario: "mounted",
                    input: BootstrapCaSource::Mounted,
                    expect: (0, true, false, true, false),
                },
            ],
            |source| {
                let rendered = render_user_data_test_template(source, None);
                (
                    rendered.matches("ip vrf exec mgmt curl --retry 5 --retry-all-errors -v -o /opt/forge/forge_root.pem http://carbide-pxe.forge/api/v0/tls/root_ca").count(),
                    rendered.contains("validate_bootstrap_ca()"),
                    rendered.contains("install_embedded_bootstrap_ca /opt/forge/embedded_forge_root.pem /opt/forge/forge_root.pem"),
                    rendered.contains("accept_mounted_bootstrap_ca /opt/forge/forge_root.pem"),
                    rendered.contains("  /embedded_forge_root.pem"),
                )
            },
        );
    }

    #[test]
    fn user_data_template_applies_selected_dpu_nvconfig_profile_after_bfcfg() {
        let profile = DpuNvConfigProfile::Gb200B3240V1;
        let rendered =
            render_user_data_test_template(BootstrapCaSource::LegacyDownload, Some(profile));
        let command = format!(
            "/usr/bin/mlxconfig -y -d \"${{mst_device}}\" set {}",
            profile.parameters().join(" "),
        );

        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.trim() == command.as_str())
                .count(),
            1,
        );
        let bfcfg_position = rendered
            .find("/usr/bin/bfcfg")
            .expect("rendered user data should run bfcfg");
        let profile_position = rendered
            .find(command.as_str())
            .expect("rendered user data should apply the selected NVConfig profile");

        assert!(bfcfg_position < profile_position);
        assert!(rendered.contains("No mst pciconf device found for the DPU NVConfig profile"));

        let rendered_without_profile =
            render_user_data_test_template(BootstrapCaSource::LegacyDownload, None);
        assert!(!rendered_without_profile.contains(command.as_str()));
    }

    #[test]
    fn forge_agent_config() {
        let interface_id = "91609f10-c91d-470d-a260-6293ea0c1234".parse().unwrap();
        let config = generate_forge_agent_config(interface_id, None);

        let test_config = fs::read_to_string(format!("{TEST_DATA_DIR}/agent_config.toml")).unwrap();
        assert_eq!(config, test_config);

        let data: toml::Value = toml::from_str(&config).unwrap();

        assert_eq!(
            data.get("machine")
                .unwrap()
                .get("interface-id")
                .unwrap()
                .as_str()
                .unwrap(),
            interface_id.to_string().as_str(),
        );

        assert!(data.get("forge-system").is_none());

        let skipped = match data.get("machine").unwrap().get("is_fake_dpu") {
            Some(_val) => false,
            None => true,
        };
        assert!(skipped);
    }

    #[test]
    fn forge_agent_config_with_external_api_url() {
        let interface_id = "91609f10-c91d-470d-a260-6293ea0c1234".parse().unwrap();
        let config = generate_forge_agent_config(interface_id, Some("https://10.99.0.1:1079"));

        let test_config =
            fs::read_to_string(format!("{TEST_DATA_DIR}/agent_config_external.toml")).unwrap();
        assert_eq!(config, test_config);

        let data: toml::Value = toml::from_str(&config).unwrap();

        assert_eq!(
            data.get("forge-system")
                .unwrap()
                .get("api-server")
                .unwrap()
                .as_str()
                .unwrap(),
            "https://10.99.0.1:1079",
        );

        assert_eq!(
            data.get("machine")
                .unwrap()
                .get("interface-id")
                .unwrap()
                .as_str()
                .unwrap(),
            interface_id.to_string().as_str(),
        );
    }

    /// Verifies the real user-data template renders VF settings from the configured count.
    #[test]
    fn user_data_template_uses_configured_num_of_vfs() {
        let template_glob = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pxe/templates/**/*");
        let tera = tera::Tera::new(template_glob).unwrap();

        let context = HashMap::from([
            (
                "bootstrap_ca_source".to_string(),
                "legacy_download".to_string(),
            ),
            (
                "api_url".to_string(),
                "https://carbide-api.forge".to_string(),
            ),
            (
                "forge_agent_config_b64".to_string(),
                "W21hY2hpbmVdCg==".to_string(),
            ),
            ("forge_bmc_fw_update".to_string(), String::new()),
            ("forge_dpu_nvconfig_parameters".to_string(), String::new()),
            (
                "forge_hbn_reps".to_string(),
                "pf0hpf,pf0vf0,pf0vf2".to_string(),
            ),
            (
                "forge_host_representor_intercept_bridging".to_string(),
                String::new(),
            ),
            ("forge_hbn_bridge".to_string(), "br-hbn".to_string()),
            ("hostname".to_string(), "test-host".to_string()),
            (
                "interface_id".to_string(),
                "91609f10-c91d-470d-a260-6293ea0c1234".to_string(),
            ),
            ("num_of_vfs".to_string(), "3".to_string()),
            (
                "pxe_url".to_string(),
                "http://carbide-pxe.forge".to_string(),
            ),
            ("seconds_since_epoch".to_string(), "0".to_string()),
        ]);
        let rendered = tera
            .render(
                "user-data",
                &tera::Context::from_serialize(context).unwrap(),
            )
            .unwrap();

        assert!(rendered.contains("NUM_OF_VFS=3"));
        assert!(!rendered.contains("NUM_OF_VFS=16"));
        assert!(rendered.contains("BR_HBN_REPS=pf0hpf,pf0vf0,pf0vf2"));
        assert_eq!(rendered.matches("--physdev-in pf0vf").count(), 3);
        assert!(rendered.contains("--physdev-in pf0vf0_if"));
        assert!(rendered.contains("--physdev-in pf0vf1_if"));
        assert!(rendered.contains("--physdev-in pf0vf2_if"));
        assert!(!rendered.contains("--physdev-in pf0vf3_if"));
        assert!(rendered.contains("configure_ovn_encap_ip"));
        assert!(rendered.contains("ip -4 -o address show dev oob_net0 scope global"));
        assert!(rendered.contains("expected exactly one global IPv4 address on oob_net0"));
        assert!(rendered.contains(r#""external_ids:ovn-encap-ip=${oob_ipv4_addresses[0]}""#));
        assert!(
            rendered.find("service openvswitch-switch restart").unwrap()
                < rendered.find("configure_ovn_encap_ip\n").unwrap()
        );
    }

    /// Verifies the real user-data template renders each host representor bridge entry.
    #[test]
    fn user_data_template_renders_host_representor_intercept_bridging() {
        let template_glob = concat!(env!("CARGO_MANIFEST_DIR"), "/../../pxe/templates/**/*");
        let tera = tera::Tera::new(template_glob).unwrap();

        let context = HashMap::from([
            (
                "bootstrap_ca_source".to_string(),
                "legacy_download".to_string(),
            ),
            (
                "api_url".to_string(),
                "https://carbide-api.forge".to_string(),
            ),
            (
                "forge_agent_config_b64".to_string(),
                "W21hY2hpbmVdCg==".to_string(),
            ),
            ("forge_bmc_fw_update".to_string(), String::new()),
            ("forge_dpu_nvconfig_parameters".to_string(), String::new()),
            ("forge_hbn_reps".to_string(), String::new()),
            (
                "forge_host_representor_intercept_bridging".to_string(),
                "pf0hpf:br-host:patch-br-host-to-hbn,pf0vf0:br-vf0:patch-br-vf0-to-hbn".to_string(),
            ),
            ("forge_hbn_bridge".to_string(), "br-sfc".to_string()),
            ("hostname".to_string(), "test-host".to_string()),
            (
                "interface_id".to_string(),
                "91609f10-c91d-470d-a260-6293ea0c1234".to_string(),
            ),
            ("num_of_vfs".to_string(), "3".to_string()),
            (
                "pxe_url".to_string(),
                "http://carbide-pxe.forge".to_string(),
            ),
            ("seconds_since_epoch".to_string(), "0".to_string()),
        ]);
        let rendered = tera
            .render(
                "user-data",
                &tera::Context::from_serialize(context).unwrap(),
            )
            .unwrap();

        // Verify unavailable representors do not abort the remaining cloud-init work.
        assert!(rendered.contains(
            r#"set interface "${host_representor}" type=dpdk mtu_request=9216 external_ids='{}' || true"#
        ));
        assert!(rendered.contains(
            r#"ofport_request=$(ovs-vsctl get interface "${host_representor}" ofport) || true"#
        ));
        assert!(rendered.contains("ovs-vsctl get bridge br-sfc external_ids"));
        assert!(rendered.contains("ovs-vsctl --may-exist add-port br-sfc"));
        assert!(rendered.contains(
            "host_representor_intercept_bridge_config=\"pf0hpf:br-host:patch-br-host-to-hbn\""
        ));
        assert!(rendered.contains(
            "host_representor_intercept_bridge_config=\"pf0vf0:br-vf0:patch-br-vf0-to-hbn\""
        ));
        assert_eq!(
            rendered
                .matches(r#"add_host_representor_intercept_bridge "${host_representor}" "${host_intercept_bridge_name}" "${host_intercept_bridge_port}""#)
                .count(),
            2
        );
    }

    #[test]
    fn render_user_data_sets_fqdn_hostname() {
        let interface_id: MachineInterfaceId =
            "91609f10-c91d-470d-a260-6293ea0c1234".parse().unwrap();
        let machine_interface = forge::MachineInterface {
            id: Some(interface_id),
            hostname: "node-01".to_string(),
            mac_address: "aa:bb:cc:dd:ee:ff".to_string(),
            ..Default::default()
        };
        let domain = PxeDomain {
            domain: Some(forge::pxe_domain::Domain::LegacyDomain(forge::Domain {
                name: "forge.example.com".to_string(),
                ..Default::default()
            })),
        };
        let state = State(test_app_state());

        let (template_key, context) = render_user_data(
            interface_id,
            machine_interface,
            domain,
            None,
            None,
            None,
            None,
            None,
            None,
            BootstrapCaSource::LegacyDownload,
            None,
            state,
        );

        assert_eq!(template_key, "user-data");
        assert_eq!(
            context.get("bootstrap_ca_source").map(String::as_str),
            Some("legacy_download"),
        );
        assert_eq!(
            context.get("hostname").map(String::as_str),
            Some("node-01.forge.example.com"),
        );
    }

    #[test]
    fn render_user_data_sets_fqdn_hostname_with_new_domain() {
        let interface_id: MachineInterfaceId =
            "91609f10-c91d-470d-a260-6293ea0c1234".parse().unwrap();
        let machine_interface = forge::MachineInterface {
            id: Some(interface_id),
            hostname: "node-02".to_string(),
            mac_address: "aa:bb:cc:dd:ee:ff".to_string(),
            ..Default::default()
        };
        let domain = PxeDomain {
            domain: Some(forge::pxe_domain::Domain::NewDomain(rpc::dns::Domain {
                name: "new.forge.example.com".to_string(),
                ..Default::default()
            })),
        };
        let state = State(test_app_state());

        let (_template_key, context) = render_user_data(
            interface_id,
            machine_interface,
            domain,
            None,
            None,
            None,
            None,
            None,
            None,
            BootstrapCaSource::LegacyDownload,
            None,
            state,
        );

        assert_eq!(
            context.get("hostname").map(String::as_str),
            Some("node-02.new.forge.example.com"),
        );
    }

    /// A DPU that resolves to no instructions at all is an error rather than
    /// an empty document: this prefix is only reached by a machine that booted
    /// expecting a kickstart.
    #[tokio::test]
    async fn user_data_without_instructions_counts_instructions_empty() {
        let metrics = MetricsCapture::start();

        let _ = user_data(
            Machine {
                instructions: Default::default(),
            },
            State(test_app_state()),
        )
        .await;

        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[
                    ("endpoint", "cloud_init_dpu"),
                    ("reason", "instructions_empty"),
                ],
            ),
            1.0,
        );
    }
}
