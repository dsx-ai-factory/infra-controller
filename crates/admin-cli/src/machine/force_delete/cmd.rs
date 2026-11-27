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

        if response.all_done {
            println!("Force delete for {} succeeded", query.machine);
            // A cached DPU ID can already identify reingested hardware after Core finishes.
            break;
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
    use ::rpc::forge::{AdminForceDeleteMachineResponse, BuildInfo, InstanceList};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
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

    const HOST_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
    const DPU_ID: &str = "fm100dt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

    /// Verifies pending selectors poll Core's host ID and stop when it completes,
    /// preserving operator consent and preventing address reassignment or a later
    /// cached-DPU request from redirecting deletion.
    ///
    /// Core completion must also stop deletion on the first reply with attached DPU IDs.
    /// Omitted orphan consent and DPU waiting must remain disabled on every poll.
    #[tokio::test]
    async fn force_delete_polls_resolved_machine_ids() {
        // Supply orphan consent and DPU waiting only in cases that exercise explicit opt-ins.
        let explicit_flags = [
            "--allow-delete-with-orphaned-dpf-crds",
            "--wait-for-instance-dpu",
        ];
        let cases = [
            // Resolve an address once so reassignment cannot redirect later polling.
            Case {
                scenario: "IP selectors are resolved only once",
                input: (
                    "192.0.2.10",
                    vec!["192.0.2.10", HOST_ID],
                    explicit_flags.as_slice(),
                ),
                expect: Yields(()),
            },
            // Preserve an exact host target through pending deletion and completion.
            // Omit orphan consent and DPU waiting so retries must preserve default cleanup.
            Case {
                scenario: "exact host IDs remain the polling target",
                input: (HOST_ID, vec![HOST_ID, HOST_ID], [].as_slice()),
                expect: Yields(()),
            },
            // Stop a DPU-selected deletion when its host completes before another DPU request.
            Case {
                scenario: "exact DPU IDs finish with their resolved host",
                input: (DPU_ID, vec![DPU_ID, HOST_ID], explicit_flags.as_slice()),
                expect: Yields(()),
            },
            // Stop after immediate completion even when Core returns attached DPU IDs.
            Case {
                scenario: "completed deletion does not retry the returned DPU IDs",
                input: (HOST_ID, vec![HOST_ID], explicit_flags.as_slice()),
                expect: Yields(()),
            },
        ];
        // Exercise each selector with its supplied flags through the command and decoded requests.
        check_cases_async(
            cases,
            |(selector, expected_queries, extra_args)| async move {
                check_force_delete_requests(selector, &expected_queries, extra_args).await;
                Ok::<_, ()>(())
            },
        )
        .await;
    }

    /// Checks decoded force-delete targets and explicit consent against the expected
    /// polling sequence so each selector exercises the public RPC transport.
    /// Omitted orphan consent and DPU waiting must stay disabled on every request.
    async fn check_force_delete_requests(
        selector: &str,
        expected_queries: &[&str],
        extra_args: &[&str],
    ) {
        // Keep Instance deletion consent separate from the row's orphan consent and DPU waiting.
        let query = Args::try_parse_from(
            [
                "force-delete",
                "--machine",
                selector,
                "--allow-delete-with-instance",
            ]
            .into_iter()
            .chain(extra_args.iter().copied()),
        )
        .expect("force-delete arguments parse");
        let initial_request = AdminForceDeleteMachineRequest::from(&query);

        // Prepare a mock Core listener and a record of its decoded requests.
        // Complete on the last expected request so each row selects immediate or pending deletion.
        let expected_request_count = expected_queries.len();
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
                            // Exact Machine IDs check for an Instance before sending deletion.
                            if request.uri().path()
                                == ::rpc::service_path!("FindInstanceByMachineID")
                            {
                                return Ok(grpc_response(InstanceList::default()));
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
                                // Preserve Machine IDs in terminal replies to exercise Core's completed response.
                                attempt if attempt <= expected_request_count => {
                                    AdminForceDeleteMachineResponse {
                                        all_done: attempt == expected_request_count, // Finish on the expected request.
                                        managed_host_machine_id: HOST_ID.to_string(), // Return the host ID.
                                        dpu_machine_id: DPU_ID.to_string(), // Include the attached DPU in the response.
                                        dpu_machine_ids: vec![DPU_ID.to_string()], // Include every attached DPU ID.
                                        ..Default::default()
                                    }
                                }
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

        // Exercise the real five-second delay for pending deletion and stop the server before assertions.
        let result =
            tokio::time::timeout(Duration::from_secs(30), force_delete(query, &client)).await;
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "mock Core server failed: {error}");
        }
        result
            .expect("force deletion completes within thirty seconds")
            .expect("force deletion succeeds");

        // Require the expected targets and independently check supplied and omitted flags on every poll.
        let requests = captured_requests
            .lock()
            .expect("captured requests are available");
        assert_eq!(requests.len(), expected_queries.len());
        for (request, expected_query) in requests.iter().zip(expected_queries) {
            assert!(request.allow_delete_with_instance);
            assert!(request.allow_delete_with_instance_type);
            assert_eq!(
                request.allow_delete_with_orphaned_dpf_crds,
                extra_args.contains(&"--allow-delete-with-orphaned-dpf-crds")
            );
            assert_eq!(
                request.wait_for_instance_dpu,
                extra_args.contains(&"--wait-for-instance-dpu")
            );

            // Change only the expected target so every remaining cleanup flag must also match.
            let mut expected = initial_request.clone();
            expected.host_query = (*expected_query).to_string();
            assert_eq!(request, &expected);
        }
    }
}
