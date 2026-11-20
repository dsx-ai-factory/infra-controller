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

use std::str::FromStr;
use std::time::Duration;

use ::rpc::forge::AdminForceDeleteMachineRequest;
use carbide_uuid::machine::MachineId;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn force_delete(mut query: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    const RETRY_TIME: Duration = Duration::from_secs(5);
    const MAX_WAIT_TIME: Duration = Duration::from_secs(60 * 20);

    let start = std::time::Instant::now();
    let mut dpu_machine_id = String::new();

    if let Ok(id) = MachineId::from_str(&query.machine)
        && api_client
            .0
            .find_instance_by_machine_id(id)
            .await
            .is_ok_and(|i| !i.instances.is_empty())
        && !query.allow_delete_with_instance
    {
        return Err(CarbideCliError::GenericError(
                "Machine has an attached Instance. Re-run with --allow-delete-with-instance to acknowledge that its control-plane record will be removed without first requesting a graceful workload shutdown and that force-delete cleanup may forcibly restart the host".to_string(),
            ));
    }

    loop {
        let req: AdminForceDeleteMachineRequest = (&query).into();
        let response = api_client.0.admin_force_delete_machine(req).await?;
        println!(
            "Force delete response: {}",
            serde_json::to_string_pretty(&response)?
        );

        // Poll the resolved host ID so address or hostname reassignment cannot select another host.
        if !response.all_done && !response.managed_host_machine_id.is_empty() {
            query.machine.clone_from(&response.managed_host_machine_id);
        }

        if dpu_machine_id.is_empty() && !response.dpu_machine_id.is_empty() {
            dpu_machine_id = response.dpu_machine_id;
        }

        if response.all_done {
            println!("Force delete for {} succeeded", query.machine);

            // If we only searched for a Machine, then the DPU might be left behind
            // since the site controller can't look up the DPU by host machine ID anymore.
            // To also clean up the DPU, we modify our query and continue to delete
            if !dpu_machine_id.is_empty() && query.machine != dpu_machine_id {
                println!("Starting to delete potentially stale DPU machine {dpu_machine_id}");
                query.machine = dpu_machine_id.clone();
            } else {
                // No DPU to delete
                break;
            }
        }

        if start.elapsed() > MAX_WAIT_TIME {
            return Err(CarbideCliError::GenericError(format!(
                "Unable to force delete machine after {}s. Exiting",
                MAX_WAIT_TIME.as_secs()
            )));
        }

        println!(
            "Machine has not been fully deleted. Retrying after {}s",
            RETRY_TIME.as_secs()
        );
        tokio::time::sleep(RETRY_TIME).await;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};

    use ::rpc::carbide_api_client::CarbideApiClient;
    use ::rpc::carbide_tls_client::{ApiConfig, CarbideClientConfig};
    use ::rpc::forge::{AdminForceDeleteMachineResponse, BuildInfo};
    use clap::Parser;
    use http_body_util::BodyExt;
    use hyper::Request;
    use hyper::body::Incoming;
    use hyper::server::conn::http2;
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use prost::Message as _;
    use tokio::net::TcpListener;

    use super::*;
    use crate::test_support::grpc_response;

    /// Verifies a pending IP query is followed by Core's host ID and then the cached
    /// DPU ID with unchanged request flags, keeping address reassignment from
    /// redirecting deletion or discarding the operator's consent.
    #[tokio::test]
    async fn pending_force_delete_polls_resolved_machine_ids() {
        const SELECTOR: &str = "192.0.2.10";
        const HOST_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
        const DPU_ID: &str = "fm100dt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

        // Use an IP selector with explicit Instance/orphan consent and DPU waiting.
        let query = Args::try_parse_from([
            "force-delete",
            "--machine",
            SELECTOR,
            "--allow-delete-with-instance",
            "--allow-delete-with-orphaned-dpf-crds",
            "--wait-for-instance-dpu",
        ])
        .expect("force-delete arguments parse");
        let initial_request = AdminForceDeleteMachineRequest::from(&query);

        // Prepare a mock Core listener and a record of its decoded requests.
        let captured_requests = Arc::new(Mutex::new(Vec::new()));
        let requests = captured_requests.clone();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock Core listener binds");
        let address = listener.local_addr().expect("mock listener has an address");
        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.expect("mock accepts the client");
            http2::Builder::new(TokioExecutor::new())
                .serve_connection(
                    TokioIo::new(connection),
                    service_fn(move |request: Request<Incoming>| {
                        let requests = requests.clone();
                        async move {
                            // Preserve the client's version handshake before deletion starts.
                            if request.uri().path() == ::rpc::service_path!("Version") {
                                return Ok::<_, Infallible>(grpc_response(BuildInfo::default()));
                            }
                            assert_eq!(
                                request.uri().path(),
                                ::rpc::service_path!("AdminForceDeleteMachine")
                            );
                            // Capture the actual target and consent before replying to this poll.
                            let body = request
                                .into_body()
                                .collect()
                                .await
                                .expect("force-delete request body is readable")
                                .to_bytes();
                            let request = AdminForceDeleteMachineRequest::decode(
                                body.get(5..)
                                    .expect("force-delete request has a gRPC frame"),
                            )
                            .expect("force-delete request decodes");
                            let mut requests =
                                requests.lock().expect("captured requests are available");
                            requests.push(request);
                            let response = match requests.len() {
                                // Return both IDs while requiring another host deletion poll.
                                1 => AdminForceDeleteMachineResponse {
                                    all_done: false,                              // Keep host deletion pending.
                                    managed_host_machine_id: HOST_ID.to_string(), // Return the host ID.
                                    dpu_machine_id: DPU_ID.to_string(), // Supply the later DPU target.
                                    ..Default::default()
                                },
                                // Omit IDs after deletion so later polls must use the saved targets.
                                2 | 3 => AdminForceDeleteMachineResponse {
                                    all_done: true, // Complete this target.
                                    ..Default::default()
                                },
                                attempt => panic!("unexpected force-delete request {attempt}"),
                            };
                            Ok(grpc_response(response))
                        }
                    }),
                )
                .await
                .expect("mock serves force deletion");
        });
        let client = ApiClient(CarbideApiClient::new(
            &ApiConfig::new(
                &format!("http://{address}"),
                &CarbideClientConfig::default(),
            ),
            carbide_instrument::Instrumentation::noop(),
        ));

        // Exercise both real five-second polling delays and stop the server before assertions.
        let result =
            tokio::time::timeout(Duration::from_secs(30), force_delete(query, &client)).await;
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "mock Core server failed: {error}");
        }
        result
            .expect("force deletion completes within thirty seconds")
            .expect("force deletion succeeds");

        // Require the original selector, the resolved host, then the cached DPU with unchanged flags.
        let requests = captured_requests
            .lock()
            .expect("captured requests are available");
        assert_eq!(requests.len(), 3);
        for (request, expected_query) in requests.iter().zip([
            // Resolve the operator's IP only on the initial request.
            SELECTOR,
            // Pin pending deletion to the host even if the IP later changes owners.
            HOST_ID, // Query the cached DPU after host completion omits both IDs.
            DPU_ID,
        ]) {
            // Change only the expected target so every consent and cleanup flag must match.
            let mut expected = initial_request.clone();
            expected.host_query = expected_query.to_string();
            assert_eq!(request, &expected);
        }
    }
}
