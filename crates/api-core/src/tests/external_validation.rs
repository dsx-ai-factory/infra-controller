// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Persistence, concurrency, authorization, and immutable replay boundaries of the workflow RPCs.

use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::{self as rpc};
use carbide_authn::middleware::{ExternalUserInfo, Principal};
use carbide_uuid::external_validation::{
    ExternalValidationCycleId, ExternalValidationHoldId, ExternalValidationRequestId,
};
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::StableHostMachineId;
use chrono::Utc;
use health_report::{HealthAlertClassification, HealthProbeAlert, HealthReport};
use model::external_validation::{AttemptState, Hold, HoldState};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::{Code, Request};
use uuid::Uuid;

use crate::auth::AuthContext;
use crate::tests::common::api_fixtures::instance::default_tenant_config;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_config,
};

const SOURCE: &str = "external-validation-hold";

fn authenticated<T>(value: T) -> Request<T> {
    with_principal(
        value,
        Principal::SpiffeServiceIdentifier("elektra-site-agent".to_string()),
    )
}

fn with_principal<T>(value: T, principal: Principal) -> Request<T> {
    let mut request = Request::new(value);
    request.extensions_mut().insert(AuthContext {
        principals: vec![principal],
        authorization: None,
    });
    request
}

fn tenant() -> String {
    default_tenant_config().tenant_organization_id
}

async fn seed_hold(env: &TestEnv, machine: StableHostMachineId, org: &str) -> Hold {
    let mut txn = env.pool.begin().await.unwrap();
    let hold = db::external_validation::create_hold(
        &mut txn,
        machine,
        org,
        ExternalValidationCycleId::new(),
        SOURCE,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    hold
}

fn start_request(machine: StableHostMachineId, key: Uuid) -> rpc::StartExternalValidationRequest {
    rpc::StartExternalValidationRequest {
        machine_id: Some(machine),
        tenant_org_id: tenant(),
        caller_idempotency_key: key.to_string(),
    }
}

async fn read_hold(env: &TestEnv, id: ExternalValidationHoldId) -> rpc::ExternalValidationHold {
    env.api
        .find_external_validation_holds_by_ids(authenticated(
            rpc::ExternalValidationHoldsByIdsRequest {
                tenant_org_id: None,
                hold_ids: vec![id],
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .holds
        .pop()
        .unwrap()
}

async fn project_health(env: &TestEnv, machine: StableHostMachineId, source: &str) {
    let mut report = HealthReport::empty(source.to_string());
    report.alerts.push(HealthProbeAlert {
        id: "ExternalValidationRequired".parse().unwrap(),
        target: None,
        in_alert_since: Some(Utc::now()),
        message: "allocation block".to_string(),
        tenant_message: None,
        classifications: vec![HealthAlertClassification::prevent_allocations()],
    });
    env.api
        .insert_machine_health_report(Request::new(rpc::InsertMachineHealthReportRequest {
            machine_id: Some(machine.into()),
            health_report_entry: Some(rpc::HealthReportEntry {
                report: Some(report.into()),
                mode: rpc::HealthReportApplyMode::Merge.into(),
            }),
        }))
        .await
        .unwrap();
}

/// Concurrent identical starts share one request; different keys return that open attempt without rebinding.
#[crate::sqlx_test]
async fn test_external_validation_start(_: PgPoolOptions, options: PgConnectOptions) {
    let env = create_test_env(PgPoolOptions::new().connect_with(options).await.unwrap()).await;
    let host = create_managed_host(&env).await;
    let other = create_managed_host(&env).await;
    let empty = env
        .api
        .start_external_validation(authenticated(start_request(host.id, Uuid::new_v4())))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        empty.status,
        i32::from(rpc::ExternalValidationStartStatus::NoActiveHold)
    );
    assert!(empty.hold_id.is_none());
    let hold = seed_hold(&env, host.id, &tenant()).await;
    assert_eq!(hold.created_at, hold.state_changed_at);
    project_health(&env, host.id, SOURCE).await;
    // A separate source with the same alert ID must still prevent admission.
    project_health(&env, host.id, "unrelated-source").await;
    let key = Uuid::new_v4();
    let ineligible = env
        .api
        .start_external_validation(authenticated(start_request(host.id, key)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        ineligible.status,
        i32::from(rpc::ExternalValidationStartStatus::NoActiveHold)
    );
    assert_eq!(ineligible.hold_id, Some(hold.hold_id));
    assert!(ineligible.attempt.is_none());
    env.api
        .remove_machine_health_report(Request::new(rpc::RemoveMachineHealthReportRequest {
            machine_id: Some(host.id.into()),
            source: "unrelated-source".to_string(),
        }))
        .await
        .unwrap();
    let (first, second) = tokio::join!(
        env.api
            .start_external_validation(authenticated(start_request(host.id, key))),
        env.api
            .start_external_validation(authenticated(start_request(host.id, key))),
    );
    let first = first.unwrap().into_inner();
    let second = second.unwrap().into_inner();
    assert_eq!(
        first.status,
        i32::from(rpc::ExternalValidationStartStatus::Opened)
    );
    assert_eq!(second.status, first.status);
    assert_ne!(first.replayed, second.replayed);
    assert_eq!(first.attempt, second.attempt);
    let request_id = first.attempt.unwrap().request_id.unwrap();
    let already = env
        .api
        .start_external_validation(authenticated(start_request(host.id, Uuid::new_v4())))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        already.status,
        i32::from(rpc::ExternalValidationStartStatus::AlreadyOpen)
    );
    assert_eq!(already.attempt.unwrap().request_id, Some(request_id));
    assert!(!already.replayed);
    assert_eq!(
        env.api
            .start_external_validation(authenticated(start_request(other.id, key)))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );
    let persisted = read_hold(&env, hold.hold_id).await;
    assert_eq!(
        persisted.state,
        i32::from(rpc::ExternalValidationHoldState::AttemptOpen)
    );
    assert!(persisted.state_changed_at.unwrap() > persisted.created_at.unwrap());
    let counts: (i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM external_validation_attempts), (SELECT count(*) FROM external_validation_audit)").fetch_one(&env.pool).await.unwrap();
    assert_eq!(counts, (1, 1));
}

/// Different concurrent keys on the same hold cannot create two open attempts.
#[crate::sqlx_test]
async fn test_external_validation_competing_starts(_: PgPoolOptions, options: PgConnectOptions) {
    let env = create_test_env(PgPoolOptions::new().connect_with(options).await.unwrap()).await;
    let host = create_managed_host(&env).await;
    let hold = seed_hold(&env, host.id, &tenant()).await;
    let (first, second) = tokio::join!(
        env.api
            .start_external_validation(authenticated(start_request(host.id, Uuid::new_v4()))),
        env.api
            .start_external_validation(authenticated(start_request(host.id, Uuid::new_v4()))),
    );
    let first = first.unwrap().into_inner();
    let second = second.unwrap().into_inner();
    let mut statuses = vec![first.status, second.status];
    statuses.sort();
    assert_eq!(
        statuses,
        vec![
            i32::from(rpc::ExternalValidationStartStatus::Opened),
            i32::from(rpc::ExternalValidationStartStatus::AlreadyOpen)
        ]
    );
    assert_eq!(first.attempt, second.attempt);
    let current = read_hold(&env, hold.hold_id).await;
    assert_eq!(current.attempt, first.attempt);
}

/// Inventory preserves exact filters, all-ID ordering, bounded details, and terminal visibility.
#[crate::sqlx_test]
async fn test_external_validation_discovery(_: PgPoolOptions, options: PgConnectOptions) {
    let mut config = get_config();
    config.max_find_by_ids = 2;
    let env = create_test_env_with_overrides(
        PgPoolOptions::new().connect_with(options).await.unwrap(),
        TestEnvOverrides::with_config(config),
    )
    .await;
    let a = create_managed_host(&env).await;
    let b = create_managed_host(&env).await;
    let c = create_managed_host(&env).await;
    let removed_host = create_managed_host(&env).await;
    let ha = seed_hold(&env, a.id, &tenant()).await;
    let hb = seed_hold(&env, b.id, "other-org").await;
    let hc = seed_hold(&env, c.id, &tenant()).await;
    let removed = seed_hold(&env, removed_host.id, &tenant()).await;
    sqlx::query(
        "UPDATE external_validation_holds SET removed_at = clock_timestamp() WHERE hold_id = $1",
    )
    .bind(removed.hold_id)
    .execute(&env.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE external_validation_holds SET state = 'satisfied' WHERE hold_id = $1")
        .bind(hc.hold_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let now = Utc::now();
    sqlx::query("UPDATE external_validation_holds SET created_at = $1, state_changed_at = $1 WHERE hold_id = ANY($2)").bind(now).bind(&[ha.hold_id, hb.hold_id][..]).execute(&env.pool).await.unwrap();
    let mut all = vec![ha.hold_id, hb.hold_id];
    all.sort();
    let cases = [
        ("site inventory", None, None, None, all.clone()),
        ("tenant", Some(tenant()), None, None, vec![ha.hold_id]),
        ("machine", None, Some(b.id), None, vec![hb.hold_id]),
        ("AND", Some(tenant()), Some(b.id), None, vec![]),
        (
            "satisfied",
            None,
            None,
            Some(i32::from(rpc::ExternalValidationHoldState::Satisfied)),
            vec![hc.hold_id],
        ),
    ];
    for (label, tenant_org_id, machine_id, state, expected) in cases {
        let ids = env
            .api
            .find_external_validation_hold_ids(authenticated(
                rpc::ExternalValidationHoldSearchFilter {
                    tenant_org_id,
                    machine_id,
                    state,
                },
            ))
            .await
            .unwrap()
            .into_inner()
            .hold_ids;
        assert_eq!(ids, expected, "{label}");
    }
    for state in [0, 999] {
        assert_eq!(
            env.api
                .find_external_validation_hold_ids(authenticated(
                    rpc::ExternalValidationHoldSearchFilter {
                        state: Some(state),
                        ..Default::default()
                    }
                ))
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
    }
    let cases = [
        (
            "duplicates",
            vec![ha.hold_id, ha.hold_id],
            None,
            Some(vec![ha.hold_id]),
        ),
        (
            "retained outcome",
            vec![hc.hold_id],
            None,
            Some(vec![hc.hold_id]),
        ),
        (
            "wrong tenant omitted",
            vec![hb.hold_id],
            Some(tenant()),
            Some(vec![]),
        ),
        (
            "unknown omitted",
            vec![ExternalValidationHoldId::new()],
            None,
            Some(vec![]),
        ),
        ("removed omitted", vec![removed.hold_id], None, Some(vec![])),
        ("empty", vec![], None, None),
        (
            "duplicates count toward configured cap",
            vec![ha.hold_id; 3],
            None,
            None,
        ),
        ("empty tenant", vec![ha.hold_id], Some(String::new()), None),
    ];
    for (label, hold_ids, tenant_org_id, expected) in cases {
        let result = env
            .api
            .find_external_validation_holds_by_ids(authenticated(
                rpc::ExternalValidationHoldsByIdsRequest {
                    tenant_org_id,
                    hold_ids,
                },
            ))
            .await;
        match expected {
            Some(ids) => assert_eq!(
                result
                    .unwrap()
                    .into_inner()
                    .holds
                    .into_iter()
                    .map(|hold| hold.hold_id.unwrap())
                    .collect::<Vec<_>>(),
                ids,
                "{label}"
            ),
            None => assert_eq!(result.unwrap_err().code(), Code::InvalidArgument, "{label}"),
        }
    }
}

/// Cancel and Start replay retain the original terminal attempt while a newer attempt stays untouched.
#[crate::sqlx_test]
async fn test_external_validation_cancel(_: PgPoolOptions, options: PgConnectOptions) {
    let env = create_test_env(PgPoolOptions::new().connect_with(options).await.unwrap()).await;
    let host = create_managed_host(&env).await;
    let other = create_managed_host(&env).await;
    let hold = seed_hold(&env, host.id, &tenant()).await;
    let key = Uuid::new_v4();
    let opened = env
        .api
        .start_external_validation(authenticated(start_request(host.id, key)))
        .await
        .unwrap()
        .into_inner()
        .attempt
        .unwrap();
    let id = opened.request_id.unwrap();
    let request = rpc::CancelExternalValidationRequest {
        request_id: Some(id),
        tenant_org_id: tenant(),
        details: "é".repeat(2048),
        machine_id: Some(host.id),
    };
    let cases = [
        (
            "empty reason",
            rpc::CancelExternalValidationRequest {
                details: String::new(),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "byte bound",
            rpc::CancelExternalValidationRequest {
                details: "é".repeat(2049),
                ..request.clone()
            },
            Code::InvalidArgument,
        ),
        (
            "expected host",
            rpc::CancelExternalValidationRequest {
                machine_id: Some(other.id),
                ..request.clone()
            },
            Code::NotFound,
        ),
        (
            "tenant binding",
            rpc::CancelExternalValidationRequest {
                tenant_org_id: "wrong-org".into(),
                ..request.clone()
            },
            Code::PermissionDenied,
        ),
        (
            "unknown",
            rpc::CancelExternalValidationRequest {
                request_id: Some(ExternalValidationRequestId::new()),
                ..request.clone()
            },
            Code::NotFound,
        ),
    ];
    for (label, input, code) in cases {
        assert_eq!(
            env.api
                .cancel_external_validation(authenticated(input))
                .await
                .unwrap_err()
                .code(),
            code,
            "{label}"
        );
    }
    for (state, instance_id) in [("creating", None), ("created", Some(InstanceId::new()))] {
        sqlx::query("UPDATE external_validation_attempts SET allocation_state = $2, validation_instance_id = $3 WHERE request_id = $1").bind(id).bind(state).bind(instance_id).execute(&env.pool).await.unwrap();
        assert_eq!(
            env.api
                .cancel_external_validation(authenticated(request.clone()))
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition
        );
    }
    sqlx::query("UPDATE external_validation_attempts SET allocation_state = 'not_started', validation_instance_id = NULL WHERE request_id = $1").bind(id).execute(&env.pool).await.unwrap();
    let admin = Principal::ExternalUser(ExternalUserInfo::new(
        None,
        "nico-cli-client".into(),
        Some("operator".into()),
    ));
    let closed = env
        .api
        .cancel_external_validation(with_principal(request.clone(), admin))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        closed.state,
        i32::from(rpc::ExternalValidationAttemptState::Cancelled)
    );
    assert!(closed.closed_at.is_some());
    let pending = read_hold(&env, hold.hold_id).await;
    assert_eq!(
        pending.state,
        i32::from(rpc::ExternalValidationHoldState::Pending)
    );
    assert!(pending.attempt.is_none());
    let newer = env
        .api
        .start_external_validation(authenticated(start_request(host.id, Uuid::new_v4())))
        .await
        .unwrap()
        .into_inner()
        .attempt
        .unwrap();
    let before_replay = read_hold(&env, hold.hold_id).await;
    assert_eq!(
        env.api
            .cancel_external_validation(authenticated(request.clone()))
            .await
            .unwrap()
            .into_inner(),
        closed
    );
    let replayed_start = env
        .api
        .start_external_validation(authenticated(start_request(host.id, key)))
        .await
        .unwrap()
        .into_inner();
    assert!(replayed_start.replayed);
    assert_eq!(replayed_start.attempt, Some(closed));
    assert_eq!(read_hold(&env, hold.hold_id).await, before_replay);
    assert_eq!(before_replay.attempt, Some(newer));
    for (input, code) in [
        (
            rpc::CancelExternalValidationRequest {
                details: "changed".into(),
                ..request.clone()
            },
            Code::AlreadyExists,
        ),
        (
            rpc::CancelExternalValidationRequest {
                machine_id: Some(other.id),
                ..request
            },
            Code::NotFound,
        ),
    ] {
        assert_eq!(
            env.api
                .cancel_external_validation(authenticated(input))
                .await
                .unwrap_err()
                .code(),
            code
        );
    }
    let actors: Vec<String> = sqlx::query_scalar(
        "SELECT actor FROM external_validation_audit WHERE request_id = $1 ORDER BY id",
    )
    .bind(id)
    .fetch_all(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        actors,
        [
            "spiffe-service-id/elektra-site-agent",
            "external-user/operator"
        ]
    );
}

/// Every terminal outcome retains its instance binding; replay after cleanup cannot advance gate timestamps.
#[crate::sqlx_test]
async fn test_external_validation_complete(_: PgPoolOptions, options: PgConnectOptions) {
    let env = create_test_env(PgPoolOptions::new().connect_with(options).await.unwrap()).await;
    let segment = env.create_vpc_and_tenant_segment().await;
    let other = create_managed_host(&env).await;
    for (outcome, expected_state) in [
        (
            rpc::ExternalValidationOutcome::Passed,
            rpc::ExternalValidationAttemptState::Passed,
        ),
        (
            rpc::ExternalValidationOutcome::Failed,
            rpc::ExternalValidationAttemptState::Failed,
        ),
        (
            rpc::ExternalValidationOutcome::Cancelled,
            rpc::ExternalValidationAttemptState::Cancelled,
        ),
    ] {
        let host = create_managed_host(&env).await;
        let hold = seed_hold(&env, host.id, &tenant()).await;
        let key = Uuid::new_v4();
        let id = env
            .api
            .start_external_validation(authenticated(start_request(host.id, key)))
            .await
            .unwrap()
            .into_inner()
            .attempt
            .unwrap()
            .request_id
            .unwrap();
        let instance = host
            .instance_builer(&env)
            .single_interface_network_config(segment)
            .build()
            .await;
        let request = rpc::CompleteExternalValidationRequest {
            request_id: Some(id),
            tenant_org_id: tenant(),
            validation_instance_id: Some(instance.id),
            outcome: outcome.into(),
            details: None,
            machine_id: Some(host.id),
        };
        if outcome == rpc::ExternalValidationOutcome::Passed {
            assert_eq!(
                env.api
                    .complete_external_validation(authenticated(request.clone()))
                    .await
                    .unwrap_err()
                    .code(),
                Code::FailedPrecondition
            );
        }
        // M3 will write this pair atomically with allocation; no M2 RPC can manufacture a binding.
        sqlx::query("UPDATE external_validation_attempts SET allocation_state = 'created', validation_instance_id = $2 WHERE request_id = $1").bind(id).bind(instance.id).execute(&env.pool).await.unwrap();
        if outcome == rpc::ExternalValidationOutcome::Passed {
            let cases = [
                (
                    "binding",
                    rpc::CompleteExternalValidationRequest {
                        validation_instance_id: Some(InstanceId::new()),
                        ..request.clone()
                    },
                    Code::AlreadyExists,
                ),
                (
                    "expected host",
                    rpc::CompleteExternalValidationRequest {
                        machine_id: Some(other.id),
                        ..request.clone()
                    },
                    Code::NotFound,
                ),
                (
                    "unspecified",
                    rpc::CompleteExternalValidationRequest {
                        outcome: 0,
                        ..request.clone()
                    },
                    Code::InvalidArgument,
                ),
                (
                    "unknown outcome",
                    rpc::CompleteExternalValidationRequest {
                        outcome: 999,
                        ..request.clone()
                    },
                    Code::InvalidArgument,
                ),
                (
                    "byte bound",
                    rpc::CompleteExternalValidationRequest {
                        details: Some("é".repeat(2049)),
                        ..request.clone()
                    },
                    Code::InvalidArgument,
                ),
            ];
            for (label, input, code) in cases {
                assert_eq!(
                    env.api
                        .complete_external_validation(authenticated(input))
                        .await
                        .unwrap_err()
                        .code(),
                    code,
                    "{label}"
                );
            }
        }
        let closed = env
            .api
            .complete_external_validation(authenticated(request.clone()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(closed.state, i32::from(expected_state));
        assert!(closed.closed_at.is_some());
        assert_eq!(closed.validation_instance_id, Some(instance.id));
        let awaiting = read_hold(&env, hold.hold_id).await;
        assert_eq!(
            awaiting.state,
            i32::from(rpc::ExternalValidationHoldState::AwaitingCleanup)
        );
        if outcome == rpc::ExternalValidationOutcome::Passed {
            assert_eq!(
                env.api
                    .start_external_validation(authenticated(start_request(
                        host.id,
                        Uuid::new_v4()
                    )))
                    .await
                    .unwrap_err()
                    .code(),
                Code::FailedPrecondition
            );
        }
        instance.delete().await;
        assert!(
            db::instance::find_by_id(&env.pool, instance.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            env.api
                .complete_external_validation(authenticated(request.clone()))
                .await
                .unwrap()
                .into_inner(),
            closed
        );
        assert_eq!(read_hold(&env, hold.hold_id).await, awaiting);
        if outcome == rpc::ExternalValidationOutcome::Passed {
            assert_eq!(
                env.api
                    .complete_external_validation(authenticated(
                        rpc::CompleteExternalValidationRequest {
                            details: Some(String::new()),
                            ..request.clone()
                        }
                    ))
                    .await
                    .unwrap_err()
                    .code(),
                Code::AlreadyExists
            );
            assert_eq!(
                env.api
                    .complete_external_validation(authenticated(
                        rpc::CompleteExternalValidationRequest {
                            machine_id: Some(other.id),
                            ..request
                        }
                    ))
                    .await
                    .unwrap_err()
                    .code(),
                Code::NotFound
            );
        }
        if outcome == rpc::ExternalValidationOutcome::Passed {
            // A passing result remains readable after lifecycle later satisfies the hold.
            sqlx::query(
                "UPDATE external_validation_holds SET state = 'satisfied' WHERE hold_id = $1",
            )
            .bind(hold.hold_id)
            .execute(&env.pool)
            .await
            .unwrap();
        }
        assert_eq!(
            read_hold(&env, hold.hold_id).await.attempt,
            Some(closed.clone())
        );
        let replay = env
            .api
            .start_external_validation(authenticated(start_request(host.id, key)))
            .await
            .unwrap()
            .into_inner();
        assert!(replay.replayed);
        assert_eq!(replay.hold_id, Some(hold.hold_id));
        assert_eq!(replay.attempt, Some(closed));
    }
}

/// Unauthenticated or ungranted callers fail before any DB lookup, even when middleware is bypassed.
#[crate::sqlx_test]
async fn test_external_validation_authorization(_: PgPoolOptions, options: PgConnectOptions) {
    let env = create_test_env(PgPoolOptions::new().connect_with(options).await.unwrap()).await;
    env.pool.close().await;
    let principals = [
        None,
        Some(Principal::Anonymous),
        Some(Principal::TrustedCertificate),
        Some(Principal::SpiffeMachineIdentifier("host".into())),
        Some(Principal::SpiffeServiceIdentifier("unrelated".into())),
    ];
    for principal in principals {
        let expected = if principal.is_none() {
            Code::Unauthenticated
        } else {
            Code::PermissionDenied
        };
        fn request<T>(input: T, principal: &Option<Principal>) -> Request<T> {
            match principal {
                Some(principal) => with_principal(input, principal.clone()),
                None => Request::new(input),
            }
        }
        let statuses = [
            env.api
                .find_external_validation_hold_ids(request(Default::default(), &principal))
                .await
                .map(|_| ())
                .unwrap_err()
                .code(),
            env.api
                .find_external_validation_holds_by_ids(request(Default::default(), &principal))
                .await
                .map(|_| ())
                .unwrap_err()
                .code(),
            env.api
                .start_external_validation(request(Default::default(), &principal))
                .await
                .map(|_| ())
                .unwrap_err()
                .code(),
            env.api
                .cancel_external_validation(request(Default::default(), &principal))
                .await
                .map(|_| ())
                .unwrap_err()
                .code(),
            env.api
                .complete_external_validation(request(Default::default(), &principal))
                .await
                .map(|_| ())
                .unwrap_err()
                .code(),
        ];
        assert_eq!(statuses, [expected; 5], "{principal:?}");
    }
}

/// Persistence constraints prevent competing active gates and the same cycle from acquiring a new identity.
#[crate::sqlx_test]
async fn test_external_validation_durable_constraints(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new()
        .connect_with(options.clone())
        .await
        .unwrap();
    let env = create_test_env(pool).await;
    let host = create_managed_host(&env).await;
    let hold = seed_hold(&env, host.id, &tenant()).await;
    let mut txn = env.pool.begin().await.unwrap();
    let error = db::external_validation::create_hold(
        &mut txn,
        host.id,
        &tenant(),
        ExternalValidationCycleId::new(),
        SOURCE,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, db::DatabaseError::AlreadyFoundError { .. }));
    txn.rollback().await.unwrap();
    sqlx::query("UPDATE external_validation_holds SET state = 'satisfied' WHERE hold_id = $1")
        .bind(hold.hold_id)
        .execute(&env.pool)
        .await
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    assert!(matches!(
        db::external_validation::create_hold(
            &mut txn,
            host.id,
            &tenant(),
            hold.validation_cycle_id,
            SOURCE
        )
        .await
        .unwrap_err(),
        db::DatabaseError::AlreadyFoundError { .. }
    ));
    txn.rollback().await.unwrap();
    let next = seed_hold(&env, host.id, &tenant()).await;
    let key = Uuid::new_v4();
    let attempt = env
        .api
        .start_external_validation(authenticated(start_request(host.id, key)))
        .await
        .unwrap()
        .into_inner()
        .attempt
        .unwrap();
    let mut txn = env.pool.begin().await.unwrap();
    assert!(matches!(
        db::external_validation::open_attempt(&mut txn, &next, Uuid::new_v4(), Utc::now())
            .await
            .unwrap_err(),
        db::DatabaseError::AlreadyFoundError { .. }
    ));
    txn.rollback().await.unwrap();
    drop(host);
    env.pool.close().await;
    drop(env);
    // Shut down the fixture and its pool, then recover committed identities through a fresh pool.
    let reconnected = PgPoolOptions::new().connect_with(options).await.unwrap();
    let restored = db::external_validation::find_by_key(&reconnected, &tenant(), key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restored.hold.hold_id, next.hold_id);
    let restored = restored.attempt.unwrap();
    assert_eq!(restored.request_id, attempt.request_id.unwrap());
    assert_eq!(restored.state, AttemptState::Open);
    assert_eq!(
        db::external_validation::find_by_ids(&reconnected, &[next.hold_id], None)
            .await
            .unwrap()[0]
            .hold
            .state,
        HoldState::AttemptOpen
    );
}

/// A failed audit write rolls back both the attempt result and gate transition.
#[crate::sqlx_test]
async fn test_external_validation_audit_rollback(_: PgPoolOptions, options: PgConnectOptions) {
    let env = create_test_env(PgPoolOptions::new().connect_with(options).await.unwrap()).await;
    let host = create_managed_host(&env).await;
    let hold = seed_hold(&env, host.id, &tenant()).await;
    let id = env
        .api
        .start_external_validation(authenticated(start_request(host.id, Uuid::new_v4())))
        .await
        .unwrap()
        .into_inner()
        .attempt
        .unwrap()
        .request_id
        .unwrap();
    let before = read_hold(&env, hold.hold_id).await;
    sqlx::query("ALTER TABLE external_validation_audit ADD CONSTRAINT test_audit_failure CHECK (action <> 'cancel')")
        .execute(&env.pool).await.unwrap();
    let error = env
        .api
        .cancel_external_validation(authenticated(rpc::CancelExternalValidationRequest {
            request_id: Some(id),
            tenant_org_id: tenant(),
            details: "abandon".into(),
            machine_id: None,
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::Internal);
    assert_eq!(read_hold(&env, hold.hold_id).await, before);
    let audit_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM external_validation_audit WHERE request_id = $1")
            .bind(id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(audit_count, 1);
}

/// A saved UUID is insufficient when the live instance changed ownership or started deletion.
#[crate::sqlx_test]
async fn test_external_validation_instance_binding(_: PgPoolOptions, options: PgConnectOptions) {
    let env = create_test_env(PgPoolOptions::new().connect_with(options).await.unwrap()).await;
    let host = create_managed_host(&env).await;
    let other = create_managed_host(&env).await;
    let hold = seed_hold(&env, host.id, &tenant()).await;
    let id = env
        .api
        .start_external_validation(authenticated(start_request(host.id, Uuid::new_v4())))
        .await
        .unwrap()
        .into_inner()
        .attempt
        .unwrap()
        .request_id
        .unwrap();
    let segment = env.create_vpc_and_tenant_segment().await;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(segment)
        .build()
        .await;
    crate::tests::common::api_fixtures::tenant::create_fixture_tenant(&env, "different-org")
        .await
        .unwrap();
    sqlx::query("UPDATE external_validation_attempts SET allocation_state = 'created', validation_instance_id = $2 WHERE request_id = $1")
        .bind(id).bind(instance.id).execute(&env.pool).await.unwrap();
    let request = rpc::CompleteExternalValidationRequest {
        request_id: Some(id),
        tenant_org_id: tenant(),
        validation_instance_id: Some(instance.id),
        outcome: rpc::ExternalValidationOutcome::Passed.into(),
        details: None,
        machine_id: Some(host.id),
    };
    let before = read_hold(&env, hold.hold_id).await;
    for (label, machine, org, deleted) in [
        (
            "tenant ownership",
            host.id,
            "different-org".to_string(),
            None,
        ),
        ("machine ownership", other.id, tenant(), None),
        ("deleting instance", host.id, tenant(), Some(Utc::now())),
    ] {
        sqlx::query(
            "UPDATE instances SET machine_id = $2, tenant_org = $3, deleted = $4 WHERE id = $1",
        )
        .bind(instance.id)
        .bind(machine)
        .bind(org)
        .bind(deleted)
        .execute(&env.pool)
        .await
        .unwrap();
        assert_eq!(
            env.api
                .complete_external_validation(authenticated(request.clone()))
                .await
                .unwrap_err()
                .code(),
            Code::FailedPrecondition,
            "{label}"
        );
        assert_eq!(read_hold(&env, hold.hold_id).await, before, "{label}");
    }
    sqlx::query(
        "UPDATE instances SET machine_id = $2, tenant_org = $3, deleted = NULL WHERE id = $1",
    )
    .bind(instance.id)
    .bind(host.id)
    .bind(tenant())
    .execute(&env.pool)
    .await
    .unwrap();
    assert_eq!(
        env.api
            .complete_external_validation(authenticated(request))
            .await
            .unwrap()
            .into_inner()
            .state,
        i32::from(rpc::ExternalValidationAttemptState::Passed)
    );
}
