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

use std::net::{IpAddr, Ipv6Addr};
use std::sync::Arc;

use carbide_instrument::{Event, Outcome, emit};
use rpc::fmds::fmds_config_service_server::FmdsConfigService;
use rpc::fmds::{UpdateConfigRequest, UpdateConfigResponse};
use tonic::{Request, Response, Status};

use crate::state::{FmdsConfig, FmdsState, IBDeviceConfig, IBInstanceConfig};

pub struct FmdsGrpcServer {
    state: Arc<FmdsState>,
}

impl FmdsGrpcServer {
    pub fn new(state: Arc<FmdsState>) -> Self {
        Self { state }
    }
}

/// An agent's config update was accepted or rejected. Both cases move the same
/// counter; each variant keeps the level and wording that result already had,
/// and holds only the field that result has.
#[derive(Event)]
#[event(
    event_name = "fmds_config_update_ingested",
    metric_name = "carbide_fmds_config_updates_total",
    component = "fmds",
    metric = counter,
    describe = "Number of FMDS gRPC config-update ingests, by outcome",
    labels(outcome: Outcome),
)]
enum ConfigUpdateIngested {
    /// Applied; the agent address is how operators find who sent it.
    #[event(labels(outcome = Ok), log = info, message = "Received config update from agent")]
    Accepted {
        #[context]
        agent_address: String,
    },

    /// Rejected, and the caller receives the same `Status`.
    #[event(labels(outcome = Error), log = warn, message = "Failed to ingest config update")]
    Rejected {
        #[context]
        error: String,
    },
}

#[derive(Debug)]
struct AppliedConfigUpdate {
    response: Response<UpdateConfigResponse>,
    agent_address: String,
}

#[tonic::async_trait]
impl FmdsConfigService for FmdsGrpcServer {
    async fn update_config(
        &self,
        request: Request<UpdateConfigRequest>,
    ) -> Result<Response<UpdateConfigResponse>, Status> {
        match self.apply_config_update(request) {
            Ok(applied) => {
                emit(ConfigUpdateIngested::Accepted {
                    agent_address: applied.agent_address,
                });
                Ok(applied.response)
            }
            Err(status) => {
                emit(ConfigUpdateIngested::Rejected {
                    error: status.to_string(),
                });
                Err(status)
            }
        }
    }
}

impl FmdsGrpcServer {
    fn apply_config_update(
        &self,
        request: Request<UpdateConfigRequest>,
    ) -> Result<AppliedConfigUpdate, Status> {
        let agent_address = request
            .remote_addr()
            .map(|addr| addr.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let update = request
            .into_inner()
            .config_update
            .ok_or_else(|| Status::invalid_argument("missing config_update"))?;

        let legacy_address = match update.address.as_str() {
            "" => None,
            address => Some(address.parse::<IpAddr>().map_err(|error| {
                Status::invalid_argument(format!("invalid `address` field `{address}`: {error}"))
            })?),
        };
        let configured_ipv6 = match update.address_ipv6.as_str() {
            "" => None,
            address => Some(address.parse::<Ipv6Addr>().map_err(|error| {
                Status::invalid_argument(format!(
                    "invalid `address_ipv6` field `{address}`: {error}"
                ))
            })?),
        };

        // Older agents selected the first physical address for `address`, so it can contain IPv6.
        // Treat that value as IPv6 during a rolling upgrade instead of serving it as `public-ipv4`.
        let (public_ipv4, legacy_ipv6) = match legacy_address {
            Some(IpAddr::V4(address)) => (Some(address), None),
            Some(IpAddr::V6(address)) => (None, Some(address)),
            None => (None, None),
        };

        let ib_devices = if update.ib_devices.is_empty() {
            None
        } else {
            Some(
                update
                    .ib_devices
                    .into_iter()
                    .map(|dev| IBDeviceConfig {
                        pf_guid: dev.pf_guid,
                        instances: dev
                            .instances
                            .into_iter()
                            .map(|inst| IBInstanceConfig {
                                ib_partition_id: inst
                                    .ib_partition_id
                                    .and_then(|id| id.parse().ok()),
                                ib_guid: inst.ib_guid,
                                lid: inst.lid,
                            })
                            .collect(),
                    })
                    .collect(),
            )
        };

        let config = FmdsConfig {
            public_ipv4,
            public_ipv6: configured_ipv6.or(legacy_ipv6),
            hostname: update.hostname,
            instance_name: update.instance_name.filter(|name| !name.is_empty()),
            sitename: update.sitename,
            instance_id: update.instance_id,
            machine_id: update.machine_id,
            user_data: update.user_data,
            ib_devices,
            asn: update.asn,
        };

        self.state.update_config(config);

        if let Some(machine_identity) = update.machine_identity {
            self.state
                .apply_machine_identity_from_proto(machine_identity)
                .map_err(Status::invalid_argument)?;
        }

        Ok(AppliedConfigUpdate {
            response: Response::new(UpdateConfigResponse {}),
            agent_address,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{CapturedFieldKind, MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::{Fails, Yields};
    use carbide_test_support::{Check, check_values, scenarios};
    use forge_dpu_fmds_shared::machine_identity::MachineIdentityParams;
    use rpc::fmds::{FmdsConfigUpdate, FmdsMachineIdentityConfig, IbDevice, IbInstance};

    use super::*;

    fn make_test_state() -> Arc<FmdsState> {
        Arc::new(FmdsState::try_new("https://api.test".to_string(), None).unwrap())
    }

    fn make_test_update() -> FmdsConfigUpdate {
        FmdsConfigUpdate {
            address: "10.0.0.1".to_string(),
            address_ipv6: "2001:db8::1".to_string(),
            hostname: "test-host".to_string(),
            instance_name: Some("test-instance".to_string()),
            sitename: Some("test-site".to_string()),
            instance_id: Some(uuid::uuid!("67e55044-10b1-426f-9247-bb680e5fe0c8").into()),
            machine_id: Some(
                "fm100ht6n80e7do39u8gmt7cvhm89pb32st9ngevgdolu542l1nfa4an0rg"
                    .parse()
                    .unwrap(),
            ),
            user_data: "cloud-init-data".to_string(),
            ib_devices: vec![],
            asn: 65000,
            machine_identity: Some(MachineIdentityParams::default().into()),
        }
    }

    #[test]
    fn test_update_config_omitted_machine_identity_preserves_serving() {
        let state = make_test_state();
        let server = FmdsGrpcServer::new(state.clone());

        let mut first = make_test_update();
        first.machine_identity = Some(FmdsMachineIdentityConfig {
            requests_per_second: 5,
            burst: 10,
            wait_timeout_secs: 3,
            sign_timeout_secs: 6,
            sign_proxy_url: None,
            sign_proxy_tls_root_ca: None,
        });

        server
            .apply_config_update(Request::new(UpdateConfigRequest {
                config_update: Some(first),
            }))
            .unwrap();

        let ptr_after_first = Arc::as_ptr(&state.machine_identity.load_full());

        let mut second = make_test_update();
        second.address = "10.0.0.2".to_string();
        second.machine_identity = None;

        server
            .apply_config_update(Request::new(UpdateConfigRequest {
                config_update: Some(second),
            }))
            .unwrap();

        assert_eq!(
            Arc::as_ptr(&state.machine_identity.load_full()),
            ptr_after_first
        );

        let config = state.config.load_full().unwrap();
        assert_eq!(config.public_ipv4, Some("10.0.0.2".parse().unwrap()));
    }

    #[test]
    fn test_update_config_stores_data() {
        let state = make_test_state();
        let server = FmdsGrpcServer::new(state.clone());

        let request = Request::new(UpdateConfigRequest {
            config_update: Some(make_test_update()),
        });

        let response = server.apply_config_update(request);
        assert!(response.is_ok());

        let config = state.config.load_full().unwrap();
        assert_eq!(config.public_ipv4, Some("10.0.0.1".parse().unwrap()));
        assert_eq!(config.public_ipv6, Some("2001:db8::1".parse().unwrap()));
        assert_eq!(config.hostname, "test-host");
        assert_eq!(config.instance_name.as_deref(), Some("test-instance"));
        assert_eq!(config.sitename.as_deref(), Some("test-site"));
        assert_eq!(config.asn, 65000);

        let mut update = make_test_update();
        update.instance_name = Some(String::new());
        server
            .apply_config_update(Request::new(UpdateConfigRequest {
                config_update: Some(update),
            }))
            .unwrap();

        let config = state.config.load_full().unwrap();
        assert!(config.instance_name.is_none());
    }

    #[test]
    fn test_update_config_classifies_legacy_and_dual_stack_addresses() {
        struct AddressFields {
            address: &'static str,
            address_ipv6: &'static str,
        }

        scenarios!(
            run = |fields: AddressFields| {
                let state = make_test_state();
                let server = FmdsGrpcServer::new(state.clone());
                let mut update = make_test_update();
                update.address = fields.address.to_string();
                update.address_ipv6 = fields.address_ipv6.to_string();
                server
                    .apply_config_update(Request::new(UpdateConfigRequest {
                        config_update: Some(update),
                    }))
                    .map_err(drop)?;
                let config = state.config.load_full().unwrap();
                Ok::<_, ()>((config.public_ipv4, config.public_ipv6))
            };
            "current dual-stack agent" {
                AddressFields {
                    address: "192.0.2.10",
                    address_ipv6: "2001:db8::10",
                } => Yields((
                    Some("192.0.2.10".parse().unwrap()),
                    Some("2001:db8::10".parse().unwrap()),
                )),
            }

            "legacy agent" {
                AddressFields {
                    address: "192.0.2.10",
                    address_ipv6: "",
                } => Yields((Some("192.0.2.10".parse().unwrap()), None)),
                AddressFields {
                    address: "2001:db8::10",
                    address_ipv6: "",
                } => Yields((None, Some("2001:db8::10".parse().unwrap()))),
            }

            "invalid address" {
                AddressFields {
                    address: "not-an-address",
                    address_ipv6: "",
                } => Fails,
                AddressFields {
                    address: "192.0.2.10",
                    address_ipv6: "not-an-address",
                } => Fails,
            }
        );
    }

    #[test]
    fn test_update_config_missing_config_update() {
        let state = make_test_state();
        let server = FmdsGrpcServer::new(state);

        let request = Request::new(UpdateConfigRequest {
            config_update: None,
        });

        let response = server.apply_config_update(request);
        assert!(response.is_err());
        assert_eq!(response.unwrap_err().code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn test_update_config_with_ib_devices() {
        let state = make_test_state();
        let server = FmdsGrpcServer::new(state.clone());

        let mut update = make_test_update();
        update.ib_devices = vec![IbDevice {
            pf_guid: "pfguid1".to_string(),
            instances: vec![
                IbInstance {
                    ib_partition_id: Some("67e55044-10b1-426f-9247-bb680e5fe0c8".to_string()),
                    ib_guid: Some("guid1".to_string()),
                    lid: 42,
                },
                IbInstance {
                    ib_partition_id: None,
                    ib_guid: Some("guid2".to_string()),
                    lid: 43,
                },
            ],
        }];

        let request = Request::new(UpdateConfigRequest {
            config_update: Some(update),
        });

        server.apply_config_update(request).unwrap();

        let config = state.config.load_full().unwrap();
        let devices = config.ib_devices.as_ref().unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].pf_guid, "pfguid1");
        assert_eq!(devices[0].instances.len(), 2);
        assert_eq!(devices[0].instances[0].ib_guid.as_deref(), Some("guid1"));
        assert_eq!(devices[0].instances[0].lid, 42);
        assert!(devices[0].instances[0].ib_partition_id.is_some());
        assert!(devices[0].instances[1].ib_partition_id.is_none());
    }

    #[test]
    fn test_update_config_empty_ib_devices_becomes_none() {
        let state = make_test_state();
        let server = FmdsGrpcServer::new(state.clone());

        let request = Request::new(UpdateConfigRequest {
            config_update: Some(make_test_update()),
        });

        server.apply_config_update(request).unwrap();

        let config = state.config.load_full().unwrap();
        assert!(config.ib_devices.is_none());
    }

    enum TerminalCase {
        Accepted,
        MissingUpdate,
    }

    impl TerminalCase {
        fn metric_label(&self) -> &'static str {
            match self {
                Self::Accepted => "ok",
                Self::MissingUpdate => "error",
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct TerminalObservation {
        status: Option<tonic::Code>,
        metric_delta: f64,
        logs: Vec<LogObservation>,
    }

    #[derive(Debug, PartialEq)]
    struct LogObservation {
        metadata_name: String,
        level: tracing::Level,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        outcome: Option<String>,
        agent_address: Option<String>,
        agent_address_kind: Option<CapturedFieldKind>,
        error: Option<String>,
        error_kind: Option<CapturedFieldKind>,
    }

    fn expected_log(
        metadata_name: &str,
        level: tracing::Level,
        message: &str,
        outcome: &str,
        agent_address: Option<&str>,
        error: Option<&str>,
    ) -> Vec<LogObservation> {
        vec![LogObservation {
            metadata_name: metadata_name.to_string(),
            level,
            message: message.to_string(),
            event_name: Some(metadata_name.to_string()),
            metric_name: Some("carbide_fmds_config_updates_total".to_string()),
            outcome: Some(outcome.to_string()),
            agent_address: agent_address.map(str::to_string),
            agent_address_kind: agent_address.map(|_| CapturedFieldKind::Debug),
            error: error.map(str::to_string),
            error_kind: error.map(|_| CapturedFieldKind::Debug),
        }]
    }

    fn observe_terminal_call(case: TerminalCase) -> TerminalObservation {
        let outcome = case.metric_label();
        let request = Request::new(UpdateConfigRequest {
            config_update: match case {
                TerminalCase::Accepted => Some(make_test_update()),
                TerminalCase::MissingUpdate => None,
            },
        });
        let server = FmdsGrpcServer::new(make_test_state());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let metrics = MetricsCapture::start();
        let mut result = None;
        let logs = capture_logs(|| {
            result = Some(runtime.block_on(server.update_config(request)));
        })
        .into_iter()
        .map(|log| {
            let event_name = log.field("event_name").map(str::to_string);
            let metric_name = log.field("metric_name").map(str::to_string);
            let outcome = log.field("outcome").map(str::to_string);
            let agent_address = log.field("agent_address").map(str::to_string);
            let agent_address_kind = log.field_kind("agent_address");
            let error = log.field("error").map(str::to_string);
            let error_kind = log.field_kind("error");
            LogObservation {
                metadata_name: log.metadata_name,
                level: log.level,
                message: log.message,
                event_name,
                metric_name,
                outcome,
                agent_address,
                agent_address_kind,
                error,
                error_kind,
            }
        })
        .collect();

        TerminalObservation {
            status: result.unwrap().err().map(|status| status.code()),
            metric_delta: metrics
                .counter_delta("carbide_fmds_config_updates_total", &[("outcome", outcome)]),
            logs,
        }
    }

    #[test]
    fn update_config_emits_one_terminal_event_per_call() {
        let missing_update = Status::invalid_argument("missing config_update").to_string();
        check_values(
            [
                Check {
                    scenario: "accepted updates keep the agent address field",
                    input: TerminalCase::Accepted,
                    expect: TerminalObservation {
                        status: None,
                        metric_delta: 1.0,
                        logs: expected_log(
                            "fmds_config_update_ingested",
                            tracing::Level::INFO,
                            "Received config update from agent",
                            "ok",
                            Some("unknown"),
                            None,
                        ),
                    },
                },
                Check {
                    scenario: "rejected updates keep the status error field",
                    input: TerminalCase::MissingUpdate,
                    expect: TerminalObservation {
                        status: Some(tonic::Code::InvalidArgument),
                        metric_delta: 1.0,
                        logs: expected_log(
                            "fmds_config_update_ingested",
                            tracing::Level::WARN,
                            "Failed to ingest config update",
                            "error",
                            None,
                            Some(&missing_update),
                        ),
                    },
                },
            ],
            observe_terminal_call,
        );
    }
}
