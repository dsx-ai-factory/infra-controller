// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

fn reset_batch_response(
    job_id: &str,
    total_nodes: u32,
    rejected_nodes: &[&str],
) -> rms::BatchResetSwitchSdnFactoryDefaultResponse {
    let failed_nodes = rejected_nodes.len() as u32;
    rms::BatchResetSwitchSdnFactoryDefaultResponse {
        response: Some(rms::NodeBatchResponse {
            job_id: job_id.to_owned(),
            status: if failed_nodes == 0 {
                rms::ReturnCode::Success
            } else {
                rms::ReturnCode::Failure
            } as i32,
            stats: Some(rms::NodeOperationStats {
                total_nodes,
                successful_nodes: total_nodes - failed_nodes,
                failed_nodes,
            }),
            node_results: rejected_nodes
                .iter()
                .map(|node_id| rms::NodeOperationResult {
                    node_id: (*node_id).to_owned(),
                    status: rms::ReturnCode::Failure as i32,
                    error_message: "reset already in progress".to_owned(),
                })
                .collect(),
            ..Default::default()
        }),
    }
}

#[carbide_macros::sqlx_test]
async fn component_reset_replaces_only_accepted_references(pool: sqlx::PgPool) {
    let (mock, backend, _, _, _, sw1, sw2) = make_backend(&pool).await;
    let endpoints = [make_sw_endpoint(SW_MAC_2), make_sw_endpoint(SW_MAC_1)];
    let kind = RmsJobKind::SwitchSdnFactoryReset;
    for endpoint in &endpoints {
        db::direct_dispatch_rms_job::save(&pool, endpoint.bmc_mac, kind, "previous-reset")
            .await
            .unwrap();
    }
    let mut all_rejected = reset_batch_response("", 2, &[&sw1.to_string(), &sw2.to_string()]);
    let rejected_batch = all_rejected.response.as_mut().unwrap();
    rejected_batch.node_results.clear();
    rejected_batch.message = "batch rejected before admission".into();
    let mut partly_reported_rejection =
        reset_batch_response("", 2, &[&sw1.to_string(), &sw2.to_string()]);
    let rejected_batch = partly_reported_rejection.response.as_mut().unwrap();
    rejected_batch
        .node_results
        .retain(|result| result.node_id == sw1.to_string());
    rejected_batch.message = "parent job creation failed".into();
    let cases = [
        (
            // Only SW1 is named by RMS, despite SW2 being submitted first.
            "partial acceptance correlates the named rejection",
            Ok(reset_batch_response("new-reset", 2, &[&sw1.to_string()])),
            [true, false],
            [None, Some("reset already in progress")],
        ),
        (
            "whole batch rejected without individual records",
            Ok(all_rejected),
            [false, false],
            [Some("batch rejected before admission"); 2],
        ),
        (
            "parent creation failed after one node was rejected",
            Ok(partly_reported_rejection),
            [false, false],
            [
                Some("parent job creation failed"),
                Some("reset already in progress"),
            ],
        ),
        (
            "missing accepted parent ID",
            Ok(reset_batch_response(" \t", 2, &[&sw1.to_string()])),
            [false, false],
            [
                Some("accepted the SDN reset but returned no job reference"),
                Some("reset already in progress"),
            ],
        ),
        (
            "ambiguous",
            Err(RackManagerError::ApiInvocationError(
                tonic::Status::unavailable("reply lost after acceptance"),
            )),
            [false, false],
            [Some("reply lost after acceptance"); 2],
        ),
    ];
    let case_count = cases.len();
    for (index, (scenario, response, accepted, expected_errors)) in cases.into_iter().enumerate() {
        mock.enqueue_batch_reset_switch_sdn_factory_default(response)
            .await;
        let results = NvSwitchManager::batch_reset_sdn_factory_default(&backend, &endpoints, None)
            .await
            .unwrap();
        assert_eq!(results.len(), endpoints.len());
        let results: HashMap<_, _> = results
            .into_iter()
            .map(|result| (result.bmc_mac, result))
            .collect();
        assert_eq!(
            endpoints
                .iter()
                .map(|endpoint| results[&endpoint.bmc_mac].success)
                .collect::<Vec<_>>(),
            accepted,
            "{scenario}: {results:?}",
        );
        for (endpoint, expected_error) in endpoints.iter().zip(expected_errors) {
            let error = results[&endpoint.bmc_mac].error.as_deref();
            match expected_error {
                Some(expected) => assert!(
                    error.is_some_and(|error| error.contains(expected)),
                    "{scenario}: {}: {error:?}",
                    endpoint.bmc_mac,
                ),
                None => assert!(error.is_none(), "{scenario}: {error:?}"),
            }
        }
        for (endpoint, expected) in endpoints.iter().zip(["new-reset", "previous-reset"]) {
            assert_eq!(
                db::direct_dispatch_rms_job::get(&pool, endpoint.bmc_mac, kind)
                    .await
                    .unwrap()
                    .as_deref(),
                Some(expected),
                "{scenario}: {}",
                endpoint.bmc_mac,
            );
        }
        assert_eq!(
            mock.batch_reset_switch_sdn_factory_default_calls()
                .await
                .len(),
            index + 1,
            "{scenario}: submissions must not be retried",
        );
    }
    // A storage failure preserves that switch's previous reference and cause,
    // while accepted peers still get recorded under the shared parent.
    sqlx::query(
        "ALTER TABLE direct_dispatch_rms_jobs ADD CHECK \
         (job_id <> 'unrecorded-reset' OR bmc_mac <> 'aa:bb:cc:dd:ff:02')",
    )
    .execute(&pool)
    .await
    .unwrap();
    mock.enqueue_batch_reset_switch_sdn_factory_default(Ok(reset_batch_response(
        "unrecorded-reset",
        2,
        &[],
    )))
    .await;
    let (results, logs) = capture_logs_async(NvSwitchManager::batch_reset_sdn_factory_default(
        &backend, &endpoints, None,
    ))
    .await;
    let results = results.unwrap();
    assert_eq!(results.len(), endpoints.len());
    let results: HashMap<_, _> = results
        .into_iter()
        .map(|result| (result.bmc_mac, result))
        .collect();
    assert!(!results[&endpoints[0].bmc_mac].success);
    let error = results[&endpoints[0].bmc_mac].error.as_deref().unwrap();
    assert!(error.contains("RMS accepted the reset but NICo could not record its reference"));
    assert!(error.contains("violates check constraint"));
    assert!(results[&endpoints[1].bmc_mac].success);
    assert!(results[&endpoints[1].bmc_mac].error.is_none());
    for (endpoint, expected) in endpoints.iter().zip(["new-reset", "unrecorded-reset"]) {
        assert_eq!(
            db::direct_dispatch_rms_job::get(&pool, endpoint.bmc_mac, kind)
                .await
                .unwrap()
                .as_deref(),
            Some(expected),
        );
    }
    assert!(
        logs.iter()
            .any(|entry| entry.field("rms_job_id") == Some("unrecorded-reset"))
    );
    assert_eq!(
        mock.batch_reset_switch_sdn_factory_default_calls()
            .await
            .len(),
        case_count + 1
    );
    assert!(mock.get_job_status_calls().await.is_empty());
}

#[tokio::test]
async fn component_reset_submission_rejects_unobservable_batches() {
    #[derive(Debug, PartialEq, Eq)]
    enum Outcome {
        Unknown,
        Unsupported,
    }
    use Outcome::{Unknown, Unsupported};

    let mut inconsistent = reset_batch_response("parent", 2, &[]);
    inconsistent.response.as_mut().unwrap().status = rms::ReturnCode::Failure as i32;
    let mut missing_rejection = reset_batch_response("parent", 2, &["node-1"]);
    missing_rejection
        .response
        .as_mut()
        .unwrap()
        .node_results
        .clear();
    let mut duplicate = reset_batch_response("parent", 2, &["node-1"]);
    let duplicate_batch = duplicate.response.as_mut().unwrap();
    duplicate_batch
        .node_results
        .push(duplicate_batch.node_results[0].clone());
    let mut unexpected_success = reset_batch_response("parent", 2, &["node-1"]);
    unexpected_success
        .response
        .as_mut()
        .unwrap()
        .node_results
        .first_mut()
        .unwrap()
        .status = rms::ReturnCode::Success as i32;
    let cases = [
        (
            "missing batch",
            Ok(rms::BatchResetSwitchSdnFactoryDefaultResponse::default()),
            Unknown,
            None,
        ),
        (
            "contradictory status",
            Ok(inconsistent),
            Unknown,
            Some("parent"),
        ),
        (
            "missing rejection identity",
            Ok(missing_rejection),
            Unknown,
            Some("parent"),
        ),
        (
            "unknown rejection identity",
            Ok(reset_batch_response("parent", 2, &["other-node"])),
            Unknown,
            Some("parent"),
        ),
        (
            "duplicate rejection identity",
            Ok(duplicate),
            Unknown,
            Some("parent"),
        ),
        (
            "unexpected success in rejection records",
            Ok(unexpected_success),
            Unknown,
            Some("parent"),
        ),
        (
            "lost transport",
            Err(RackManagerError::ApiInvocationError(
                tonic::Status::unavailable("lost"),
            )),
            Unknown,
            None,
        ),
        (
            "unsupported RPC",
            Err(RackManagerError::ApiInvocationError(
                tonic::Status::unimplemented("unsupported"),
            )),
            Unsupported,
            None,
        ),
    ];
    carbide_test_support::check_cases_async(
        cases
            .into_iter()
            .map(|(scenario, input, expected, candidate_job_id)| Case {
                scenario,
                input,
                expect: Yields((expected, candidate_job_id.map(str::to_owned))),
            }),
        |response| async move {
            let mock = MockRmsApi::new();
            mock.enqueue_batch_reset_switch_sdn_factory_default(response)
                .await;
            let switches = [(SW_MAC_1, "node-1"), (SW_MAC_2, "node-2")].map(|(mac, node_id)| {
                (
                    mac.parse::<MacAddress>().unwrap(),
                    rms::NodeInfo {
                        node_id: node_id.to_owned(),
                        ..Default::default()
                    },
                )
            });
            let (result, logs) = capture_logs_async(rms_batch_reset_switch_sdn_factory_default(
                &mock,
                switches.into(),
                None,
            ))
            .await;
            let candidate_job_id = logs
                .iter()
                .find_map(|log| log.field("rms_job_id").map(str::to_owned));
            let outcome = match result {
                Err(ComponentManagerError::OperationOutcomeUnknown(_)) => Unknown,
                Err(ComponentManagerError::Unsupported(_)) => Unsupported,
                other => return Err(format!("unexpected result {other:?}")),
            };
            assert_eq!(
                mock.batch_reset_switch_sdn_factory_default_calls()
                    .await
                    .len(),
                1
            );
            Ok::<_, String>((outcome, candidate_job_id))
        },
    )
    .await;
}

#[carbide_macros::sqlx_test]
async fn component_reset_dispatches_valid_targets_in_one_batch(pool: sqlx::PgPool) {
    let (mock, backend, _, _, _, sw1, sw2) = make_backend(&pool).await;
    let endpoints = [
        make_sw_endpoint(SW_MAC_2),
        make_sw_endpoint(UNKNOWN_MAC),
        make_sw_endpoint(SW_MAC_1),
    ];
    mock.enqueue_batch_reset_switch_sdn_factory_default(Ok(reset_batch_response("parent", 2, &[])))
        .await;
    let results =
        NvSwitchManager::batch_reset_sdn_factory_default(&backend, &endpoints, Some("sdn-domain"))
            .await
            .unwrap();
    assert_eq!(results.len(), endpoints.len());
    let results: HashMap<_, _> = results
        .into_iter()
        .map(|result| (result.bmc_mac, result))
        .collect();
    assert!(results[&endpoints[0].bmc_mac].success);
    assert!(!results[&endpoints[1].bmc_mac].success);
    assert!(
        results[&endpoints[1].bmc_mac]
            .error
            .as_deref()
            .unwrap()
            .contains(RMS_IDENTITY_LOOKUP_ERROR)
    );
    assert!(results[&endpoints[2].bmc_mac].success);
    let calls = mock.batch_reset_switch_sdn_factory_default_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].domain.as_deref(), Some("sdn-domain"));
    assert_eq!(
        calls[0]
            .nodes
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .map(|node| node.node_id.as_str())
            .collect::<Vec<_>>(),
        [sw2.to_string(), sw1.to_string()],
    );
    for endpoint in [&endpoints[0], &endpoints[2]] {
        assert_eq!(
            db::direct_dispatch_rms_job::get(
                &pool,
                endpoint.bmc_mac,
                RmsJobKind::SwitchSdnFactoryReset
            )
            .await
            .unwrap()
            .as_deref(),
            Some("parent"),
        );
    }
}

#[carbide_macros::sqlx_test]
async fn component_reset_status_observes_only_the_requested_child(pool: sqlx::PgPool) {
    let (mock, backend, _, _, _, sw1, sw2) = make_backend(&pool).await;
    let mac = SW_MAC_1.parse::<MacAddress>().unwrap();
    db::direct_dispatch_rms_job::save(&pool, mac, RmsJobKind::SwitchSdnFactoryReset, "parent")
        .await
        .unwrap();
    let child = |id: &str, node_id: &str, state: rms::JobExecutionState| rms::JobStatus {
        job_id: id.to_owned(),
        parent_job_id: Some("parent".to_owned()),
        node_id: Some(node_id.to_owned()),
        execution_state: state as i32,
        error_message: if state == rms::JobExecutionState::Failed {
            "target failure".to_owned()
        } else {
            String::new()
        },
        ..Default::default()
    };
    let mut cases = [
        (
            "queued despite failed sibling",
            rms::JobExecutionState::Queued,
            ComponentResetState::Queued,
        ),
        (
            "running despite failed sibling",
            rms::JobExecutionState::Running,
            ComponentResetState::InProgress,
        ),
        (
            "completed despite failed sibling",
            rms::JobExecutionState::Completed,
            ComponentResetState::Completed,
        ),
        (
            "failed child reports its own error",
            rms::JobExecutionState::Failed,
            ComponentResetState::Failed("target failure".into()),
        ),
    ]
    .into_iter()
    .map(|(scenario, state, expected)| {
        (
            scenario,
            Ok(vec![child("target", &sw1.to_string(), state)]),
            Some(expected),
        )
    })
    .collect::<Vec<_>>();
    let mut unrelated = child(
        "unrelated",
        &sw1.to_string(),
        rms::JobExecutionState::Completed,
    );
    unrelated.parent_job_id = Some("other-parent".to_owned());
    cases.extend([
        (
            "MAC child survives ingestion",
            Ok(vec![child(
                "target",
                &mac.to_string(),
                rms::JobExecutionState::Completed,
            )]),
            Some(ComponentResetState::Completed),
        ),
        ("missing target child", Ok(vec![]), None),
        (
            "matching node from another parent",
            Ok(vec![unrelated]),
            None,
        ),
        (
            "ambiguous MAC and switch ID children",
            Ok(vec![
                child(
                    "target",
                    &sw1.to_string(),
                    rms::JobExecutionState::Completed,
                ),
                child(
                    "old-identity",
                    &mac.to_string(),
                    rms::JobExecutionState::Running,
                ),
            ]),
            None,
        ),
        (
            "unknown child state",
            Ok(vec![child(
                "target",
                &sw1.to_string(),
                rms::JobExecutionState::Unspecified,
            )]),
            None,
        ),
        (
            "RMS lost the job",
            Err(RackManagerError::ApiInvocationError(
                tonic::Status::not_found("RMS restarted"),
            )),
            None,
        ),
    ]);
    let case_count = cases.len();
    carbide_test_support::check_cases_async(
        cases.into_iter().map(|(scenario, input, expected)| Case {
            scenario,
            input: (input, expected),
            expect: Yields(()),
        }),
        |(response, expected)| {
            let mock = &mock;
            let backend = &backend;
            async move {
                let response = response.map(|mut children| {
                    children.extend([
                        rms::JobStatus {
                            job_id: "parent".to_owned(),
                            execution_state: rms::JobExecutionState::Failed as i32,
                            ..Default::default()
                        },
                        rms::JobStatus {
                            error_message: "unrelated sibling failure".to_owned(),
                            ..child("sibling", &sw2.to_string(), rms::JobExecutionState::Failed)
                        },
                    ]);
                    rms::GetJobStatusResponse {
                        job_states: children,
                    }
                });
                mock.enqueue_get_job_status(response).await;
                let mut statuses = NvSwitchManager::get_sdn_factory_reset_status(backend, &[mac])
                    .await
                    .map_err(|error| error.to_string())?;
                assert_eq!(statuses.len(), 1);
                let status = statuses.pop().unwrap();
                assert_eq!(status.bmc_mac, mac);
                match (status.state, expected) {
                    (
                        Ok(ComponentResetState::Failed(message)),
                        Some(ComponentResetState::Failed(expected_message)),
                    ) if message.contains(&expected_message) => Ok(()),
                    (Ok(state), Some(expected)) if state == expected => Ok(()),
                    (Err(ComponentManagerError::OperationOutcomeUnknown(_)), None) => Ok(()),
                    (observed, expected) => Err(format!(
                        "unexpected result {observed:?}; expected {expected:?}"
                    )),
                }
            }
        },
    )
    .await;
    let calls = mock.get_job_status_calls().await;
    assert_eq!(calls.len(), case_count);
    assert!(
        calls
            .iter()
            .all(|call| call.job_id == "parent" && call.include_child_job_states)
    );

    // A missing reference is local to its MAC; known peers retain their own
    // observed states, including a terminal reset failure with a successful poll.
    let second_mac = SW_MAC_2.parse::<MacAddress>().unwrap();
    let untracked_mac = UNKNOWN_MAC.parse::<MacAddress>().unwrap();
    db::direct_dispatch_rms_job::save(
        &pool,
        second_mac,
        RmsJobKind::SwitchSdnFactoryReset,
        "parent",
    )
    .await
    .unwrap();
    let response = rms::GetJobStatusResponse {
        job_states: vec![
            rms::JobStatus {
                job_id: "parent".to_owned(),
                ..Default::default()
            },
            child("first", &sw1.to_string(), rms::JobExecutionState::Completed),
            child("second", &sw2.to_string(), rms::JobExecutionState::Failed),
        ],
    };
    for _ in 0..2 {
        mock.enqueue_get_job_status(Ok(response.clone())).await;
    }
    let statuses =
        NvSwitchManager::get_sdn_factory_reset_status(&backend, &[untracked_mac, mac, second_mac])
            .await
            .unwrap();
    assert_eq!(statuses.len(), 3);
    let statuses: HashMap<_, _> = statuses
        .into_iter()
        .map(|status| (status.bmc_mac, status))
        .collect();
    assert!(matches!(
        statuses[&untracked_mac].state,
        Err(ComponentManagerError::NotFound(_))
    ));
    assert!(matches!(
        statuses[&mac].state,
        Ok(ComponentResetState::Completed)
    ));
    assert!(matches!(
        &statuses[&second_mac].state,
        Ok(ComponentResetState::Failed(message)) if message.contains("target failure")
    ));
    assert_eq!(mock.get_job_status_calls().await.len(), case_count + 2);
    assert!(
        mock.batch_reset_switch_sdn_factory_default_calls()
            .await
            .is_empty()
    );
}
