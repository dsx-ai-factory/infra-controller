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
use carbide_uuid::machine::{AsMachineId, MachineId, MachineIdSource, MachineType};
use model::test_support::machine_snapshot::{
    dpu_hardware_info, dpu_machine_id, managed_host_state_snapshot,
};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, Sampler, SdkTracerProvider, SpanData};
use rpc::forge::forge_server::Forge;
use tonic::Request;
use tracing::Instrument;
use tracing::instrument::WithSubscriber;
use tracing_subscriber::prelude::*;

fn capture() -> (SdkTracerProvider, InMemorySpanExporter, tracing::Dispatch) {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_sampler(Sampler::AlwaysOn)
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("core-machine-test")));
    (provider, exporter, tracing::Dispatch::new(subscriber))
}

fn attribute(span: &SpanData, key: &str) -> Option<String> {
    span.attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| attribute.value.to_string())
}

#[test]
fn request_identity_distinguishes_predicted_hosts_and_dpus() {
    let (_provider, exporter, dispatch) = capture();
    carbide_test_support::check_values(
        [
            carbide_test_support::Check {
                scenario: "established host",
                input: MachineType::Host,
                expect: (true, false, true),
            },
            carbide_test_support::Check {
                scenario: "predicted host",
                input: MachineType::PredictedHost,
                expect: (false, true, true),
            },
            carbide_test_support::Check {
                scenario: "DPU",
                input: MachineType::Dpu,
                expect: (true, false, false),
            },
        ],
        |machine_type| {
            let id = MachineId::new(MachineIdSource::Tpm, [0x11; 32], machine_type);
            tracing::dispatcher::with_default(&dispatch, || {
                let span = tracing::info_span!("request", forge.machine_id = tracing::field::Empty);
                let _entered = span.enter();
                crate::api::log_machine_id(&id);
            });
            let spans = exporter.get_finished_spans().unwrap();
            let span = spans.last().unwrap();
            assert_eq!(attribute(span, "forge.machine_id"), Some(id.to_string()));
            for key in [
                "nico.machine.id",
                "nico.machine.predicted_id",
                "nico.managed_host.id",
            ] {
                if let Some(value) = attribute(span, key) {
                    assert_eq!(value, id.to_string());
                }
            }
            (
                attribute(span, "nico.machine.id").is_some(),
                attribute(span, "nico.machine.predicted_id").is_some(),
                attribute(span, "nico.managed_host.id").is_some(),
            )
        },
    );
}

#[test]
fn dpu_snapshot_association_does_not_replace_the_direct_target() {
    let (_provider, exporter, dispatch) = capture();
    let snapshot = managed_host_state_snapshot();
    let dpu = &snapshot.dpu_snapshots[0];
    tracing::dispatcher::with_default(&dispatch, || {
        let span = tracing::info_span!("request", forge.machine_id = tracing::field::Empty);
        let _entered = span.enter();
        crate::api::log_machine_id(&dpu.id.into());
        crate::api::log_managed_host_snapshot(&snapshot, &dpu.id.into());
    });
    let spans = exporter.get_finished_spans().unwrap();
    let span = &spans[0];
    assert_eq!(attribute(span, "nico.machine.id"), Some(dpu.id.to_string()));
    assert_eq!(
        attribute(span, "nico.machine.bmc.ip"),
        dpu.status
            .bmc_info
            .ip
            .map(|ip| ip.to_canonical().to_string())
    );
    assert_eq!(
        attribute(span, "nico.managed_host.id"),
        Some(snapshot.host_snapshot.id.to_string())
    );
    assert_eq!(
        attribute(span, "nico.managed_host.bmc.ip"),
        snapshot
            .host_snapshot
            .status
            .bmc_info
            .ip
            .map(|ip| ip.to_canonical().to_string())
    );
}

#[crate::sqlx_test]
async fn machine_rpc_errors_keep_resolved_identity(pool: sqlx::PgPool) {
    let env = super::create_test_env(pool).await;
    let (_provider, exporter, dispatch) = capture();
    let id = dpu_machine_id(0);
    let hardware = dpu_hardware_info(0);
    let discovery_id = model::machine::machine_id::from_hardware_info(&hardware).unwrap();
    async {
        let span = tracing::info_span!("network_config", forge.machine_id = tracing::field::Empty);
        assert!(
            env.api
                .get_managed_host_network_config(Request::new(
                    rpc::forge::ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(id),
                    }
                ))
                .instrument(span)
                .await
                .is_err()
        );
        let span = tracing::info_span!("network_status", forge.machine_id = tracing::field::Empty);
        assert!(
            env.api
                .record_dpu_network_status(Request::new(rpc::forge::DpuNetworkStatus {
                    dpu_machine_id: Some(id),
                    ..Default::default()
                }))
                .instrument(span)
                .await
                .is_err()
        );
        let span = tracing::info_span!("discovery", forge.machine_id = tracing::field::Empty);
        assert!(
            env.api
                .discover_machine(Request::new(rpc::forge::MachineDiscoveryInfo {
                    discovery_data: Some(rpc::DiscoveryData::Info(
                        rpc::DiscoveryInfo::try_from(hardware).unwrap()
                    )),
                    ..Default::default()
                }))
                .instrument(span)
                .await
                .is_err()
        );
        let span = tracing::info_span!("missing_identity");
        assert!(
            env.api
                .get_managed_host_network_config(Request::new(Default::default()))
                .instrument(span)
                .await
                .is_err()
        );
        // A known target must survive failure to acquire a database connection,
        // not only a later lookup of a nonexistent machine.
        env.pool.close().await;
        let span = tracing::info_span!(
            "database_unavailable",
            forge.machine_id = tracing::field::Empty
        );
        assert!(
            env.api
                .get_next_remediation_for_machine(Request::new(
                    rpc::forge::GetNextRemediationForMachineRequest {
                        dpu_machine_id: Some(id)
                    }
                ))
                .instrument(span)
                .await
                .is_err()
        );
    }
    .with_subscriber(dispatch)
    .await;
    let spans = exporter.get_finished_spans().unwrap();
    for (name, id) in [
        ("network_config", id.to_machine_id()),
        ("network_status", id.to_machine_id()),
        ("discovery", discovery_id),
        ("database_unavailable", id.to_machine_id()),
    ] {
        let span = spans.iter().find(|span| span.name == name).unwrap();
        assert_eq!(attribute(span, "nico.machine.id"), Some(id.to_string()));
        assert_eq!(attribute(span, "forge.machine_id"), Some(id.to_string()));
    }
    let missing = spans
        .iter()
        .find(|span| span.name == "missing_identity")
        .unwrap();
    assert!(attribute(missing, "nico.machine.id").is_none());
}

#[crate::sqlx_test]
async fn provisioning_and_network_config_operations_record_inventory_identity(pool: sqlx::PgPool) {
    let env = super::create_test_env(pool).await;
    let (_provider, exporter, dispatch) = capture();
    let host = super::common::api_fixtures::create_managed_host(&env)
        .with_subscriber(dispatch.clone())
        .await;
    let id = host.dpu_ids[0];
    let result = async {
        env.api
            .get_managed_host_network_config(Request::new(
                rpc::forge::ManagedHostNetworkConfigRequest {
                    dpu_machine_id: Some(id),
                },
            ))
            .instrument(tracing::info_span!("network_config"))
            .await
    }
    .with_subscriber(dispatch.clone())
    .await;
    result.unwrap();

    let mut snapshot =
        db::managed_host::load_snapshot(&mut env.db_reader(), &host.id, Default::default())
            .await
            .unwrap()
            .unwrap();
    let state = snapshot.managed_state.clone();
    let expected_bmc_ip = snapshot
        .host_snapshot
        .status
        .bmc_info
        .ip
        .unwrap()
        .to_canonical()
        .to_string();
    let mut services = env.machine_state_handler_services();
    let mut metrics = carbide_machine_controller::metrics::MachineMetrics::default();
    let mut pending_db_writes = state_controller::db_write_batch::DbWriteBatch::new();
    let mut ctx = state_controller::state_handler::StateHandlerContext {
        services: &mut services,
        metrics: &mut metrics,
        pending_db_writes: &mut pending_db_writes,
    };
    use state_controller::state_handler::StateHandler;
    async {
        env.machine_state_handler
            .handle_object_state(&host.id.into(), &mut snapshot, &state, &mut ctx)
            .await
            .unwrap();
    }
    .with_subscriber(dispatch)
    .await;
    let spans = exporter.get_finished_spans().unwrap();
    let span = spans
        .iter()
        .find(|span| span.name == "network_config")
        .unwrap();
    assert_eq!(attribute(span, "nico.machine.id"), Some(id.to_string()));
    assert_eq!(
        attribute(span, "nico.managed_host.id"),
        Some(host.id.to_string())
    );
    assert_eq!(
        attribute(span, "nico.managed_host.bmc.ip"),
        Some(expected_bmc_ip.clone())
    );
    let ingestion = spans
        .iter()
        .find(|span| {
            span.name == "ingest_managed_host"
                && attribute(span, "nico.machine.bmc.ip") == Some(expected_bmc_ip.clone())
        })
        .unwrap();
    assert!(attribute(ingestion, "nico.machine.predicted_id").is_some());
    let reconcile = spans
        .iter()
        .find(|span| {
            span.name == "handle_object_state"
                && attribute(span, "nico.machine.id") == Some(host.id.to_string())
        })
        .unwrap();
    assert_eq!(
        attribute(reconcile, "nico.machine.bmc.ip"),
        Some(expected_bmc_ip)
    );
}
