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

use std::error::Error;
use std::time::Duration;

use carbide_test_harness::prelude::{PgPool, TestHarness, sqlx_test};
use carbide_uuid::operating_system::OperatingSystemId;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    IpxeTemplateArtifact, IpxeTemplateArtifactCacheStrategy, IpxeTemplateArtifacts,
    OperatingSystemType, TenantState,
};
use tonic::Code;

#[sqlx_test]
async fn test_create_operating_system_ipxe(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "test-ipxe-os".to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: Some("inline iPXE OS".to_string()),
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: Some("cloud-init data".to_string()),
                ipxe_script: Some("chain --autofree https://boot.netboot.xyz".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap();

    let os = resp.into_inner();
    assert_eq!(os.name, "test-ipxe-os");
    assert_eq!(os.tenant_organization_id.as_deref(), Some("test-org"));
    assert_eq!(os.r#type, OperatingSystemType::OsTypeIpxe as i32);
    assert_eq!(os.description.as_deref(), Some("inline iPXE OS"));
    assert!(os.is_active);
    assert!(os.allow_override);
    assert!(!os.phone_home_enabled);
    assert_eq!(os.user_data.as_deref(), Some("cloud-init data"));
    assert_eq!(
        os.ipxe_script.as_deref(),
        Some("chain --autofree https://boot.netboot.xyz")
    );
    assert!(os.ipxe_template_id.is_none());
    assert!(os.id.is_some());
}

#[sqlx_test]
async fn test_create_operating_system_requires_name(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "".to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::InvalidArgument);
}

#[sqlx_test]
async fn test_create_operating_system_requires_variant(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "test-os".to_string(),
                tenant_organization_id: Some("test-org".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::InvalidArgument);
}

#[sqlx_test]
async fn test_create_operating_system_allows_omitted_org(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "no-org-os".to_string(),
                tenant_organization_id: None,
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap();

    let os = resp.into_inner();
    assert_eq!(os.name, "no-org-os");
    // An omitted org round-trips as an absent (None) wire field, not "".
    assert!(os.tenant_organization_id.is_none());
    assert!(os.id.is_some());
}

#[sqlx_test]
async fn test_create_operating_system_rejects_empty_org(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "empty-org-os".to_string(),
                tenant_organization_id: Some("".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::InvalidArgument);
}

#[sqlx_test]
async fn test_get_operating_system(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let created = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "get-test-os".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://boot.example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let id = created.id.unwrap();

    let fetched = env
        .api()
        .get_operating_system(tonic::Request::new(id))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.name, "get-test-os");
    assert_eq!(fetched.r#type, OperatingSystemType::OsTypeIpxe as i32);
}

#[sqlx_test]
async fn test_get_operating_system_not_found(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let id: OperatingSystemId = uuid::Uuid::nil().into();
    let resp = env
        .api()
        .get_operating_system(tonic::Request::new(id))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::NotFound);
}

// Renaming the table makes `get_operating_system` fail with PostgreSQL `42P01`.
// The shared status conversion must hide SQL diagnostics from clients while
// retaining the original `DatabaseError` behind its redacted source wrapper.
#[sqlx_test]
async fn test_get_operating_system_database_failure_hides_diagnostics_and_retains_source(
    pool: PgPool,
) {
    let env = TestHarness::builder(pool.clone()).build().await;

    sqlx::query("ALTER TABLE operating_systems RENAME TO unavailable_operating_systems")
        .execute(&pool)
        .await
        .expect("OS table should be unavailable for the handler query");

    let id: OperatingSystemId = uuid::Uuid::nil().into();
    let error = env
        .api()
        .get_operating_system(tonic::Request::new(id))
        .await
        .expect_err("the OS query must fail when its table is unavailable");

    assert_eq!(error.code(), Code::Internal);
    assert_eq!(error.message(), "internal database error");

    let source = error
        .source()
        .expect("database status source should be retained")
        .source()
        .expect("original database error should be retained")
        .downcast_ref::<db::DatabaseError>()
        .expect("status should retain the typed database error");
    let db::DatabaseError::Sqlx(annotated) = source else {
        panic!("expected the original SQLx error: {source:?}");
    };
    let driver_error = annotated
        .source
        .as_database_error()
        .expect("PostgreSQL error should be retained");
    assert_eq!(driver_error.code().as_deref(), Some("42P01"));
    let diagnostic = source.to_string();
    assert!(diagnostic.contains("FROM operating_systems"));
}

#[sqlx_test]
async fn test_update_operating_system(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let created = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "original-name".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: Some("original desc".to_string()),
                is_active: true,
                allow_override: false,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let id = created.id.unwrap();

    let updated = env
        .api()
        .update_operating_system(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemRequest {
                id: Some(id),
                name: Some("updated-name".to_string()),
                description: Some("updated desc".to_string()),
                is_active: Some(false),
                allow_override: Some(true),
                phone_home_enabled: Some(true),
                user_data: Some("new user-data".to_string()),
                ipxe_script: Some("chain http://updated.example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: None,
                ipxe_template_artifacts: None,
                ipxe_template_definition_hash: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(updated.name, "updated-name");
    assert_eq!(updated.description.as_deref(), Some("updated desc"));
    assert!(!updated.is_active);
    assert!(updated.allow_override);
    assert!(updated.phone_home_enabled);
    assert_eq!(updated.user_data.as_deref(), Some("new user-data"));
    assert_eq!(
        updated.ipxe_script.as_deref(),
        Some("chain http://updated.example.com")
    );
}

#[sqlx_test]
async fn test_delete_operating_system(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let created = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "delete-test-os".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let id = created.id.unwrap();

    let del_resp = env
        .api()
        .delete_operating_system(tonic::Request::new(id.into()))
        .await;
    assert!(del_resp.is_ok());

    let get_resp = env
        .api()
        .get_operating_system(tonic::Request::new(id))
        .await;
    assert!(get_resp.is_err());
    assert_eq!(get_resp.unwrap_err().code(), Code::NotFound);
}

#[sqlx_test]
async fn test_find_operating_system_ids(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let os1 = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "find-os-1".to_string(),
                tenant_organization_id: Some("find-org".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://one.example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let os2 = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "find-os-2".to_string(),
                tenant_organization_id: Some("find-org".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://two.example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let resp = env
        .api()
        .find_operating_system_ids(tonic::Request::new(
            rpc::forge::OperatingSystemSearchFilter {
                tenant_organization_id: Some("find-org".to_string()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let os1_id = os1.id.unwrap();
    let os2_id = os2.id.unwrap();
    assert!(resp.ids.contains(&os1_id));
    assert!(resp.ids.contains(&os2_id));
    assert_eq!(resp.ids.len(), 2);
}

#[sqlx_test]
async fn test_find_operating_systems_by_ids(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let os1 = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "by-id-os-1".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://one.example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let id1 = os1.id.unwrap();

    let resp = env
        .api()
        .find_operating_systems_by_ids(tonic::Request::new(
            rpc::forge::OperatingSystemsByIdsRequest { ids: vec![id1] },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.operating_systems.len(), 1);
    assert_eq!(resp.operating_systems[0].name, "by-id-os-1");
}

#[sqlx_test]
async fn test_list_ipxe_templates(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .list_ipxe_templates(tonic::Request::new(rpc::forge::ListIpxeTemplatesRequest {}))
        .await
        .unwrap()
        .into_inner();

    assert!(
        !resp.templates.is_empty(),
        "should have at least one embedded template"
    );
    for tmpl in &resp.templates {
        assert!(tmpl.id.is_some(), "template id must be set");
        assert!(!tmpl.name.is_empty());
        assert!(!tmpl.template.is_empty());
    }
}

#[sqlx_test]
async fn test_get_ipxe_template(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let all = env
        .api()
        .list_ipxe_templates(tonic::Request::new(rpc::forge::ListIpxeTemplatesRequest {}))
        .await
        .unwrap()
        .into_inner();

    let first = &all.templates[0];
    let first_id = first.id.expect("template id must be set");

    let resp = env
        .api()
        .get_ipxe_template(tonic::Request::new(rpc::forge::GetIpxeTemplateRequest {
            id: Some(first_id),
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(&resp.name, &first.name);
    assert_eq!(resp.id, Some(first_id));
    assert!(!resp.template.is_empty());
}

// ---------------------------------------------------------------------------
// Helpers shared by artifact tests
// ---------------------------------------------------------------------------

/// Creates an OS with a qcow-image template and two CACHED_ONLY artifacts plus
/// one CACHE_AS_NEEDED artifact, then returns the OS UUID string.
async fn create_os_with_artifacts(env: &TestHarness) -> OperatingSystemId {
    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "artifact-test-os".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: Some("ea756ddd-add3-5e42-a202-44bfc2d5aac2".parse().unwrap()),
                ipxe_template_parameters: vec![rpc::forge::IpxeTemplateParameter {
                    name: "image_url".to_string(),
                    value: "http://example.com/image.qcow2".to_string(),
                }],
                ipxe_template_artifacts: vec![
                    IpxeTemplateArtifact {
                        name: "kernel".to_string(),
                        url: "http://example.com/kernel".to_string(),
                        sha: None,
                        auth_type: None,
                        auth_token: None,
                        cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                        cached_url: None,
                    },
                    IpxeTemplateArtifact {
                        name: "initrd".to_string(),
                        url: "http://example.com/initrd".to_string(),
                        sha: None,
                        auth_type: None,
                        auth_token: None,
                        cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                        cached_url: None,
                    },
                    IpxeTemplateArtifact {
                        name: "overlay".to_string(),
                        url: "http://example.com/overlay".to_string(),
                        sha: None,
                        auth_type: None,
                        auth_token: None,
                        cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded as i32,
                        cached_url: None,
                    },
                ],
            },
        ))
        .await
        .unwrap()
        .into_inner();
    resp.id.unwrap()
}

#[sqlx_test]
async fn test_concurrent_operating_system_updates_preserve_committed_fields(pool: PgPool) {
    enum PendingUpdate {
        FullDefinition,
        CachedArtifact,
    }

    #[derive(Debug, PartialEq)]
    struct PersistedState {
        scenario: &'static str,
        name: String,
        is_active: bool,
        kernel_cached_url: Option<String>,
        initrd_cached_url: Option<String>,
        status: TenantState,
    }

    let cases = [
        (
            PendingUpdate::FullDefinition,
            PersistedState {
                scenario: "full definition resend",
                name: "renamed-os".to_string(),
                is_active: false,
                kernel_cached_url: Some("http://cache.local/kernel".to_string()),
                initrd_cached_url: None,
                status: TenantState::Provisioning,
            },
        ),
        (
            PendingUpdate::CachedArtifact,
            PersistedState {
                scenario: "cached artifact update",
                name: "artifact-test-os".to_string(),
                is_active: false,
                kernel_cached_url: Some("http://cache.local/kernel".to_string()),
                initrd_cached_url: Some("http://cache.local/initrd".to_string()),
                status: TenantState::Ready,
            },
        ),
    ];
    let env = TestHarness::builder(pool.clone()).build().await;
    let mut actual = Vec::new();
    let mut expected = Vec::new();

    for (pending_update, expected_state) in cases {
        let os_id = create_os_with_artifacts(&env).await;
        let definition = env
            .api()
            .get_operating_system(tonic::Request::new(os_id))
            .await
            .expect("OS fixture should be readable")
            .into_inner();
        let mut holder = pool.begin().await.expect("holder transaction should start");
        let holder_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *holder)
            .await
            .expect("holder backend PID should be available");
        let existing = db::operating_system::get_for_update(&mut holder, os_id.into())
            .await
            .expect("OS fixture should exist");
        let mut artifacts = existing
            .ipxe_artifacts
            .as_ref()
            .expect("OS fixture should have artifacts")
            .0
            .clone();
        artifacts[0]["cached_url"] = serde_json::json!("http://cache.local/kernel");

        // Hold the row while the RPC starts. Even a full definition resend must
        // retain cached URLs committed by this competing write.
        db::operating_system::update(
            &mut holder,
            &existing,
            &db::operating_system::UpdateOperatingSystem {
                id: os_id.into(),
                name: None,
                description: None,
                is_active: Some(false),
                allow_override: None,
                phone_home_enabled: None,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: None,
                ipxe_parameters: None,
                ipxe_artifacts: Some(artifacts),
                ipxe_definition_hash: None,
                status: None,
            },
        )
        .await
        .expect("competing OS write should succeed");

        let update = async {
            match pending_update {
                PendingUpdate::FullDefinition => env
                    .api()
                    .update_operating_system(tonic::Request::new(
                        rpc::forge::UpdateOperatingSystemRequest {
                            id: Some(os_id),
                            name: Some("renamed-os".to_string()),
                            ipxe_template_id: definition.ipxe_template_id,
                            ipxe_template_parameters: Some(rpc::forge::IpxeTemplateParameters {
                                items: definition.ipxe_template_parameters,
                            }),
                            ipxe_template_artifacts: Some(IpxeTemplateArtifacts {
                                items: definition.ipxe_template_artifacts,
                            }),
                            ..Default::default()
                        },
                    ))
                    .await
                    .map(|_| ()),
                PendingUpdate::CachedArtifact => env
                    .api()
                    .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
                        rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                            id: Some(os_id),
                            updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                                name: "initrd".to_string(),
                                cached_url: Some("http://cache.local/initrd".to_string()),
                            }],
                        },
                    ))
                    .await
                    .map(|_| ()),
            }
        };
        tokio::pin!(update);
        tokio::select! {
            result = &mut update => panic!(
                "{}: OS update completed before waiting for the competing write: {result:?}",
                expected_state.scenario
            ),
            blocked = tokio::time::timeout(
                Duration::from_secs(10),
                db::test_support::postgres::wait_for_blocked_query(
                    &pool, holder_pid, "operating_systems"
                ),
            ) => {
                blocked.unwrap_or_else(|error| panic!(
                    "{}: OS handler should wait for the competing write: {error}",
                    expected_state.scenario
                ));
            }
        }
        holder
            .commit()
            .await
            .expect("competing write should commit");

        tokio::time::timeout(Duration::from_secs(30), update)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{}: OS update should complete after the competing write commits: {error}",
                    expected_state.scenario
                )
            })
            .unwrap_or_else(|error| {
                panic!(
                    "{}: waiting OS update should succeed: {error}",
                    expected_state.scenario
                )
            });

        let fetched = env
            .api()
            .get_operating_system(tonic::Request::new(os_id))
            .await
            .expect("updated OS should be readable")
            .into_inner();
        actual.push(PersistedState {
            scenario: expected_state.scenario,
            name: fetched.name,
            is_active: fetched.is_active,
            kernel_cached_url: fetched.ipxe_template_artifacts[0].cached_url.clone(),
            initrd_cached_url: fetched.ipxe_template_artifacts[1].cached_url.clone(),
            status: TenantState::try_from(fetched.status).expect("OS status should be recognized"),
        });
        expected.push(expected_state);
    }

    assert_eq!(actual, expected);
}

// ---------------------------------------------------------------------------
// GetOperatingSystemCachableIpxeTemplateArtifacts tests
// ---------------------------------------------------------------------------

#[sqlx_test]
async fn test_get_operating_system_cachable_ipxe_template_artifacts_returns_ordered_list(
    pool: PgPool,
) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    let resp = env
        .api()
        .get_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::GetOperatingSystemCachableIpxeTemplateArtifactsRequest { id: Some(os_id) },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.artifacts.len(), 3);
    assert_eq!(resp.artifacts[0].name, "kernel");
    assert_eq!(resp.artifacts[1].name, "initrd");
    assert_eq!(resp.artifacts[2].name, "overlay");
}

#[sqlx_test]
async fn test_get_operating_system_cachable_ipxe_template_artifacts_not_found(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let id: OperatingSystemId = uuid::Uuid::nil().into();

    let resp = env
        .api()
        .get_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::GetOperatingSystemCachableIpxeTemplateArtifactsRequest { id: Some(id) },
        ))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::NotFound);
}

// ---------------------------------------------------------------------------
// UpdateOperatingSystemCachableIpxeTemplateArtifacts tests
// ---------------------------------------------------------------------------

#[sqlx_test]
async fn test_set_artifacts_cached_url_partial_update(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // Only update the first artifact (partial list).
    let resp = env
        .api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                    name: "kernel".to_string(),
                    cached_url: Some("http://cache.local/kernel".to_string()),
                }],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.artifacts.len(), 3);
    assert_eq!(
        resp.artifacts[0].cached_url.as_deref(),
        Some("http://cache.local/kernel")
    );
    assert!(resp.artifacts[1].cached_url.is_none()); // initrd unchanged
    assert!(resp.artifacts[2].cached_url.is_none()); // overlay unchanged
}

#[sqlx_test]
async fn test_set_artifacts_cached_url_ordered_duplicate_names(pool: PgPool) {
    // OS has two artifacts named "kernel". Updates must appear twice to set both.
    let env = TestHarness::builder(pool).build().await;

    let os = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "dup-kernel-os".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: Some("ea756ddd-add3-5e42-a202-44bfc2d5aac2".parse().unwrap()),
                ipxe_template_parameters: vec![rpc::forge::IpxeTemplateParameter {
                    name: "image_url".to_string(),
                    value: "http://example.com/image.qcow2".to_string(),
                }],
                ipxe_template_artifacts: vec![
                    IpxeTemplateArtifact {
                        name: "kernel".to_string(),
                        url: "http://example.com/kernel-a".to_string(),
                        sha: None,
                        auth_type: None,
                        auth_token: None,
                        cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                        cached_url: None,
                    },
                    IpxeTemplateArtifact {
                        name: "kernel".to_string(),
                        url: "http://example.com/kernel-b".to_string(),
                        sha: None,
                        auth_type: None,
                        auth_token: None,
                        cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                        cached_url: None,
                    },
                ],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let os_id = os.id.unwrap();

    // Two updates for "kernel" — each consumes the next unmatched occurrence.
    let resp = env
        .api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(),
                        cached_url: Some("http://cache.local/kernel-a".to_string()),
                    },
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(),
                        cached_url: Some("http://cache.local/kernel-b".to_string()),
                    },
                ],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        resp.artifacts[0].cached_url.as_deref(),
        Some("http://cache.local/kernel-a")
    );
    assert_eq!(
        resp.artifacts[1].cached_url.as_deref(),
        Some("http://cache.local/kernel-b")
    );
}

#[sqlx_test]
async fn test_set_artifacts_cached_url_too_many_same_name_fails(pool: PgPool) {
    // Only one "kernel" in the OS but two update entries → second should fail.
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    let resp = env
        .api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(),
                        cached_url: Some("http://cache.local/kernel-1".to_string()),
                    },
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(), // no second kernel exists
                        cached_url: Some("http://cache.local/kernel-2".to_string()),
                    },
                ],
            },
        ))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::NotFound);
}

#[sqlx_test]
async fn test_set_artifacts_cached_url_unknown_name_fails(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    let resp = env
        .api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                    name: "does-not-exist".to_string(),
                    cached_url: Some("http://cache.local/whatever".to_string()),
                }],
            },
        ))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::NotFound);
}

#[sqlx_test]
async fn test_set_artifacts_transitions_to_ready_when_all_cached_only_set(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // Set cached_url only for the two CACHED_ONLY artifacts; leave the
    // CACHE_AS_NEEDED "overlay" artifact untouched.
    let _ = env
        .api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(),
                        cached_url: Some("http://cache.local/kernel".to_string()),
                    },
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "initrd".to_string(),
                        cached_url: Some("http://cache.local/initrd".to_string()),
                    },
                ],
            },
        ))
        .await
        .unwrap();

    // Status should now be READY.
    let fetched = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(fetched.status, TenantState::Ready as i32);
}

#[sqlx_test]
async fn test_set_artifacts_does_not_transition_to_ready_when_cached_only_incomplete(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // Only set kernel; initrd (also CACHED_ONLY) is still missing.
    let _ = env
        .api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                    name: "kernel".to_string(),
                    cached_url: Some("http://cache.local/kernel".to_string()),
                }],
            },
        ))
        .await
        .unwrap();

    let fetched = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();

    assert_ne!(fetched.status, TenantState::Ready as i32);
}

#[sqlx_test]
async fn test_set_artifacts_cached_url_clear(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // Set then clear kernel's cached_url.
    env.api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                    name: "kernel".to_string(),
                    cached_url: Some("http://cache.local/kernel".to_string()),
                }],
            },
        ))
        .await
        .unwrap();

    let resp = env
        .api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                    name: "kernel".to_string(),
                    cached_url: None, // clear it
                }],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert!(resp.artifacts[0].cached_url.is_none());
}

#[sqlx_test]
async fn test_clear_cached_url_demotes_ready_to_provisioning(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // Set all CACHED_ONLY artifacts so the OS becomes READY.
    env.api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(),
                        cached_url: Some("http://cache.local/kernel".to_string()),
                    },
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "initrd".to_string(),
                        cached_url: Some("http://cache.local/initrd".to_string()),
                    },
                ],
            },
        ))
        .await
        .unwrap();

    let fetched = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(fetched.status, TenantState::Ready as i32);

    // Clear one CACHED_ONLY artifact's cached_url — status must revert.
    env.api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![rpc::forge::IpxeTemplateArtifactUpdateRequest {
                    name: "kernel".to_string(),
                    cached_url: None,
                }],
            },
        ))
        .await
        .unwrap();

    let fetched = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        fetched.status,
        TenantState::Provisioning as i32,
        "clearing a CACHED_ONLY artifact's cached_url must demote READY to PROVISIONING"
    );
}

// ---------------------------------------------------------------------------
// Cache ownership: create and changed definitions cannot install cached_url
// ---------------------------------------------------------------------------

#[sqlx_test]
async fn test_create_strips_cached_url(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "strip-test".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: Some("ea756ddd-add3-5e42-a202-44bfc2d5aac2".parse().unwrap()),
                ipxe_template_parameters: vec![rpc::forge::IpxeTemplateParameter {
                    name: "image_url".to_string(),
                    value: "http://example.com/image.qcow2".to_string(),
                }],
                ipxe_template_artifacts: vec![IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "http://example.com/kernel".to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                    cached_url: Some("http://sneaky.local/kernel".to_string()),
                }],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let os_id = resp.id.unwrap();
    let arts = env
        .api()
        .get_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::GetOperatingSystemCachableIpxeTemplateArtifactsRequest { id: Some(os_id) },
        ))
        .await
        .unwrap()
        .into_inner();

    assert!(
        arts.artifacts[0].cached_url.is_none(),
        "cached_url must be stripped on create"
    );
}

#[sqlx_test]
async fn test_create_with_cached_only_sets_provisioning(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "provision-test".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: Some("ea756ddd-add3-5e42-a202-44bfc2d5aac2".parse().unwrap()),
                ipxe_template_parameters: vec![rpc::forge::IpxeTemplateParameter {
                    name: "image_url".to_string(),
                    value: "http://example.com/image.qcow2".to_string(),
                }],
                ipxe_template_artifacts: vec![IpxeTemplateArtifact {
                    name: "kernel".to_string(),
                    url: "http://example.com/kernel".to_string(),
                    sha: None,
                    auth_type: None,
                    auth_token: None,
                    cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                    cached_url: None,
                }],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        resp.status,
        TenantState::Provisioning as i32,
        "OS with CACHED_ONLY artifact and no cached_url must start as PROVISIONING"
    );
}

// Unchanged definitions retain the site's cache; caller-supplied URLs cannot fill it.
#[sqlx_test]
async fn test_update_preserves_cached_urls_only_for_unchanged_definitions(pool: PgPool) {
    enum DefinitionUpdate {
        Unchanged,
        PartialCacheWithSuppliedUrls,
        StaleStoredHash,
        Template,
        ArtifactOrder,
        CacheStrategy,
        Parameters,
        ParametersWithStaleHash,
        EmptyArtifacts,
    }

    struct Case {
        name: &'static str,
        change: DefinitionUpdate,
        status: TenantState,
        keep_cache: bool,
        hash_changes: bool,
    }

    let env = TestHarness::builder(pool).build().await;
    for case in [
        Case {
            name: "unchanged REST resend",
            change: DefinitionUpdate::Unchanged,
            status: TenantState::Ready,
            keep_cache: true,
            hash_changes: false,
        },
        Case {
            name: "supplied URLs cannot complete a partial cache",
            change: DefinitionUpdate::PartialCacheWithSuppliedUrls,
            status: TenantState::Provisioning,
            keep_cache: true,
            hash_changes: false,
        },
        Case {
            name: "repair stale stored hash",
            change: DefinitionUpdate::StaleStoredHash,
            status: TenantState::Ready,
            keep_cache: true,
            hash_changes: true,
        },
        Case {
            name: "changed cache strategy",
            change: DefinitionUpdate::CacheStrategy,
            status: TenantState::Provisioning,
            keep_cache: false,
            hash_changes: false,
        },
        Case {
            name: "changed template",
            change: DefinitionUpdate::Template,
            status: TenantState::Provisioning,
            keep_cache: false,
            hash_changes: true,
        },
        Case {
            name: "reordered artifacts with unchanged hash",
            change: DefinitionUpdate::ArtifactOrder,
            status: TenantState::Provisioning,
            keep_cache: false,
            hash_changes: false,
        },
        Case {
            name: "changed parameters",
            change: DefinitionUpdate::Parameters,
            status: TenantState::Provisioning,
            keep_cache: false,
            hash_changes: true,
        },
        Case {
            name: "changed parameters with stale hash and omitted artifacts",
            change: DefinitionUpdate::ParametersWithStaleHash,
            status: TenantState::Provisioning,
            keep_cache: false,
            hash_changes: false,
        },
        Case {
            name: "explicit artifact removal with stale hash",
            change: DefinitionUpdate::EmptyArtifacts,
            status: TenantState::Ready,
            keep_cache: false,
            hash_changes: false,
        },
    ] {
        let os_id = create_os_with_artifacts(&env).await;
        if matches!(
            case.change,
            DefinitionUpdate::StaleStoredHash | DefinitionUpdate::Template
        ) {
            let original = env
                .api()
                .get_operating_system(tonic::Request::new(os_id))
                .await
                .unwrap()
                .into_inner();
            let mut parameters = original.ipxe_template_parameters;
            let supplied_hash = if matches!(case.change, DefinitionUpdate::StaleStoredHash) {
                // Older REST updates could save a new definition with its old hash.
                parameters[0].value = "http://example.com/replacement.qcow2".to_string();
                original.ipxe_template_definition_hash
            } else {
                // Both templates must accept the same parameters for a template-only change.
                parameters.push(rpc::forge::IpxeTemplateParameter {
                    name: "kernel_params".to_string(),
                    value: "console=tty0".to_string(),
                });
                None
            };
            env.api()
                .update_operating_system(tonic::Request::new(
                    rpc::forge::UpdateOperatingSystemRequest {
                        id: Some(os_id),
                        ipxe_template_parameters: Some(rpc::forge::IpxeTemplateParameters {
                            items: parameters,
                        }),
                        ipxe_template_definition_hash: supplied_hash,
                        ..Default::default()
                    },
                ))
                .await
                .unwrap();
        }
        let cached_artifacts: &[&str] =
            if matches!(case.change, DefinitionUpdate::PartialCacheWithSuppliedUrls) {
                &["kernel"]
            } else {
                &["kernel", "initrd", "overlay"]
            };
        env.api()
            .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
                rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                    id: Some(os_id),
                    updates: cached_artifacts
                        .iter()
                        .map(|artifact| rpc::forge::IpxeTemplateArtifactUpdateRequest {
                            name: artifact.to_string(),
                            cached_url: Some(format!("http://cache.local/{artifact}")),
                        })
                        .collect(),
                },
            ))
            .await
            .unwrap();
        let stored = env
            .api()
            .get_operating_system(tonic::Request::new(os_id))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(
            stored.status,
            if cached_artifacts.len() == 1 {
                TenantState::Provisioning
            } else {
                TenantState::Ready
            } as i32,
            "{}: initial cache state",
            case.name
        );
        let mut request = rpc::forge::UpdateOperatingSystemRequest {
            id: Some(os_id),
            name: Some(case.name.to_string()),
            ipxe_template_id: stored.ipxe_template_id,
            ipxe_template_parameters: Some(rpc::forge::IpxeTemplateParameters {
                items: stored.ipxe_template_parameters.clone(),
            }),
            ipxe_template_artifacts: Some(IpxeTemplateArtifacts {
                items: stored.ipxe_template_artifacts.clone(),
            }),
            ..Default::default()
        };
        for artifact in &mut request.ipxe_template_artifacts.as_mut().unwrap().items {
            artifact.cached_url =
                if matches!(case.change, DefinitionUpdate::PartialCacheWithSuppliedUrls) {
                    Some("http://untrusted.local/artifact".to_string())
                } else {
                    None
                };
        }
        match case.change {
            DefinitionUpdate::Unchanged
            | DefinitionUpdate::PartialCacheWithSuppliedUrls
            | DefinitionUpdate::StaleStoredHash => {}
            DefinitionUpdate::Template => {
                request.ipxe_template_id =
                    Some("c4b1d4f6-69ba-5f55-90cd-ab2acd002475".parse().unwrap());
            }
            DefinitionUpdate::ArtifactOrder => {
                request
                    .ipxe_template_artifacts
                    .as_mut()
                    .unwrap()
                    .items
                    .swap(0, 1);
            }
            DefinitionUpdate::CacheStrategy => {
                request.ipxe_template_artifacts.as_mut().unwrap().items[2].cache_strategy =
                    IpxeTemplateArtifactCacheStrategy::CachedOnly as i32;
            }
            DefinitionUpdate::Parameters | DefinitionUpdate::ParametersWithStaleHash => {
                request.ipxe_template_parameters.as_mut().unwrap().items[0].value =
                    "http://example.com/new-image.qcow2".to_string();
                if matches!(case.change, DefinitionUpdate::ParametersWithStaleHash) {
                    request.ipxe_template_definition_hash =
                        stored.ipxe_template_definition_hash.clone();
                    request.ipxe_template_artifacts = None;
                }
            }
            DefinitionUpdate::EmptyArtifacts => {
                request
                    .ipxe_template_artifacts
                    .as_mut()
                    .unwrap()
                    .items
                    .clear();
                request.ipxe_template_definition_hash =
                    stored.ipxe_template_definition_hash.clone();
            }
        }
        let expected_params = request
            .ipxe_template_parameters
            .as_ref()
            .unwrap()
            .items
            .clone();
        let expected_template = request.ipxe_template_id;
        let mut expected_artifacts = request
            .ipxe_template_artifacts
            .as_ref()
            .map(|wrapper| wrapper.items.clone())
            .unwrap_or_else(|| stored.ipxe_template_artifacts.clone());
        if case.keep_cache {
            expected_artifacts = stored.ipxe_template_artifacts.clone();
        } else {
            for artifact in &mut expected_artifacts {
                artifact.cached_url = None;
            }
        }

        env.api()
            .update_operating_system(tonic::Request::new(request))
            .await
            .unwrap();
        let persisted = env
            .api()
            .get_operating_system(tonic::Request::new(os_id))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(persisted.name, case.name);
        assert_eq!(
            persisted.ipxe_template_id, expected_template,
            "{}",
            case.name
        );
        assert_eq!(persisted.status, case.status as i32, "{}", case.name);
        assert_eq!(
            persisted.ipxe_template_parameters, expected_params,
            "{}",
            case.name
        );
        assert_eq!(
            persisted.ipxe_template_artifacts, expected_artifacts,
            "{}",
            case.name
        );
        assert_eq!(
            persisted.ipxe_template_definition_hash != stored.ipxe_template_definition_hash,
            case.hash_changes,
            "{}: hash changes",
            case.name
        );
    }
}

// A supplied hash can bypass validation. Omitting it must validate the stored
// definition before applying even a metadata change.
#[sqlx_test]
async fn test_update_without_hash_rejects_invalid_stored_definition(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;
    let original = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    let invalid = env
        .api()
        .update_operating_system(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemRequest {
                id: Some(os_id),
                ipxe_template_parameters: Some(rpc::forge::IpxeTemplateParameters {
                    items: vec![],
                }),
                ipxe_template_definition_hash: original.ipxe_template_definition_hash,
                ..Default::default()
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let error = env
        .api()
        .update_operating_system(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemRequest {
                id: Some(os_id),
                name: Some("must-not-be-saved".to_string()),
                ..Default::default()
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::InvalidArgument);

    let persisted = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(persisted, invalid);
}

#[sqlx_test]
async fn test_changed_artifact_list_discards_stored_and_supplied_cached_urls(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // Cache the existing definition, which includes an overlay artifact.
    env.api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(),
                        cached_url: Some("http://cache.local/kernel".to_string()),
                    },
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "initrd".to_string(),
                        cached_url: Some("http://cache.local/initrd".to_string()),
                    },
                ],
            },
        ))
        .await
        .unwrap();

    // Removing the overlay changes the definition, even though kernel/initrd match.
    env.api()
        .update_operating_system(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemRequest {
                id: Some(os_id),
                name: None,
                description: None,
                is_active: None,
                allow_override: None,
                phone_home_enabled: None,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: None,
                ipxe_template_parameters: None,
                ipxe_template_artifacts: Some(IpxeTemplateArtifacts {
                    items: vec![
                        IpxeTemplateArtifact {
                            name: "kernel".to_string(),
                            url: "http://example.com/kernel".to_string(),
                            sha: None,
                            auth_type: None,
                            auth_token: None,
                            cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                            cached_url: Some("http://sneaky.local/kernel".to_string()),
                        },
                        IpxeTemplateArtifact {
                            name: "initrd".to_string(),
                            url: "http://example.com/initrd".to_string(),
                            sha: None,
                            auth_type: None,
                            auth_token: None,
                            cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                            cached_url: Some("http://sneaky.local/initrd".to_string()),
                        },
                    ],
                }),
                ipxe_template_definition_hash: None,
            },
        ))
        .await
        .unwrap();

    let arts = env
        .api()
        .get_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::GetOperatingSystemCachableIpxeTemplateArtifactsRequest { id: Some(os_id) },
        ))
        .await
        .unwrap()
        .into_inner();

    for a in &arts.artifacts {
        assert!(
            a.cached_url.is_none(),
            "cached_url for '{}' must be cleared when the definition changes",
            a.name
        );
    }
}

#[sqlx_test]
async fn test_update_with_cached_only_artifacts_recomputes_status(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // Set all cached_urls so OS becomes READY.
    env.api()
        .update_operating_system_cachable_ipxe_template_artifacts(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemIpxeTemplateArtifactRequest {
                id: Some(os_id),
                updates: vec![
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "kernel".to_string(),
                        cached_url: Some("http://cache.local/kernel".to_string()),
                    },
                    rpc::forge::IpxeTemplateArtifactUpdateRequest {
                        name: "initrd".to_string(),
                        cached_url: Some("http://cache.local/initrd".to_string()),
                    },
                ],
            },
        ))
        .await
        .unwrap();

    let ready_os = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ready_os.status, TenantState::Ready as i32);

    // Removing overlay changes the definition, so cached URLs are invalidated
    // and the remaining CACHED_ONLY artifacts need provisioning again.
    env.api()
        .update_operating_system(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemRequest {
                id: Some(os_id),
                name: None,
                description: None,
                is_active: None,
                allow_override: None,
                phone_home_enabled: None,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: None,
                ipxe_template_parameters: None,
                ipxe_template_artifacts: Some(IpxeTemplateArtifacts {
                    items: vec![
                        IpxeTemplateArtifact {
                            name: "kernel".to_string(),
                            url: "http://example.com/kernel".to_string(),
                            sha: None,
                            auth_type: None,
                            auth_token: None,
                            cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                            cached_url: None,
                        },
                        IpxeTemplateArtifact {
                            name: "initrd".to_string(),
                            url: "http://example.com/initrd".to_string(),
                            sha: None,
                            auth_type: None,
                            auth_token: None,
                            cache_strategy: IpxeTemplateArtifactCacheStrategy::CachedOnly as i32,
                            cached_url: None,
                        },
                    ],
                }),
                ipxe_template_definition_hash: None,
            },
        ))
        .await
        .unwrap();

    let updated_os = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        updated_os.status,
        TenantState::Provisioning as i32,
        "status must revert to PROVISIONING when CACHED_ONLY artifacts lack cached_url"
    );
}

#[sqlx_test]
async fn test_update_promotes_to_ready_when_no_cached_only_remains(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;
    let os_id = create_os_with_artifacts(&env).await;

    // OS starts as PROVISIONING (has CACHED_ONLY artifacts without cached_url).
    let os = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(os.status, TenantState::Provisioning as i32);

    // Update artifacts to remove all CACHED_ONLY strategies — only CACHE_AS_NEEDED remains.
    env.api()
        .update_operating_system(tonic::Request::new(
            rpc::forge::UpdateOperatingSystemRequest {
                id: Some(os_id),
                name: None,
                description: None,
                is_active: None,
                allow_override: None,
                phone_home_enabled: None,
                user_data: None,
                ipxe_script: None,
                ipxe_template_id: None,
                ipxe_template_parameters: None,
                ipxe_template_artifacts: Some(IpxeTemplateArtifacts {
                    items: vec![
                        IpxeTemplateArtifact {
                            name: "kernel".to_string(),
                            url: "http://example.com/kernel".to_string(),
                            sha: None,
                            auth_type: None,
                            auth_token: None,
                            cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded as i32,
                            cached_url: None,
                        },
                        IpxeTemplateArtifact {
                            name: "initrd".to_string(),
                            url: "http://example.com/initrd".to_string(),
                            sha: None,
                            auth_type: None,
                            auth_token: None,
                            cache_strategy: IpxeTemplateArtifactCacheStrategy::CacheAsNeeded as i32,
                            cached_url: None,
                        },
                    ],
                }),
                ipxe_template_definition_hash: None,
            },
        ))
        .await
        .unwrap();

    let updated_os = env
        .api()
        .get_operating_system(tonic::Request::new(os_id))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        updated_os.status,
        TenantState::Ready as i32,
        "status must promote to READY when no CACHED_ONLY artifacts remain"
    );
}

// ---------------------------------------------------------------------------
// End compliance tests
// ---------------------------------------------------------------------------

#[sqlx_test]
async fn test_get_ipxe_template_not_found(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let nonexistent_id = carbide_uuid::ipxe_template::IpxeTemplateId::nil();
    let resp = env
        .api()
        .get_ipxe_template(tonic::Request::new(rpc::forge::GetIpxeTemplateRequest {
            id: Some(nonexistent_id),
        }))
        .await;

    assert!(resp.is_err());
    assert_eq!(resp.unwrap_err().code(), Code::NotFound);
}

#[sqlx_test]
async fn test_create_operating_system_with_explicit_id(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let explicit_id = uuid::Uuid::new_v4();
    let id_proto: OperatingSystemId = explicit_id.into();

    let resp = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: Some(id_proto),
                name: "explicit-id-os".to_string(),
                tenant_organization_id: Some("org1".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(uuid::Uuid::from(resp.id.unwrap()), explicit_id);
}

#[sqlx_test]
async fn test_deleted_os_not_returned_by_find_ids(pool: PgPool) {
    let env = TestHarness::builder(pool).build().await;

    let created = env
        .api()
        .create_operating_system(tonic::Request::new(
            rpc::forge::CreateOperatingSystemRequest {
                id: None,
                name: "soon-deleted-os".to_string(),
                tenant_organization_id: Some("del-org".to_string()),
                description: None,
                is_active: true,
                allow_override: true,
                phone_home_enabled: false,
                user_data: None,
                ipxe_script: Some("chain http://example.com".to_string()),
                ipxe_template_id: None,
                ipxe_template_parameters: vec![],
                ipxe_template_artifacts: vec![],
            },
        ))
        .await
        .unwrap()
        .into_inner();

    let id = created.id.unwrap();
    env.api()
        .delete_operating_system(tonic::Request::new(id.into()))
        .await
        .unwrap();

    let resp = env
        .api()
        .find_operating_system_ids(tonic::Request::new(
            rpc::forge::OperatingSystemSearchFilter {
                tenant_organization_id: Some("del-org".to_string()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert!(resp.ids.is_empty());
}
