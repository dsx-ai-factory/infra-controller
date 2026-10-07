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

//! The NoCloud datasource served to an assigned instance.
//!
//! A tenant's machine reaches this from `ds=nocloud;s=` on its kernel
//! command line and fetches the full NoCloud set, so this prefix carries all
//! four documents.

use std::borrow::Cow;
use std::collections::HashMap;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use carbide_instrument::emit;

use super::log_and_generate_generic_error;
use crate::common::{AppState, Machine};
use crate::metrics::{BootEndpoint, CloudInitConsumer, OutcomeReason, PxeBootOutcome};

/// Every outcome on these routes carries the same endpoint label.
const ENDPOINT: BootEndpoint = BootEndpoint::CloudInit(CloudInitConsumer::Tenant);

/// Serves the tenant's own cloud-init document.
///
/// An instance with none configured gets an empty document rather than an
/// error: having no user-data is an ordinary way to run an instance, and
/// cloud-init still needs something to fetch.
async fn user_data(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let mut template_data: HashMap<String, String> = HashMap::new();
    template_data.insert(
        "user_data".to_string(),
        machine
            .instructions
            .custom_cloud_init
            .unwrap_or_else(|| "{}".to_string()),
    );
    emit(PxeBootOutcome {
        endpoint: ENDPOINT,
        reason: OutcomeReason::Ok,
    });

    axum_template::Render(
        "user-data-assigned".to_string(),
        state.engine.clone(),
        template_data,
    )
}

async fn meta_data(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let (template_key, template_data) = match machine.instructions.metadata {
        None => log_and_generate_generic_error(
            format!("No metadata was found for machine {machine:?}"),
            OutcomeReason::MetadataNotFound,
            ENDPOINT,
        ),
        Some(metadata) => {
            let mut template_data = HashMap::from([
                ("instance_id".to_string(), metadata.instance_id),
                ("cloud_name".to_string(), metadata.cloud_name),
                ("platform".to_string(), metadata.platform),
            ]);
            if let Some(local_hostname) = metadata.local_hostname {
                template_data.insert("local_hostname".to_string(), local_hostname);
            }

            emit(PxeBootOutcome {
                endpoint: ENDPOINT,
                reason: OutcomeReason::Ok,
            });
            ("meta-data".to_string(), template_data)
        }
    };

    axum_template::Render(template_key, state.engine.clone(), template_data)
}

/// Extracts the top-level `network:` key (if present) from a tenant's
/// custom cloud-init document and returns it as its own standalone YAML
/// document, suitable for seeding NoCloud's separate `network-config`
/// file. A `network:` key inside `user-data` itself is not a recognized
/// user-data format and is silently ignored by cloud-init.
fn extract_network_config(custom_cloud_init: &str) -> Option<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(custom_cloud_init).ok()?;
    serde_yaml::to_string(value.get("network")?).ok()
}

/// Default network-config served when a tenant hasn't provided a custom
/// `network:` key in their cloud-init userdata. cloud-init's own default
/// behavior (no network-config at all) only DHCPs the first network
/// interface it finds; this instead DHCPs every matching interface, under
/// both the predictable ("en*") and legacy ("eth*") naming conventions,
/// so multi-NIC hosts come up with working networking on every port.
const DEFAULT_NETWORK_CONFIG: &str = r#"version: 2
ethernets:
  predictable-names:
    match:
      name: "en*"
    dhcp4: true
    dhcp6: true
  legacy-names:
    match:
      name: "eth*"
    dhcp4: true
    dhcp6: true
"#;

/// Resolves the network-config YAML to use for a machine: the `network:`
/// key extracted from the tenant's custom cloud-init userdata if present,
/// otherwise DEFAULT_NETWORK_CONFIG (DHCP on every interface), rather
/// than an empty document that would fall back to cloud-init's own
/// first-interface-only default.
fn resolve_network_config(custom_cloud_init: Option<&str>) -> Cow<'static, str> {
    custom_cloud_init
        .and_then(extract_network_config)
        .map(Cow::Owned)
        .unwrap_or(Cow::Borrowed(DEFAULT_NETWORK_CONFIG))
}

/// Serves NoCloud's `network-config` document for a tenant's assigned
/// machine, extracted from any `network:` key present in their custom
/// cloud-init userdata. When no such key is present, serves
/// DEFAULT_NETWORK_CONFIG instead of an empty document, so hosts get
/// DHCP on every interface by default rather than cloud-init's own
/// first-interface-only behavior.
async fn network_config(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let network_config_yaml =
        resolve_network_config(machine.instructions.custom_cloud_init.as_deref());
    let template_data = HashMap::from([("network_config", network_config_yaml)]);
    axum_template::Render("network-config", state.engine.clone(), template_data)
}

async fn vendor_data(state: State<AppState>) -> impl IntoResponse {
    emit(PxeBootOutcome {
        endpoint: ENDPOINT,
        reason: OutcomeReason::Ok,
    });
    axum_template::Render(
        "printcontext",
        state.engine.clone(),
        HashMap::<String, String>::new(),
    )
}

/// Builds the tenant's route table under `path_prefix`: the four documents
/// NoCloud fetches.
pub(crate) fn get_router(path_prefix: &str) -> Router<AppState> {
    Router::new()
        .route(
            format!("{}/{}", path_prefix, "user-data").as_str(),
            get(user_data),
        )
        .route(
            format!("{}/{}", path_prefix, "meta-data").as_str(),
            get(meta_data),
        )
        .route(
            format!("{}/{}", path_prefix, "vendor-data").as_str(),
            get(vendor_data),
        )
        .route(
            format!("{}/{}", path_prefix, "network-config").as_str(),
            get(network_config),
        )
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::MetricsCapture;
    use rpc::forge;

    use super::*;
    use crate::common::{test_app_state, test_app_state_with_templates};

    /// Table-driven coverage for `extract_network_config` across its three
    /// input variants: a present `network:` key, a missing one, and
    /// malformed YAML.
    #[test]
    fn extract_network_config_handles_various_inputs() {
        struct Case {
            name: &'static str,
            input: &'static str,
            expect_some: bool,
        }

        let cases = [
            Case {
                name: "network key present",
                input: "#cloud-config\nnetwork:\n  version: 2\n  ethernets:\n    eth0:\n      addresses:\n        - 10.10.10.50/24\nwrite_files:\n  - path: /tmp/foo\n    content: bar\n",
                expect_some: true,
            },
            Case {
                name: "no network key",
                input: "#cloud-config\nwrite_files:\n  - path: /tmp/foo\n    content: bar\n",
                expect_some: false,
            },
            Case {
                name: "invalid yaml",
                input: "not: valid: yaml: at: all: :::",
                expect_some: false,
            },
        ];

        for case in cases {
            let result = extract_network_config(case.input);
            assert_eq!(
                result.is_some(),
                case.expect_some,
                "case '{}' failed",
                case.name
            );

            if case.expect_some {
                let parsed: serde_yaml::Value = serde_yaml::from_str(&result.unwrap()).unwrap();
                assert_eq!(parsed.get("version").unwrap().as_u64().unwrap(), 2);
                assert!(
                    parsed
                        .get("ethernets")
                        .and_then(|e| e.get("eth0"))
                        .is_some(),
                    "case '{}': expected eth0 config present",
                    case.name
                );
            }
        }
    }

    #[test]
    fn resolve_network_config_handles_various_inputs() {
        struct Case {
            name: &'static str,
            custom_cloud_init: Option<&'static str>,
            expect_default: bool,
        }

        let cases = [
            Case {
                name: "no network key in custom cloud-init",
                custom_cloud_init: Some("#cloud-config\nwrite_files: []\n"),
                expect_default: true,
            },
            Case {
                name: "network key present in custom cloud-init",
                custom_cloud_init: Some(
                    "#cloud-config\nnetwork:\n  version: 2\n  ethernets:\n    eth0:\n      addresses:\n        - 10.10.10.50/24\n",
                ),
                expect_default: false,
            },
            Case {
                name: "no custom cloud-init at all",
                custom_cloud_init: None,
                expect_default: true,
            },
        ];

        for case in cases {
            let result = resolve_network_config(case.custom_cloud_init);

            if case.expect_default {
                assert_eq!(
                    result, DEFAULT_NETWORK_CONFIG,
                    "case '{}' failed",
                    case.name
                );
            } else {
                let parsed: serde_yaml::Value = serde_yaml::from_str(&result).unwrap_or_else(|e| {
                    panic!("case '{}': result was not valid YAML: {}", case.name, e)
                });
                assert_eq!(
                    parsed.get("version").unwrap().as_u64().unwrap(),
                    2,
                    "case '{}' failed",
                    case.name
                );
                let eth0_addresses = parsed
                    .get("ethernets")
                    .and_then(|e| e.get("eth0"))
                    .and_then(|e| e.get("addresses"))
                    .and_then(|a| a.as_sequence())
                    .unwrap_or_else(|| {
                        panic!("case '{}': expected ethernets.eth0.addresses", case.name)
                    });
                assert_eq!(
                    eth0_addresses[0].as_str().unwrap(),
                    "10.10.10.50/24",
                    "case '{}' failed",
                    case.name
                );
            }
        }
    }

    /// When an instance has a name, meta-data includes local-hostname so
    /// cloud-init sets the OS hostname via the NoCloud datasource.
    #[tokio::test]
    async fn meta_data_includes_local_hostname_when_instance_has_name() {
        // Serializes against the tests that assert exact counter deltas: driving
        // a handler emits into the shared registry, and MetricsCapture::start is
        // what takes the lock that keeps those deltas meaningful.
        let _metrics = MetricsCapture::start();

        let response = meta_data(
            Machine {
                instructions: forge::CloudInitInstructions {
                    metadata: Some(forge::CloudInitMetaData {
                        instance_id: "test-instance-id".to_string(),
                        cloud_name: "nvidia".to_string(),
                        platform: "forge".to_string(),
                        local_hostname: Some("my-node".to_string()),
                    }),
                    ..Default::default()
                },
            },
            State(test_app_state_with_templates()),
        )
        .await
        .into_response();

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(
            text.contains("local-hostname: \"my-node\""),
            "meta-data should contain local-hostname, got: {text}"
        );
    }

    /// When an instance has no name, meta-data must not include local-hostname
    /// so cloud-init falls back to its default hostname derivation.
    #[tokio::test]
    async fn meta_data_omits_local_hostname_when_instance_has_no_name() {
        // Serializes against the tests that assert exact counter deltas: driving
        // a handler emits into the shared registry, and MetricsCapture::start is
        // what takes the lock that keeps those deltas meaningful.
        let _metrics = MetricsCapture::start();

        let response = meta_data(
            Machine {
                instructions: forge::CloudInitInstructions {
                    metadata: Some(forge::CloudInitMetaData {
                        instance_id: "test-instance-id".to_string(),
                        cloud_name: "nvidia".to_string(),
                        platform: "forge".to_string(),
                        local_hostname: None,
                    }),
                    ..Default::default()
                },
            },
            State(test_app_state_with_templates()),
        )
        .await
        .into_response();

        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(
            !text.contains("local-hostname"),
            "meta-data must not contain local-hostname when name is empty, got: {text}"
        );
    }

    /// A meta-data request with no metadata lands in the generic-error
    /// funnel, which serves the error template and moves the outcome
    /// counter.
    #[tokio::test]
    async fn meta_data_without_metadata_counts_metadata_not_found() {
        let metrics = MetricsCapture::start();

        let _ = meta_data(
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
                    ("endpoint", "cloud_init_tenant"),
                    ("reason", "metadata_not_found")
                ],
            ),
            1.0,
        );
    }

    /// A user-data request answered from the tenant's custom cloud-init
    /// counts as a served outcome.
    #[tokio::test]
    async fn user_data_with_custom_cloud_init_counts_ok() {
        let metrics = MetricsCapture::start();

        let _ = user_data(
            Machine {
                instructions: forge::CloudInitInstructions {
                    custom_cloud_init: Some("#cloud-config".to_string()),
                    ..Default::default()
                },
            },
            State(test_app_state()),
        )
        .await;

        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[("endpoint", "cloud_init_tenant"), ("reason", "ok")],
            ),
            1.0,
        );
    }
}
