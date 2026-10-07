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
use std::fmt::Display;

use axum::Router;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use carbide_instrument::emit;
use forge_tls::client_config::ClientCert;
use rpc::forge_tls_client::ForgeClientConfig;

use crate::common::{AppState, Machine, MachineInterface};
use crate::metrics::{BootEndpoint, OutcomeReason, PxeBootOutcome, PxeCustomIpxeFetchFailed};
use crate::routes::RpcContext;

enum PxeErrorCode {
    ArchitectureNotFound = 105,
    InterfaceNotFound = 106,
    InstructionsEmpty = 107,
}

fn generate_error_template<D1, D2>(error_str: D1, error_code: D2) -> HashMap<String, String>
where
    D1: Display,
    D2: Display,
{
    HashMap::from([(
        "error".to_string(),
        format!(
            r#"
echo {error_str} ||
exit {error_code} ||
"#,
        ),
    )])
}

async fn whoami(machine: Machine, state: State<AppState>) -> impl IntoResponse {
    let (template_key, template_data) =
        if let Some(instructions) = machine.instructions.discovery_instructions {
            match (instructions.machine_interface, instructions.domain) {
                (Some(interface), Some(_)) => {
                    let mut template_data = HashMap::new();
                    template_data.insert("fqdn".to_string(), interface.hostname);
                    template_data.insert("mac_address".to_string(), interface.mac_address);

                    emit(PxeBootOutcome {
                        endpoint: BootEndpoint::Whoami,
                        reason: OutcomeReason::Ok,
                    });
                    ("whoami".to_string(), template_data)
                }
                _ => {
                    emit(PxeBootOutcome {
                        endpoint: BootEndpoint::Whoami,
                        reason: OutcomeReason::InterfaceNotFound,
                    });
                    (
                        "error".to_string(),
                        generate_error_template(
                            "Could not load interface or domain",
                            PxeErrorCode::InterfaceNotFound as isize,
                        ),
                    )
                }
            }
        } else {
            emit(PxeBootOutcome {
                endpoint: BootEndpoint::Whoami,
                reason: OutcomeReason::InstructionsEmpty,
            });
            (
                "error".to_string(),
                generate_error_template(
                    "Could not load instructions",
                    PxeErrorCode::InstructionsEmpty as isize,
                ),
            )
        };

    axum_template::Render(template_key, state.engine.clone(), template_data)
}

async fn boot(contents: MachineInterface, state: State<AppState>) -> impl IntoResponse {
    let (template_key, template_data) = match contents.architecture {
        Some(arch) => {
            // The wrapping `pxe` Tera template (pxe/templates/pxe) consumes:
            //   - {{ pxe_url }}        -- carbide-pxe URL for cloud-init.
            //   - {{ static_pxe_url }} -- carbide-pxe URL for /public/blobs.
            //   - {{ ipxe }}           -- the OS-specific iPXE script body
            //                             returned by carbide-api.
            let mut template_data = HashMap::new();
            template_data.insert("pxe_url".to_string(), state.runtime_config.pxe_url.clone());

            if !state.runtime_config.static_pxe_url.is_empty() {
                template_data.insert(
                    "static_pxe_url".to_string(),
                    state.runtime_config.static_pxe_url.clone(),
                );
            }

            let pxe_response = RpcContext::get_pxe_instructions(
                arch.into(),
                contents.client_ip,
                contents.product,
                &state.runtime_config.internal_api_url,
                &ForgeClientConfig::new(
                    state.runtime_config.forge_root_ca_path.clone(),
                    Some(ClientCert {
                        cert_path: state.runtime_config.server_cert_path.clone(),
                        key_path: state.runtime_config.server_key_path.clone(),
                    }),
                ),
            )
            .await;

            // Use URL overrides from the API if present (for external
            // clients), falling back to global config.
            let (api_url, pxe_url, static_pxe_url) = match &pxe_response {
                Ok(resp) => (
                    resp.api_url_override
                        .clone()
                        .unwrap_or_else(|| state.runtime_config.client_facing_api_url.clone()),
                    resp.pxe_url_override
                        .clone()
                        .unwrap_or_else(|| state.runtime_config.pxe_url.clone()),
                    resp.static_pxe_url_override
                        .clone()
                        .unwrap_or_else(|| state.runtime_config.static_pxe_url.clone()),
                ),
                Err(_) => (
                    state.runtime_config.client_facing_api_url.clone(),
                    state.runtime_config.pxe_url.clone(),
                    state.runtime_config.static_pxe_url.clone(),
                ),
            };

            // Both branches return a script over HTTP 200, so the HTTP
            // metrics cannot tell them apart. Emit at the branch that chooses
            // the real or fallback script; that keeps each request to one
            // `carbide_pxe_boot_outcomes_total` increment.
            let instructions = match pxe_response {
                Ok(resp) => {
                    emit(PxeBootOutcome {
                        endpoint: BootEndpoint::Boot,
                        reason: OutcomeReason::Ok,
                    });
                    resp.pxe_script
                }
                Err(err) => {
                    let instructions = format!(
                        r#"
echo Failed to fetch custom_ipxe: {err} ||
exit 101 ||
"#
                    );
                    emit(PxeCustomIpxeFetchFailed {
                        endpoint: BootEndpoint::Boot,
                        reason: OutcomeReason::UpstreamApiError,
                        error: err,
                    });
                    instructions
                }
            }
            .replace("[api_url]", &api_url)
            .replace("[pxe_url]", &pxe_url);

            // Override template URLs for external clients.
            template_data.insert("pxe_url".to_string(), pxe_url);
            template_data.insert("static_pxe_url".to_string(), static_pxe_url);
            template_data.insert("ipxe".to_string(), instructions);

            ("pxe", template_data)
        }
        None => {
            // Unreachable in production: the MachineInterface extractor
            // rejects a missing/invalid architecture with a 4xx (and counts
            // it) before this handler runs. Served defensively, uncounted.
            (
                "error",
                generate_error_template(
                    "Architecture not found".to_string(),
                    PxeErrorCode::ArchitectureNotFound as isize,
                ),
            )
        }
    };

    axum_template::Render(template_key, state.engine.clone(), template_data)
}

pub(crate) fn get_router(path_prefix: &str) -> Router<AppState> {
    Router::new()
        .route(
            format!("{}/{}", path_prefix, "whoami").as_str(),
            get(whoami),
        )
        .route(format!("{}/{}", path_prefix, "boot").as_str(), get(boot))
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::MetricsCapture;

    use super::*;
    use crate::common::test_app_state;

    /// A whoami request with no discovery instructions serves the error
    /// template and moves the outcome counter -- previously this failure
    /// left no trace anywhere.
    #[tokio::test]
    async fn whoami_without_instructions_counts_instructions_empty() {
        let metrics = MetricsCapture::start();

        let _ = whoami(
            Machine {
                instructions: Default::default(),
            },
            State(test_app_state()),
        )
        .await;

        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[("endpoint", "whoami"), ("reason", "instructions_empty")],
            ),
            1.0,
        );
    }

    /// A boot request with an invalid `buildarch` never reaches the handler:
    /// the extractor rejects it with a 4xx and counts the outcome there.
    #[tokio::test]
    async fn invalid_buildarch_is_rejected_and_counted_at_the_extractor() {
        use axum::extract::FromRequestParts;

        let metrics = MetricsCapture::start();

        let request = axum::http::Request::builder()
            .uri("/boot?buildarch=not-an-architecture")
            .body(())
            .expect("request");
        let (mut parts, _) = request.into_parts();
        let rejection = MachineInterface::from_request_parts(&mut parts, &test_app_state()).await;

        assert!(
            rejection.is_err(),
            "a bad buildarch never reaches the handler"
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_pxe_boot_outcomes_total",
                &[("endpoint", "boot"), ("reason", "architecture_not_found")],
            ),
            1.0,
        );
    }
}
