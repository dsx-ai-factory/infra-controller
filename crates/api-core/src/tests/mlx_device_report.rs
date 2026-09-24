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

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Body;
use carbide_uuid::machine::{HostMachineId, MachineId};
use futures::StreamExt;
use model::dpa_interface::DpaSearchConfig;
use model::machine::status::MlxDeviceObservation;
use prost::Message;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    ScoutStreamApiBoundMessage, ScoutStreamInitRequest, scout_stream_api_bound_message,
};
use rpc::protos::mlx_device::{
    MlxAdminDeviceReportRequest, MlxDeviceInfo, MlxDeviceInfoReportResponse, MlxDeviceReport,
    PublishMlxDeviceReportRequest, mlx_device_info_report_response,
};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Streaming};
use tonic_prost::ProstDecoder;

use crate::api::{Api, ScoutStreamType};
use crate::auth::AuthContext;
use crate::tests::common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_test_env, create_test_env_with_overrides,
    get_config,
};

fn machine_request<T>(message: T, machine_id: MachineId) -> Request<T> {
    let mut request = Request::new(message);
    let mut context = AuthContext::default();
    context.principals.push(
        carbide_authn::middleware::Principal::SpiffeMachineIdentifier(machine_id.to_string()),
    );
    request.extensions_mut().insert(context);
    request
}

fn report(machine_id: Option<MachineId>, seconds: i64) -> MlxDeviceReport {
    MlxDeviceReport {
        machine_id,
        timestamp: Some(prost_types::Timestamp { seconds, nanos: 0 }.into()),
        devices: vec![MlxDeviceInfo {
            pci_name: "01:00.0".into(),
            device_type: "ConnectX-8".into(),
            part_number: "unknown-part-number".into(),
            fw_version_current: seconds.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

async fn observation(
    pool: &sqlx::PgPool,
    machine_id: HostMachineId,
) -> Option<MlxDeviceObservation> {
    db::machine::find_one(pool, &machine_id, Default::default())
        .await
        .unwrap()
        .unwrap()
        .status
        .mlx_device_observation
}

#[crate::sqlx_test]
async fn startup_retains_generic_observations_without_svpc(pool: sqlx::PgPool) {
    let mut config = get_config();
    config.ewethers_config = None;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    assert!(!env.api.runtime_config.is_svpc_enabled());

    env.api
        .publish_mlx_device_report(machine_request(
            PublishMlxDeviceReportRequest {
                report: Some(report(Some(machine_id), 2)),
            },
            machine_id,
        ))
        .await
        .unwrap();
    let stored = observation(&env.pool, host_machine_id).await.unwrap();
    assert_eq!(
        stored.devices[0].part_number.as_deref(),
        Some("unknown-part-number")
    );
    assert_eq!(stored.devices[0].base_mac, None);
    assert!(
        db::dpa_interface::find_by_machine_id(
            &env.pool,
            host_machine_id,
            DpaSearchConfig::default()
        )
        .await
        .unwrap()
        .is_empty()
    );

    // An older report and a newer unbound publication must both leave the
    // authenticated snapshot unchanged, even though both RPCs succeed.
    for request in [
        machine_request(
            PublishMlxDeviceReportRequest {
                report: Some(report(Some(machine_id), 1)),
            },
            machine_id,
        ),
        Request::new(PublishMlxDeviceReportRequest {
            report: Some(report(Some(machine_id), 3)),
        }),
    ] {
        env.api.publish_mlx_device_report(request).await.unwrap();
        assert_eq!(
            observation(&env.pool, host_machine_id).await.as_ref(),
            Some(&stored)
        );
    }
}

#[crate::sqlx_test]
async fn startup_snapshot_failure_still_processes_dpa_data(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    assert!(env.api.runtime_config.is_svpc_enabled());

    // Fail only the new snapshot statement; independent DPA writes still work.
    sqlx::query(
        "ALTER TABLE machines ADD CONSTRAINT test_reject_mlx_observation
         CHECK (mlx_device_observation IS NULL)",
    )
    .execute(&env.pool)
    .await
    .unwrap();
    let mut device_report = report(Some(machine_id), 2);
    device_report.devices[0].base_mac = "00:11:22:33:44:55".into();
    device_report.devices[0].device_description = "ConnectX-8 SuperNIC".into();
    let error = env
        .api
        .publish_mlx_device_report(machine_request(
            PublishMlxDeviceReportRequest {
                report: Some(device_report),
            },
            machine_id,
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(error.message().contains("test_reject_mlx_observation"));
    assert_eq!(observation(&env.pool, host_machine_id).await, None);

    let interfaces = db::dpa_interface::find_by_machine_id(
        &env.pool,
        host_machine_id,
        DpaSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(interfaces.len(), 1);
    assert_eq!(
        interfaces[0]
            .device_info
            .as_ref()
            .unwrap()
            .fw_version_current
            .as_deref(),
        Some("2")
    );
}

// Feed the real Tonic decoder so these tests exercise Init authentication and
// the production forwarding task, not a separate observation-only entry point.
async fn scout_connection(
    api: &Api,
    machine_id: MachineId,
    authenticated_machine_id: Option<MachineId>,
) -> Result<(mpsc::Sender<ScoutStreamApiBoundMessage>, ScoutStreamType), tonic::Status> {
    let (sender, receiver) = mpsc::channel::<ScoutStreamApiBoundMessage>(4);
    sender
        .send(ScoutStreamApiBoundMessage {
            flow_uuid: None,
            payload: Some(scout_stream_api_bound_message::Payload::Init(
                ScoutStreamInitRequest {
                    machine_id: Some(machine_id),
                },
            )),
        })
        .await
        .unwrap();
    let body = Body::from_stream(ReceiverStream::new(receiver).map(|message| {
        let encoded = message.encode_to_vec();
        let mut frame = Vec::with_capacity(5 + encoded.len());
        frame.push(0);
        frame.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_be_bytes());
        frame.extend_from_slice(&encoded);
        Ok::<_, Infallible>(frame)
    }));
    let stream = Streaming::new_request(ProstDecoder::new(Default::default()), body, None, None);
    let request = match authenticated_machine_id {
        Some(source) => machine_request(stream, source),
        None => Request::new(stream),
    };
    let response = api.scout_stream(request).await?;
    Ok((sender, response.into_inner()))
}

async fn live_report(
    api: &Api,
    machine_id: MachineId,
    sender: &mpsc::Sender<ScoutStreamApiBoundMessage>,
    requests: &mut ScoutStreamType,
    report: MlxDeviceReport,
) {
    let read = api.mlx_admin_show_machine(Request::new(MlxAdminDeviceReportRequest {
        machine_id: Some(machine_id),
    }));
    let reply = async {
        let request = requests.next().await.unwrap().unwrap();
        sender
            .send(ScoutStreamApiBoundMessage {
                flow_uuid: request.flow_uuid,
                payload: Some(
                    scout_stream_api_bound_message::Payload::MlxDeviceInfoReportResponse(
                        MlxDeviceInfoReportResponse {
                            reply: Some(mlx_device_info_report_response::Reply::DeviceReport(
                                report.clone(),
                            )),
                        },
                    ),
                ),
            })
            .await
            .unwrap();
    };
    let (response, ()) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(read, reply) })
            .await
            .expect("live report must not block indefinitely");
    assert_eq!(response.unwrap().into_inner().device_report, Some(report));
}

#[crate::sqlx_test]
async fn live_reports_use_authenticated_stream_identity(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    let other_machine: MachineId = host.dpu().id.into();

    let mismatch = scout_connection(&env.api, machine_id, Some(other_machine)).await;
    assert_eq!(
        mismatch.err().unwrap().code(),
        tonic::Code::PermissionDenied
    );
    assert!(!env.api.scout_stream_registry.is_connected(machine_id).await);

    let (sender, mut requests) = scout_connection(&env.api, machine_id, Some(machine_id))
        .await
        .unwrap();
    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        report(None, 1),
    )
    .await;
    let stored = observation(&env.pool, host_machine_id).await.unwrap();
    assert_eq!(stored.devices[0].fw_version_current.as_deref(), Some("1"));

    // A malformed report is still returned to the admin caller as received,
    // but it cannot erase the previous successful observation.
    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        MlxDeviceReport {
            timestamp: None,
            ..report(None, 2)
        },
    )
    .await;
    assert_eq!(
        observation(&env.pool, host_machine_id).await.as_ref(),
        Some(&stored)
    );

    // The simulator's unbound stream can still answer reads, without gaining
    // the ability to update a machine's authenticated observations.
    let (unbound_sender, mut unbound_requests) =
        scout_connection(&env.api, machine_id, None).await.unwrap();
    live_report(
        &env.api,
        machine_id,
        &unbound_sender,
        &mut unbound_requests,
        report(None, 3),
    )
    .await;
    assert_eq!(
        observation(&env.pool, host_machine_id).await.as_ref(),
        Some(&stored)
    );
}

#[crate::sqlx_test]
async fn a_blocked_snapshot_write_does_not_block_the_live_report(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let machine_id: MachineId = host.id.into();
    let host_machine_id = HostMachineId::try_from(machine_id).unwrap();
    let (sender, mut requests) = scout_connection(&env.api, machine_id, Some(machine_id))
        .await
        .unwrap();
    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        report(None, 1),
    )
    .await;
    let stored = observation(&env.pool, host_machine_id).await.unwrap();

    let mut locking_transaction = env.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE")
        .bind(machine_id)
        .fetch_one(locking_transaction.as_mut())
        .await
        .unwrap();
    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        report(None, 2),
    )
    .await;
    assert_eq!(
        observation(&env.pool, host_machine_id).await.as_ref(),
        Some(&stored)
    );
    locking_transaction.rollback().await.unwrap();

    live_report(
        &env.api,
        machine_id,
        &sender,
        &mut requests,
        report(None, 3),
    )
    .await;
    assert_eq!(
        observation(&env.pool, host_machine_id)
            .await
            .unwrap()
            .devices[0]
            .fw_version_current
            .as_deref(),
        Some("3")
    );
}
