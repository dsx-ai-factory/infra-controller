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
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ::rpc::forge::dpu_extension_service_observability_config::Config;
use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::{
    self as rpc, DpuExtensionServiceObservabilityConfig,
    DpuExtensionServiceObservabilityConfigLogging,
};
use carbide_dpf::{
    DetachedDpuServiceDefinition, DpfError, DpuServiceHelmChartObservation, DpuServiceObservation,
};
use carbide_extension_service_controller::dpu_service::{
    dpu_service_mutable_patch, project_dpu_service,
};
use carbide_extension_service_controller::io::ExtensionServiceStateControllerIO;
use carbide_instrument::testing::MetricsCapture;
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::ConfigVersion;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::extension_service::{ExtensionServiceLifecycleState, ExtensionServiceType};
use model::tenant::TenantOrganizationId;
use state_controller::controller::Enqueuer;
use state_controller::io::StateControllerIO;
use tonic::Request;
use uuid::Uuid;

use crate::api::Api;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_test_env,
    create_test_env_with_overrides, get_config,
};

const TEST_SERVICE_DATA: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27";
const TEST_SERVICE_DATA_VERSION_2: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: version-2\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27";
const TEST_SERVICE_DATA_VERSION_3: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: version-3\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27";
const TEST_DPF_HELM_CHART_SERVICE_DATA: &str = r#"{
  "repoURL": "oci://registry.example.com/charts",
  "chartName": "tenant-service",
  "chartVersion": "1.2.3",
  "security.privileged": false
}"#;
const TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2: &str = r#"{
  "repoURL": "oci://registry.example.com/charts",
  "chartName": "tenant-service",
  "chartVersion": "2.0.0",
  "security.privileged": true,
  "values": {"replicas": 2}
}"#;
const CREDENTIAL_CLEANUP_FAILURE_METRIC: &str =
    "carbide_extension_service_credential_cleanup_failures_total";

fn create_credential() -> rpc::DpuExtensionServiceCredential {
    rpc::DpuExtensionServiceCredential {
        registry_url: "https://registry.test.com".to_string(),
        r#type: Some(
            rpc::dpu_extension_service_credential::Type::UsernamePassword(rpc::UsernamePassword {
                username: "test-username".to_string(),
                password: "test-password".to_string(),
            }),
        ),
    }
}

fn create_observability() -> rpc::DpuExtensionServiceObservability {
    rpc::DpuExtensionServiceObservability {
        configs: vec![rpc::DpuExtensionServiceObservabilityConfig {
            name: Some("prom_config".to_string()),
            config: Some(
                rpc::dpu_extension_service_observability_config::Config::Prometheus(
                    rpc::DpuExtensionServiceObservabilityConfigPrometheus {
                        scrape_interval_seconds: 1,
                        endpoint: "localhost:7777".to_string(),
                    },
                ),
            ),
        }],
    }
}

fn lifecycle_state(service: &rpc::DpuExtensionService) -> String {
    let lifecycle = service
        .lifecycle_status
        .as_ref()
        .expect("extension service response includes lifecycle status");
    serde_json::from_str::<serde_json::Value>(&lifecycle.state)
        .expect("lifecycle state is JSON")
        .get("state")
        .and_then(serde_json::Value::as_str)
        .expect("lifecycle JSON contains state")
        .to_string()
}

async fn create_test_tenants(env: &TestEnv) -> Result<(), eyre::Report> {
    let _ = env
        .api
        .create_tenant(tonic::Request::new(rpc::CreateTenantRequest {
            organization_id: "best_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "best_org".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    let _ = env
        .api
        .create_tenant(tonic::Request::new(rpc::CreateTenantRequest {
            organization_id: "another_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "another_org".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    Ok(())
}

async fn create_dpf_enabled_test_env(db_pool: sqlx::PgPool) -> TestEnv {
    let mut config = get_config();
    config.dpf.enabled = true;
    create_test_env_with_overrides(db_pool, TestEnvOverrides::with_config(config)).await
}

async fn create_dpf_controller_test_env(
    db_pool: sqlx::PgPool,
    dpf_sdk: MockDpfOperations,
) -> TestEnv {
    let mut config = get_config();
    config.dpf.enabled = true;
    let dpf_sdk: Arc<dyn DpfOperations> = Arc::new(dpf_sdk);
    create_test_env_with_overrides(
        db_pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(dpf_sdk),
    )
    .await
}

fn dpu_service_observation(service: &DetachedDpuServiceDefinition) -> DpuServiceObservation {
    DpuServiceObservation {
        name: Some(service.name.clone()),
        namespace: Some(service.namespace.clone()),
        labels: service.labels.clone(),
        helm_chart: DpuServiceHelmChartObservation {
            repo_url: service.helm_chart.repo_url.clone(),
            chart: Some(service.helm_chart.chart.clone()),
            version: service.helm_chart.version.clone(),
            release_name: Some(service.helm_chart.release_name.clone()),
            values: service.helm_chart.values.clone(),
        },
        deploy_in_cluster: Some(service.deploy_in_cluster),
        dpu_cluster_selector_present: false,
        interfaces_present: false,
        paused: None,
        security_privileged: Some(service.security_privileged),
        service_daemon_set_node_selector: Some(serde_json::json!({
            "nodeSelectorTerms": [{
                "matchExpressions": service.node_selector_labels.iter().map(|(key, value)| serde_json::json!({
                    "key": key,
                    "operator": "In",
                    "values": [value],
                })).collect::<Vec<_>>(),
            }],
        })),
        service_id: None,
        config_ports_present: false,
        is_deleting: false,
    }
}

async fn create_test_extension_service_and_tenants(
    env: &TestEnv,
) -> Result<rpc::DpuExtensionService, eyre::Report> {
    create_test_tenants(env).await?;
    create_test_extension_service(&env.api, "test-service", None).await
}

async fn create_test_extension_service(
    api: &Api,
    name: &str,
    credential: Option<rpc::DpuExtensionServiceCredential>,
) -> Result<rpc::DpuExtensionService, eyre::Report> {
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: name.to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential,
        observability: None,
    };

    let create_resp = api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;

    Ok(create_resp?.into_inner())
}

async fn create_test_extension_service_with_three_versions(
    env: &TestEnv,
) -> Result<rpc::DpuExtensionService, eyre::Report> {
    create_test_tenants(env).await?;
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_ok());

    let service_id = create_resp.unwrap().into_inner().service_id;

    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: Some(1),
        }))
        .await;
    assert!(update_resp.is_ok());

    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_3.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: Some(2),
        }))
        .await;
    assert!(update_resp.is_ok());

    Ok(update_resp.unwrap().into_inner())
}

async fn create_test_extension_service_with_ten_versions(
    env: &TestEnv,
) -> Result<String, eyre::Report> {
    create_test_tenants(env).await?;
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_ok());

    let service_id = create_resp.unwrap().into_inner().service_id;

    for _ in 0..5 {
        // Since the extension service data/credential cannot be unchanged, we need to update it to a different version
        let update_resp = env
            .api
            .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
                service_id: service_id.clone(),
                service_name: None,
                description: None,
                data: TEST_SERVICE_DATA_VERSION_2.to_string(),
                credential: None,
                observability: None,

                if_version_ctr_match: None,
            }))
            .await;
        assert!(update_resp.is_ok());

        let update_resp = env
            .api
            .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
                service_id: service_id.clone(),
                service_name: None,
                description: None,
                data: TEST_SERVICE_DATA.to_string(),
                credential: None,
                observability: None,

                if_version_ctr_match: None,
            }))
            .await;
        assert!(update_resp.is_ok());
    }

    Ok(service_id)
}

async fn get_credentials_for_extension_service(
    env: &TestEnv,
    extension_service: &rpc::DpuExtensionService,
) -> Result<Credentials, eyre::Report> {
    // Verify the credential is stored correctly in Vault
    let credential_key = carbide_secrets::credentials::CredentialKey::ExtensionService {
        service_id: extension_service.service_id.clone(),
        version: extension_service
            .latest_version_info
            .as_ref()
            .unwrap()
            .version
            .clone()
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr()
            .to_string(),
    };

    let stored_credential = env
        .api
        .credential_manager
        .get_credentials(&credential_key)
        .await?;
    stored_credential.ok_or_else(|| eyre::eyre!("could not find the credential"))
}

#[crate::sqlx_test]
async fn test_extension_service_creation(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(create_observability()),
    };

    let create_resp: Result<tonic::Response<rpc::DpuExtensionService>, tonic::Status> = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;

    assert!(create_resp.is_ok());

    let service = create_resp.unwrap().into_inner();
    assert_eq!(lifecycle_state(&service), "ready");

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_extension_service_is_rejected_when_dpf_is_disabled(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let response = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await
        .expect_err("DPF helm chart extension services require DPF to be enabled for this site");

    assert_eq!(response.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        response.message(),
        "DPF helm chart extension services require DPF to be enabled for this site"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_persists_normalized_creating_state_without_dpf_call(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    // Deliberately use non-contract field order and whitespace. The API must
    // persist the parsed model's normalized representation, rather than this
    // caller-provided encoding.
    let data = r#"{
        "security.privileged": false,
        "chartVersion": "1.2.3",
        "repoURL": "oci://registry.example.com/charts",
        "chartName": "tenant-service"
    }"#;
    let response = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "accepted-dpf-service".to_string(),
            description: Some("durable acceptance only".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: data.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();

    let service_id: ExtensionServiceId = response.service_id.parse()?;
    assert_eq!(lifecycle_state(&response), "creating");
    let latest_version = response
        .latest_version_info
        .as_ref()
        .expect("initial version is returned");
    assert_eq!(
        response.active_versions,
        vec![latest_version.version.clone()]
    );
    assert_eq!(
        latest_version
            .version
            .parse::<ConfigVersion>()?
            .version_nr(),
        1
    );
    assert_eq!(
        latest_version.data,
        model::extension_service::DpfHelmChartServiceData::parse(data)?.normalized_json()?
    );

    // The handler committed the sole V1 and controller state, but did not run
    // or call the controller. That proves create is durable acceptance only;
    // the periodic controller scan performs the later DPF work.
    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("committed DPF Helm service is controller-visible");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(record.status.controller_state_outcome.is_none());
    let version = db::extension_service::find_version_info(&mut txn, service_id, None).await?;
    assert!(!version.has_credential);
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    assert_eq!(history.len(), 1);
    assert_eq!(
        serde_json::from_str::<ExtensionServiceLifecycleState>(&history[0].state)?,
        ExtensionServiceLifecycleState::Creating
    );
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_update_replaces_v1_and_requests_reconciliation(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let initial_data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let initial_projection = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &initial_data);
    let updated_data = model::extension_service::DpfHelmChartServiceData::parse(
        TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2,
    )?;
    let updated_projection = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &updated_data);
    let existing = dpu_service_observation(&initial_projection);
    let expected_name = updated_projection.name.clone();
    let expected_patch = dpu_service_mutable_patch(
        &updated_projection,
        initial_projection.helm_chart.values.as_ref(),
    );

    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    mock.expect_get_dpu_service()
        .times(1)
        .withf({
            let expected_name = expected_name.clone();
            move |name| name == expected_name.as_str()
        })
        .returning(move |_| Ok(Some(existing.clone())));
    mock.expect_patch_dpu_service()
        .times(1)
        .withf(move |name, patch| name == expected_name.as_str() && patch == &expected_patch)
        .returning(|_, _| Ok(()));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: Some(service_id.to_string()),
            service_name: "update-dpf-service".to_string(),
            description: Some("before update".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    assert_eq!(created.service_id, service_id.to_string());

    // The existing create reconciler reaches Ready before this API-only
    // component accepts a replacement definition.
    env.run_extension_service_controller_iteration().await;

    let stale_update = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(0),
        }))
        .await
        .expect_err("a stale API revision must be rejected before replacing V1");
    assert_eq!(stale_update.code(), tonic::Code::FailedPrecondition);

    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            service_name: Some("updated-dpf-service".to_string()),
            description: Some("after update".to_string()),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(1),
        }))
        .await?
        .into_inner();

    assert_eq!(updated.version_ctr, 2);
    assert_eq!(lifecycle_state(&updated), "updating");
    let latest_version = updated
        .latest_version_info
        .as_ref()
        .expect("stable V1 is returned");
    assert_eq!(
        updated.active_versions,
        vec![latest_version.version.clone()]
    );
    assert_eq!(
        latest_version
            .version
            .parse::<ConfigVersion>()?
            .version_nr(),
        1
    );
    assert_eq!(
        latest_version.data,
        model::extension_service::DpfHelmChartServiceData::parse(
            TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2
        )?
        .normalized_json()?
    );

    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("updated DPF Helm service remains controller-visible");
    assert_eq!(record.name, "updated-dpf-service");
    assert_eq!(record.version_ctr, 2);
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert_eq!(
        db::extension_service::find_all_versions(&env.pool, service_id).await?,
        vec![latest_version.version.parse()?]
    );
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    assert_eq!(history.len(), 3);
    assert_eq!(
        serde_json::from_str::<ExtensionServiceLifecycleState>(&history[2].state)?,
        ExtensionServiceLifecycleState::Updating
    );
    txn.commit().await?;

    // A second data update is rejected before it can change V1 because DPF
    // reconciliation of the first replacement has not completed yet.
    let error = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(2),
        }))
        .await
        .expect_err("data updates must wait for Ready");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("only be updated while ready"));

    // The controller reads the old owned object, patches only the mutable
    // DPUService material, and records the in-place revision as Ready.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("updated DPF Helm service remains controller-visible");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(
        db::extension_service::find_all_versions(&env.pool, service_id).await?,
        vec![latest_version.version.parse()?]
    );
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    assert_eq!(history.len(), 4);
    assert_eq!(
        serde_json::from_str::<ExtensionServiceLifecycleState>(&history[3].state)?,
        ExtensionServiceLifecycleState::Ready
    );
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_delete_waits_for_dpf_finalization(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projected = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &data);
    let expected_name = projected.name.clone();
    let existing = dpu_service_observation(&projected);
    let mut finalizing = existing.clone();
    finalizing.is_deleting = true;
    let get_calls = Arc::new(AtomicUsize::new(0));

    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    mock.expect_get_dpu_service().times(3).returning({
        let get_calls = Arc::clone(&get_calls);
        move |_| match get_calls.fetch_add(1, Ordering::Relaxed) {
            0 => Ok(Some(existing.clone())),
            1 => Ok(Some(finalizing.clone())),
            _ => Ok(None),
        }
    });
    mock.expect_delete_dpu_service()
        .times(1)
        .withf(move |name| name == expected_name.as_str())
        .returning(|_| Ok(()));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: Some(service_id.to_string()),
            service_name: "delete-dpf-service".to_string(),
            description: Some("delete controller test".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    // Deletion is accepted only as durable intent. The record and V1 stay
    // available to the state controller while DPF finalizers run.
    env.api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            versions: vec![],
        }))
        .await?;
    env.api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            versions: vec![ConfigVersion::initial().to_string()],
        }))
        .await?;
    let mut txn = env.pool.begin().await?;
    let deleting = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("soft-deleted DPF Helm service remains controller-visible");
    assert!(deleting.deleted.is_some());
    assert_eq!(
        deleting.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleting
    );
    assert!(
        db::extension_service::find_all_versions(&env.pool, service_id)
            .await?
            .is_empty()
    );
    txn.commit().await?;

    let deleting = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.to_string()],
        }))
        .await?
        .into_inner()
        .services;
    assert_eq!(deleting.len(), 1);
    assert_eq!(lifecycle_state(&deleting[0]), "deleting");
    assert!(deleting[0].active_versions.is_empty());
    assert!(deleting[0].latest_version_info.is_none());

    let recreate_while_deleting = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await;
    let error = recreate_while_deleting
        .expect_err("a pending DPF Helm chart deletion must reserve its service name");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    // DPF accepts the delete and then keeps the CR while finalizers run. NICo
    // must poll that deletionTimestamp-bearing object without issuing a
    // second delete; only the following NotFound completes the lifecycle.
    env.run_extension_service_controller_iteration().await;
    env.run_extension_service_controller_iteration().await;
    env.run_extension_service_controller_iteration().await;

    let mut txn = env.pool.begin().await?;
    let deleted = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("terminal service record remains available");
    assert_eq!(
        deleted.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleted
    );
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    assert_eq!(history.len(), 4);
    assert_eq!(
        serde_json::from_str::<ExtensionServiceLifecycleState>(&history[3].state)?,
        ExtensionServiceLifecycleState::Deleted
    );
    txn.commit().await?;

    let terminal = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.to_string()],
        }))
        .await?
        .into_inner()
        .services;
    assert!(terminal.is_empty());

    let recreated = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    assert_ne!(recreated.service_id, service_id.to_string());

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_delete_refuses_unowned_dpu_service(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projected = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &data);
    let mut unowned = dpu_service_observation(&projected);
    unowned.labels.clear();

    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    // There is deliberately no delete expectation: a same-name CR without
    // NICo's owner label must never be deleted.
    mock.expect_get_dpu_service()
        .times(1)
        .returning(move |_| Ok(Some(unowned.clone())));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: Some(service_id.to_string()),
            service_name: "unowned-delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    env.api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            versions: vec![],
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    let mut txn = env.pool.begin().await?;
    let service = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("soft-deleted service remains available after an ownership conflict");
    assert_eq!(
        service.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_delete_recovers_after_controller_restart(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let mut initial_mock = MockDpfOperations::new();
    initial_mock
        .expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    let env = create_dpf_controller_test_env(db_pool, initial_mock).await;
    create_test_tenants(&env).await?;

    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: Some(service_id.to_string()),
            service_name: "restart-delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;
    env.api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            versions: vec![],
        }))
        .await?;

    // Drop the first API/controller fixture before starting a fresh one over
    // the same durable database state. The restarted controller discovers the
    // soft-deleted Deleting row through its periodic scan.
    let restart_pool = env.pool.clone();
    drop(env);

    let mut restarted_mock = MockDpfOperations::new();
    restarted_mock
        .expect_get_dpu_service()
        .times(1)
        .returning(|_| Err(DpfError::not_found("DPUService", "already-gone")));
    let mut config = get_config();
    config.dpf.enabled = true;
    let mut restart_overrides =
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(restarted_mock));
    // The initial fixture already created the site-wide Admin and Underlay
    // segments in this database. A restarted API must reuse them rather than
    // attempting to create overlapping fixed-prefix test infrastructure.
    restart_overrides.create_network_segments = Some(false);
    let restarted = create_test_env_with_overrides(restart_pool, restart_overrides).await;
    restarted.run_extension_service_controller_iteration().await;

    let mut txn = restarted.pool.begin().await?;
    let service = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("terminal service remains available");
    assert_eq!(
        service.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleted
    );
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_metadata_update_keeps_active_lifecycle_and_v1(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "metadata-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    let service_id: ExtensionServiceId = created.service_id.parse()?;
    env.run_extension_service_controller_iteration().await;

    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            service_name: Some("metadata-dpf-service-renamed".to_string()),
            description: Some("metadata only".to_string()),
            data: String::new(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(1),
        }))
        .await?
        .into_inner();

    assert_eq!(updated.version_ctr, 1);
    assert_eq!(updated.active_versions.len(), 1);
    assert_eq!(
        updated.active_versions[0]
            .parse::<ConfigVersion>()?
            .version_nr(),
        1
    );

    let mut txn = env.pool.begin().await?;
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    // `Creating -> Ready` only. Metadata did not request controller work.
    assert_eq!(history.len(), 2);
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_rejects_unsupported_credentials_and_observability(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    for (name, credential, observability, expected_message) in [
        (
            "dpf-credential",
            Some(rpc::DpuExtensionServiceCredential {
                registry_url: String::new(),
                r#type: None,
            }),
            None,
            "credentials for DPF helm chart extension services should be preprovisioned and are not supported through API",
        ),
        (
            "dpf-observability",
            None,
            Some(create_observability()),
            "observability configuration for DPF helm chart extension services is not supported yet",
        ),
    ] {
        let service_id = ExtensionServiceId::new();
        let response = env
            .api
            .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
                service_id: Some(service_id.to_string()),
                service_name: name.to_string(),
                description: None,
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
                data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
                credential,
                observability,
            }))
            .await
            .expect_err("unsupported DPF option must be rejected before persistence");
        assert_eq!(response.code(), tonic::Code::FailedPrecondition);
        assert_eq!(response.message(), expected_message);

        let mut txn = env.pool.begin().await?;
        assert!(
            db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
                .await?
                .is_empty(),
            "rejected DPF option must not leave a controller row"
        );
        txn.commit().await?;
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_rejects_invalid_data(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    for (name, data, expected_message) in [
        (
            "dpf-missing-chart-version",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "security.privileged":false
            }"#,
            "missing field `chartVersion`",
        ),
        (
            "dpf-unknown-field",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "security.privileged":false,
                "unsupported":true
            }"#,
            "unknown field `unsupported`",
        ),
        (
            "dpf-reserved-node-selector",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "security.privileged":false,
                "values":{"serviceDaemonSet":{"nodeSelector":{"tenant":"value"}}}
            }"#,
            "tenant values may not set NICo-owned field serviceDaemonSet.nodeSelector",
        ),
    ] {
        let service_id = ExtensionServiceId::new();
        let error = env
            .api
            .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
                service_id: Some(service_id.to_string()),
                service_name: name.to_string(),
                description: None,
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
                data: data.to_string(),
                credential: None,
                observability: None,
            }))
            .await
            .expect_err("invalid DPF Helm data must be rejected before persistence");
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(error.message().contains(expected_message));

        let mut txn = env.pool.begin().await?;
        assert!(
            db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
                .await?
                .is_empty(),
            "invalid DPF Helm data must not leave a controller row"
        );
        txn.commit().await?;
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_rejects_duplicate_name_for_tenant(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "duplicate-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;

    let error = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "Duplicate-Dpf-Service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await
        .expect_err("DPF Helm service names must be unique within a tenant");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    Ok(())
}

/// Seeds a DPF Helm service directly so controller-plumbing tests can isolate
/// their setup from create-API assertions.
async fn seed_dpf_helm_chart_service(
    env: &TestEnv,
    name: &str,
) -> Result<ExtensionServiceId, eyre::Report> {
    let service_id = ExtensionServiceId::new();
    seed_dpf_helm_chart_service_with_id(env, service_id, name).await?;
    Ok(service_id)
}

async fn seed_dpf_helm_chart_service_with_id(
    env: &TestEnv,
    service_id: ExtensionServiceId,
    name: &str,
) -> Result<(), eyre::Report> {
    let tenant: TenantOrganizationId = "best_org".parse()?;
    let mut txn = env.pool.begin().await?;
    db::extension_service::create(
        &mut txn,
        ConfigVersion::initial(),
        &service_id,
        &ExtensionServiceType::DpfHelmChart,
        name,
        &tenant,
        Some("controller fixture"),
        TEST_DPF_HELM_CHART_SERVICE_DATA,
        None,
        false,
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_reconciliation_transitions_to_active(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;
    let service_id = seed_dpf_helm_chart_service(&env, "controller-create").await?;

    env.run_extension_service_controller_iteration().await;

    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record remains available");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert!(matches!(
        record.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    assert_eq!(history.len(), 2);
    assert_eq!(
        serde_json::from_str::<ExtensionServiceLifecycleState>(&history[1].state)?,
        ExtensionServiceLifecycleState::Ready
    );
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_reconciliation_accepts_owned_already_exists(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let expected = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &data);
    let expected_name = expected.name.clone();
    let expected_observation = dpu_service_observation(&expected);

    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(move |_| {
            Err(DpfError::already_exists(
                "DPUService",
                expected_name.clone(),
            ))
        });
    mock.expect_get_dpu_service()
        .times(1)
        .returning(move |_| Ok(Some(expected_observation.clone())));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;
    seed_dpf_helm_chart_service_with_id(&env, service_id, "controller-already-exists").await?;

    env.run_extension_service_controller_iteration().await;

    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record remains available");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_reconciliation_refuses_unowned_existing_service(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let expected = project_dpu_service(service_id, carbide_dpf::NAMESPACE, &data);
    let expected_name = expected.name.clone();
    let mut unowned = dpu_service_observation(&expected);
    unowned.labels.clear();

    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(move |_| {
            Err(DpfError::already_exists(
                "DPUService",
                expected_name.clone(),
            ))
        });
    mock.expect_get_dpu_service()
        .times(1)
        .returning(move |_| Ok(Some(unowned.clone())));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;
    seed_dpf_helm_chart_service_with_id(&env, service_id, "controller-ownership-conflict").await?;

    env.run_extension_service_controller_iteration().await;

    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record remains available");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_reconciliation_retries_transient_failure(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service().times(1).returning(|_| {
        Err(DpfError::timeout(
            "create DPUService",
            "simulated transport failure",
        ))
    });
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;
    let service_id = seed_dpf_helm_chart_service(&env, "controller-retry").await?;

    env.run_extension_service_controller_iteration().await;

    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record remains available");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        record.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPF DPUService creation failed; retrying"
    ));
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_controller_queue_scan_and_persistence(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    let service_id = seed_dpf_helm_chart_service(&env, "controller-scan").await?;
    let io = ExtensionServiceStateControllerIO::default();
    let enqueuer = Enqueuer::<ExtensionServiceStateControllerIO>::new(env.pool.clone());

    // No enqueue occurs on creation. A manual iteration uses the standard
    // periodic list and reaches the durable Creating row. This fixture has no
    // DPF mock, so it also proves the controller retries safely when DPF is
    // unavailable rather than issuing a direct API-handler mutation.
    env.run_extension_service_controller_iteration().await;

    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("DPF Helm chart service remains visible to the controller");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        record.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPF SDK is unavailable; retrying DPUService creation"
    ));
    let initial_history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    assert_eq!(initial_history.len(), 1);
    txn.commit().await?;

    // An explicit framework wake-up uses the same durable queue and preserves
    // the pending lifecycle state. Production relies on periodic scans for now.
    enqueuer.enqueue_object(&service_id).await?;
    env.run_extension_service_controller_iteration().await;

    // A mistakenly enqueued Kubernetes-pod service is not controller-visible:
    // the DPF-only load query returns None and no lifecycle state is changed.
    let non_dpf_id = ExtensionServiceId::new();
    let tenant: TenantOrganizationId = "best_org".parse()?;
    let mut txn = env.pool.begin().await?;
    db::extension_service::create(
        &mut txn,
        ConfigVersion::initial(),
        &non_dpf_id,
        &ExtensionServiceType::KubernetesPod,
        "controller-non-dpf",
        &tenant,
        None,
        TEST_SERVICE_DATA,
        None,
        false,
    )
    .await?;
    txn.commit().await?;
    enqueuer.enqueue_object(&non_dpf_id).await?;
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    assert!(io.load_object_state(&mut txn, &non_dpf_id).await?.is_none());
    txn.commit().await?;

    // Exercise the IO transition/history path directly. Production code does
    // not take this transition until a later component has completed DPF work.
    let mut txn = env.pool.begin().await?;
    let creating = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record exists");
    let deleted_version = creating.status.controller_state.version.increment();
    assert_eq!(
        io.persist_controller_state(
            &mut txn,
            &service_id,
            creating.status.controller_state.version,
            deleted_version,
            &ExtensionServiceLifecycleState::Deleted,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    io.persist_state_history(
        &mut txn,
        &service_id,
        deleted_version,
        &ExtensionServiceLifecycleState::Deleted,
    )
    .await?;
    // Models a create request that completed in DPF after a delete won the
    // database race. Its stale Creating version cannot overwrite Deleted with
    // Ready; the framework re-enqueues the object after this lost CAS.
    assert_eq!(
        io.persist_controller_state(
            &mut txn,
            &service_id,
            creating.status.controller_state.version,
            creating.status.controller_state.version.increment(),
            &ExtensionServiceLifecycleState::Ready,
        )
        .await?,
        db::ConditionalWrite::NotApplied(db::ControllerStateNotCurrent)
    );
    txn.commit().await?;

    // Deleted is terminal in the shell: it is still listed by periodic scans,
    // but never receives a DPF mutation or an unintended state transition.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let deleted = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("terminal record remains visible");
    assert_eq!(
        deleted.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleted
    );
    assert!(matches!(
        deleted.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::DoNothing { .. })
    ));
    let history = db::state_history::for_object(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
    )
    .await?;
    assert_eq!(history.len(), 2);
    txn.commit().await?;

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_create_with_credential(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(create_observability()),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_ok());

    let extension_service = create_resp.unwrap().into_inner();

    assert!(
        extension_service
            .latest_version_info
            .as_ref()
            .unwrap()
            .has_credential
    );

    // Verify the credential is stored correctly in Vault
    let Credentials::UsernamePassword { username, password } =
        get_credentials_for_extension_service(&env, &extension_service).await?;

    assert_eq!(
        username,
        "url: https://registry.test.com, username: test-username"
    );
    assert_eq!(password, "test-password");

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_create_failure(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let requested_extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(create_observability()),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(requested_extension_service.clone()))
        .await;
    assert!(create_resp.is_ok());

    let extension_service = create_resp.unwrap().into_inner();
    let latest_version = extension_service.latest_version_info.unwrap();

    assert!(latest_version.has_credential);

    // Verify the credential is stored correctly in Vault
    let credential_key = carbide_secrets::credentials::CredentialKey::ExtensionService {
        service_id: extension_service.service_id.clone(),
        version: latest_version
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr()
            .to_string(),
    };

    let stored_credential = env
        .api
        .credential_manager
        .get_credentials(&credential_key)
        .await?
        .expect("creating an extension service should have created a credential");

    let metrics = MetricsCapture::start();
    env.test_credential_manager
        .set_delete_credentials_failure(true);

    // Try to create an identical extension service. The database rejects the
    // duplicate after the new credential was stored, and this fixture makes
    // the follow-up credential cleanup fail too.
    let create_resp_2 = env
        .api
        .create_dpu_extension_service(Request::new(requested_extension_service))
        .await;
    assert!(
        create_resp_2.is_err(),
        "creating a second identical extension service should have failed"
    );
    assert_eq!(
        metrics.counter_delta(
            CREDENTIAL_CLEANUP_FAILURE_METRIC,
            &[("operation", "create")],
        ),
        1.0,
    );

    let stored_credential_2 = env
        .api
        .credential_manager
        .get_credentials(&credential_key)
        .await?
        .expect("Failing to create a second extension should not delete existing credentials");

    assert_eq!(
        stored_credential, stored_credential_2,
        "Failing to create a second extension should not have changed the credentials"
    );
    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_update_race_condition(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let credential = {
        let mut c = create_credential();
        c.r#type = Some(
            rpc::dpu_extension_service_credential::Type::UsernamePassword(rpc::UsernamePassword {
                username: "test-username-1".to_string(),
                password: "test-password-1".to_string(),
            }),
        );
        c
    };

    // When updating we will try to set the credentials to this for both services.
    let updated_credential = {
        let mut c = create_credential();
        c.r#type = Some(
            rpc::dpu_extension_service_credential::Type::UsernamePassword(rpc::UsernamePassword {
                username: "test-username-updated".to_string(),
                password: "test-password-updated".to_string(),
            }),
        );
        c
    };

    // Create an original service, then create another, then update the second service to collide with the first one, which will fail.
    let service = create_test_extension_service(&env.api, "test-service-1", Some(credential))
        .await
        .unwrap();

    // Make the credential writes slow so that the database can conflict
    env.test_credential_manager
        .set_credentials_sleep_time_ms
        .store(1000, Ordering::SeqCst);

    // Make both requests at the same time
    let (update_1, update_2) = {
        let join_handle_1 = tokio::spawn({
            let api = env.api.clone();
            let request = Request::new(rpc::UpdateDpuExtensionServiceRequest {
                service_id: service.service_id.clone(),
                service_name: Some("test-service-updated".to_string()), // should cause collision
                description: Some(service.description.clone()),
                data: TEST_SERVICE_DATA.to_string(),
                credential: Some(updated_credential.clone()),
                if_version_ctr_match: None,
                observability: None,
            });
            async move { api.update_dpu_extension_service(request).await }
        });
        let join_handle_2 = tokio::spawn({
            let api = env.api.clone();
            let request = Request::new(rpc::UpdateDpuExtensionServiceRequest {
                service_id: service.service_id.clone(),
                service_name: Some("test-service-updated".to_string()), // should cause collision
                description: Some(service.description.clone()),
                data: TEST_SERVICE_DATA.to_string(),
                credential: Some(updated_credential.clone()),
                if_version_ctr_match: None,
                observability: None,
            });
            async move { api.update_dpu_extension_service(request).await }
        });
        (join_handle_1.await.unwrap(), join_handle_2.await.unwrap())
    };

    match (update_1, update_2) {
        (Ok(s), Err(_)) => {
            let Credentials::UsernamePassword {
                username: _,
                password,
            } = get_credentials_for_extension_service(&env, &s.into_inner())
                .await
                .unwrap();
            assert_eq!(
                password, "test-password-updated",
                "update_1 won the race but the password did not get updated: {password}"
            );

            let Credentials::UsernamePassword {
                username: _,
                password,
            } = get_credentials_for_extension_service(&env, &service)
                .await
                .unwrap();
            assert_eq!(
                password, "test-password-1",
                "update_2 lost the race but the password got updated: {password}"
            );
        }
        (Err(_), Ok(s)) => {
            let Credentials::UsernamePassword {
                username: _,
                password,
            } = get_credentials_for_extension_service(&env, &service)
                .await
                .unwrap();
            assert_eq!(
                password, "test-password-1",
                "update_1 lost the race but the password got updated: {password}"
            );

            let Credentials::UsernamePassword {
                username: _,
                password,
            } = get_credentials_for_extension_service(&env, &s.into_inner())
                .await
                .unwrap();
            assert_eq!(
                password, "test-password-updated",
                "update_2 won the race but the password did not get updated: {password}"
            );
        }
        (Err(e1), Err(e2)) => {
            panic!("Both services failed in updating, this should not happen: {e1:?}, {e2:?}")
        }
        (Ok(s1), Ok(s2)) => {
            panic!("Both services succeeded in updating, this should not happen: {s1:?}, {s2:?}")
        }
    };

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_update_failure(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let credentials = create_credential();

    // Create 2 services, then create another, then try to update them both to the same new name at the same time
    let service_1 =
        create_test_extension_service(&env.api, "test-service-1", Some(credentials.clone()))
            .await
            .unwrap();
    let service_2 =
        create_test_extension_service(&env.api, "test-service-2", Some(credentials.clone()))
            .await
            .unwrap();

    let service_2_credentials_before =
        get_credentials_for_extension_service(&env, &service_2).await?;

    let updated_credentials = {
        let mut c = credentials.clone();
        c.r#type = Some(
            rpc::dpu_extension_service_credential::Type::UsernamePassword(rpc::UsernamePassword {
                username: "test-username-updated".to_string(),
                password: "test-password-updated".to_string(),
            }),
        );
        c
    };

    let metrics = MetricsCapture::start();
    env.test_credential_manager
        .set_delete_credentials_failure(true);

    let update_response = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_2.service_id.clone(),
            service_name: Some("test-service-1".to_string()), // should cause collision
            description: Some(service_1.description.clone()),
            data: TEST_SERVICE_DATA.to_string(),
            credential: Some(updated_credentials.clone()),
            if_version_ctr_match: None,
            observability: None,
        }))
        .await;

    assert!(
        update_response.is_err(),
        "update_dpu_extension_service should have failed, got: {update_response:?}"
    );
    assert_eq!(
        metrics.counter_delta(
            CREDENTIAL_CLEANUP_FAILURE_METRIC,
            &[("operation", "update")],
        ),
        1.0,
    );

    let service_2_credentials_after =
        get_credentials_for_extension_service(&env, &service_2).await?;
    assert_eq!(
        service_2_credentials_before, service_2_credentials_after,
        "Failing to update should not have changed the credentials in vault"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_create_race_condition(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let credential_1 = {
        let mut c = create_credential();
        c.r#type = Some(
            rpc::dpu_extension_service_credential::Type::UsernamePassword(rpc::UsernamePassword {
                username: "test-username-1".to_string(),
                password: "test-password-1".to_string(),
            }),
        );
        c
    };
    let credential_2 = {
        let mut c = create_credential();
        c.r#type = Some(
            rpc::dpu_extension_service_credential::Type::UsernamePassword(rpc::UsernamePassword {
                username: "test-username-2".to_string(),
                password: "test-password-2".to_string(),
            }),
        );
        c
    };

    // Make the credential writes slow so that the database can conflict
    env.test_credential_manager
        .set_credentials_sleep_time_ms
        .store(1000, Ordering::SeqCst);

    // Make both requests at the same time
    let (service_1, service_2) = {
        let join_handle_1 = tokio::spawn({
            let api = env.api.clone();
            async move {
                create_test_extension_service(&api, "test-service-1", Some(credential_1.clone()))
                    .await
            }
        });
        let join_handle_2 = tokio::spawn({
            let api = env.api.clone();
            async move {
                create_test_extension_service(&api, "test-service-1", Some(credential_2.clone()))
                    .await
            }
        });
        (join_handle_1.await.unwrap(), join_handle_2.await.unwrap())
    };

    match (service_1, service_2) {
        (Ok(s), Err(_)) => {
            let Credentials::UsernamePassword {
                username: _,
                password,
            } = get_credentials_for_extension_service(&env, &s).await?;
            assert_eq!(
                password, "test-password-1",
                "service_1 won the race but the password does not match: {password}"
            );
        }
        (Err(_), Ok(s)) => {
            let Credentials::UsernamePassword {
                username: _,
                password,
            } = get_credentials_for_extension_service(&env, &s).await?;
            assert_eq!(
                password, "test-password-2",
                "service_2 won the race but the password does not match: {password}"
            );
        }
        (Err(e1), Err(e2)) => {
            panic!("Both services failed in creation, this should not happen: {e1:?}, {e2:?}")
        }
        (Ok(s1), Ok(s2)) => {
            panic!("Both services succeeded in creation, this should not happen: {s1:?}, {s2:?}")
        }
    };

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_creation_invalid_arg(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    // Test empty service name
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test empty data
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: "".to_string(),
        credential: None,
        observability: None,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid data format (not YAML or JSON)
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test".to_string(),
        credential: None,
        observability: None,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid credential registry URL
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        observability: None,
        credential: Some(rpc::DpuExtensionServiceCredential {
            registry_url: "".to_string(),
            r#type: Some(
                rpc::dpu_extension_service_credential::Type::UsernamePassword(
                    rpc::UsernamePassword {
                        username: "test-username".to_string(),
                        password: "test-password".to_string(),
                    },
                ),
            ),
        }),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid observability config
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(rpc::DpuExtensionServiceObservability {
            configs: vec![rpc::DpuExtensionServiceObservabilityConfig {
                name: Some("prom".to_string()),
                config: None,
            }],
        }),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid observability config name
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(rpc::DpuExtensionServiceObservability {
            configs: vec![rpc::DpuExtensionServiceObservabilityConfig {
                name: Some("x".to_string().repeat(65)),
                config: Some(
                    rpc::dpu_extension_service_observability_config::Config::Logging(
                        rpc::DpuExtensionServiceObservabilityConfigLogging {
                            path: "something".to_string(),
                        },
                    ),
                ),
            }],
        }),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Fail to create an an extension with too many observability configs
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: Some(rpc::DpuExtensionServiceObservability {
            configs: vec![
                DpuExtensionServiceObservabilityConfig {
                    name: None,
                    config: Some(Config::Logging(
                        DpuExtensionServiceObservabilityConfigLogging {
                            path: "/dev/null".to_string(),
                        }
                    )),
                };
                100
            ],
        }),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());
    let status = create_resp.err().unwrap();
    assert!(status.message().contains("exceeds"));

    // Fail to create an an extension with just a basic bad observability config
    // that's missing the actual config.
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: Some(rpc::DpuExtensionServiceObservability {
            configs: vec![DpuExtensionServiceObservabilityConfig {
                name: None,
                config: None,
            }],
        }),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    Ok(())
}

// Two extension services should not have same case-insensitive name
#[crate::sqlx_test]
async fn test_extension_service_creation_with_same_name(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,

        tenant_organization_id: "best_org".to_string(),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;

    assert!(create_resp.is_ok());

    println!(
        "Extension service created: {:?}",
        create_resp.unwrap().into_inner()
    );

    // Creating a new extension service with the same name and tenant organization ID should fail
    let duplicate_extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "Test-Service".to_string(),
        description: Some("Test service".to_string()),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,

        tenant_organization_id: "best_org".to_string(),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(duplicate_extension_service))
        .await;
    assert!(create_resp.is_err());
    let status = create_resp.unwrap_err();
    println!("Error: {:?}", status);
    assert!(status.message().contains("already exists"));

    // However, creating a new extension service with the same name but different tenant organization ID should be allowed
    let new_extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,

        tenant_organization_id: "another_org".to_string(),
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(new_extension_service))
        .await;
    assert!(create_resp.is_ok());

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_recreate_same_name_after_delete(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;

    create_test_tenants(&env).await?;

    let service_name = "test-service";
    let extension_service = create_test_extension_service(&env.api, service_name, None).await?;
    let service_id = extension_service.service_id.clone();

    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![],
        }))
        .await;
    assert!(delete_resp.is_ok());

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    assert!(find_resp.unwrap().into_inner().services.is_empty());

    let mut txn = env.pool.begin().await?;
    let deleted = db::extension_service::find_by_ids(&mut txn, &[service_id.parse()?], true, false)
        .await?
        .pop()
        .expect("soft-deleted Kubernetes Pod service remains in the database");
    assert_eq!(
        deleted.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleted
    );
    txn.commit().await?;

    let recreate_resp = create_test_extension_service(&env.api, service_name, None).await;
    assert!(recreate_resp.is_ok());

    let recreated_service = recreate_resp.unwrap();
    assert_eq!(recreated_service.service_name, service_name);
    assert_ne!(recreated_service.service_id, extension_service.service_id);

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_update(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_and_tenants(&env).await?;
    let service_id = service_version.service_id;

    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: Some("updated-service".to_string()),
            description: Some("Updated service".to_string()),
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: Some(create_credential()),
            observability: Some(create_observability()),
            if_version_ctr_match: Some(1),
        }))
        .await;
    assert!(update_resp.is_ok());

    let extension_service = update_resp.unwrap().into_inner();
    assert_eq!(extension_service.service_name, "updated-service");
    assert_eq!(extension_service.description, "Updated service");
    assert_eq!(extension_service.version_ctr, 2);
    let latest_version = extension_service.latest_version_info.unwrap();
    assert_eq!(
        latest_version
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );
    // The versions are in descending order, so version2 will be placed first
    assert_eq!(extension_service.active_versions.len(), 2);
    assert_eq!(
        extension_service.active_versions[0]
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );
    assert_eq!(
        extension_service.active_versions[1]
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        1
    );

    // Update but with credential deleted
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_3.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: Some(2),
        }))
        .await;
    assert!(update_resp.is_ok());

    let extension_service = update_resp.unwrap().into_inner();
    assert_eq!(extension_service.version_ctr, 3);
    assert!(
        !extension_service
            .latest_version_info
            .as_ref()
            .unwrap()
            .has_credential,
    );
    assert_eq!(
        extension_service
            .latest_version_info
            .as_ref()
            .unwrap()
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        3
    );
    assert_eq!(extension_service.active_versions.len(), 3);
    assert_eq!(
        extension_service.active_versions[0]
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        3
    );
    assert_eq!(
        extension_service.active_versions[1]
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );
    assert_eq!(
        extension_service.active_versions[2]
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        1
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_update_invalid_arg(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_and_tenants(&env).await?;
    let service_id = service_version.service_id;

    // Create another extension service with a different name
    let other_extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "other-test-service".to_string(),
        description: Some("Other test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
    };
    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(other_extension_service))
        .await;
    assert!(create_resp.is_ok());

    // Update with empty service name
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: Some("".to_string()),
            description: None,
            data: TEST_SERVICE_DATA_VERSION_3.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: None,
        }))
        .await;
    assert!(update_resp.is_err());
    assert!(
        update_resp
            .unwrap_err()
            .message()
            .contains("service_name cannot be empty")
    );

    // Update with wrong version match number
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_3.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: Some(2),
        }))
        .await;
    assert!(update_resp.is_err());

    // Update with same data and credential
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA.to_string(),
            credential: None,
            observability: Some(create_observability()),

            if_version_ctr_match: None,
        }))
        .await;
    assert!(update_resp.is_err());
    assert!(
        update_resp
            .unwrap_err()
            .message()
            .contains("no changes to data or credential from latest version")
    );

    // Update with empty data format
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: "".to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: None,
        }))
        .await;
    assert!(update_resp.is_err());
    assert!(
        update_resp
            .unwrap_err()
            .message()
            .contains("invalid empty data for KubernetesPod service, need a valid pod manifest")
    );

    // Update with invalid data format
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: "invalid data".to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: None,
        }))
        .await;
    assert!(update_resp.is_err());
    assert!(
        update_resp
            .unwrap_err()
            .message()
            .contains("pod manifest must be a valid mapping object")
    );

    // Update with wrong service id
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: Uuid::new_v4().to_string(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: None,
        }))
        .await;
    assert!(update_resp.is_err());
    assert!(
        update_resp
            .unwrap_err()
            .message()
            .contains("extension_service not found:")
    );

    // Update to a name that's already taken
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: Some("other-test-service".to_string()),
            description: None,
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: Some(1),
        }))
        .await;
    assert!(update_resp.is_err());
    let status = update_resp.err().unwrap();
    assert!(status.message().contains("already exists"));

    // Update to set too many observability configs
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: Some("other-test-service".to_string()),
            description: None,
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: Some(rpc::DpuExtensionServiceObservability {
                configs: vec![
                    DpuExtensionServiceObservabilityConfig {
                        name: None,
                        config: Some(Config::Logging(
                            DpuExtensionServiceObservabilityConfigLogging {
                                path: "/dev/null".to_string(),
                            }
                        )),
                    };
                    100
                ],
            }),

            if_version_ctr_match: Some(1),
        }))
        .await;
    assert!(update_resp.is_err());
    let status = update_resp.err().unwrap();
    assert!(status.message().contains("exceeds"));

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_update_metadata(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_and_tenants(&env).await?;
    let service_id = service_version.service_id;

    // Create another extension service with a different name
    let other_extension_service = rpc::CreateDpuExtensionServiceRequest {
        service_id: None,
        service_name: "other-test-service".to_string(),
        description: Some("Other test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
    };
    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(other_extension_service))
        .await;
    assert!(create_resp.is_ok());

    // Update only name
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: Some("updated-service".to_string()),
            description: None,
            data: "".to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: None,
        }))
        .await;
    assert!(update_resp.is_ok());

    let extension_service = update_resp.unwrap().into_inner();
    assert_eq!(extension_service.service_name, "updated-service");
    assert_eq!(extension_service.description, "Test service");
    assert_eq!(extension_service.version_ctr, 1);
    let latest_version = extension_service.latest_version_info.unwrap();
    // Expect no version increment
    assert_eq!(
        latest_version
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        1
    );

    // Normal update should still work
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: Some(create_credential()),
            observability: None,
            if_version_ctr_match: Some(1),
        }))
        .await;
    assert!(update_resp.is_ok());
    let extension_service = update_resp.unwrap().into_inner();
    assert_eq!(extension_service.service_name, "updated-service");
    assert_eq!(extension_service.description, "Test service");
    assert_eq!(extension_service.version_ctr, 2);
    let latest_version = extension_service.latest_version_info.unwrap();
    assert_eq!(
        latest_version
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );

    // Update both name and description
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: Some("updated-service-2".to_string()),
            description: Some("Updated description".to_string()),
            data: "".to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: None,
        }))
        .await;
    assert!(update_resp.is_ok());

    let extension_service = update_resp.unwrap().into_inner();
    assert_eq!(extension_service.service_name, "updated-service-2");
    assert_eq!(extension_service.description, "Updated description");
    assert_eq!(extension_service.version_ctr, 2);
    let latest_version = extension_service.latest_version_info.unwrap();
    // Expect no version increment
    assert_eq!(
        latest_version
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_find_ids(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_and_tenants(&env).await?;
    let service_id = service_version.service_id;

    // Find the extension service by name
    let find_resp = env
        .api
        .find_dpu_extension_service_ids(Request::new(rpc::DpuExtensionServiceSearchFilter {
            service_type: Some(rpc::DpuExtensionServiceType::KubernetesPod.into()),
            name: Some("test-service".to_string()),
            tenant_organization_id: Some("best_org".to_string()),
        }))
        .await;
    assert!(find_resp.is_ok());
    let service_ids = find_resp.unwrap().into_inner().service_ids;
    assert!(service_ids.contains(&service_id));

    // Find the extension service by service type
    let find_resp = env
        .api
        .find_dpu_extension_service_ids(Request::new(rpc::DpuExtensionServiceSearchFilter {
            service_type: Some(rpc::DpuExtensionServiceType::KubernetesPod.into()),
            name: None,
            tenant_organization_id: None,
        }))
        .await;
    assert!(find_resp.is_ok());
    let service_ids = find_resp.unwrap().into_inner().service_ids;
    assert!(service_ids.contains(&service_id));

    // Find the extension service by both service name and service type
    let find_resp = env
        .api
        .find_dpu_extension_service_ids(Request::new(rpc::DpuExtensionServiceSearchFilter {
            service_type: Some(rpc::DpuExtensionServiceType::KubernetesPod.into()),
            name: Some("test-service".to_string()),
            tenant_organization_id: None,
        }))
        .await;
    assert!(find_resp.is_ok());
    let service_ids = find_resp.unwrap().into_inner().service_ids;
    assert!(service_ids.contains(&service_id));

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_find_by_ids(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_and_tenants(&env).await?;
    let service_id = service_version.service_id;
    let service_ids = vec![
        service_id,
        Uuid::new_v4().to_string(),
        Uuid::new_v4().to_string(),
    ];

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids,
        }))
        .await;

    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.len() == 1);
    assert_eq!(lifecycle_state(&services[0]), "ready");

    println!("Services found: {:?}", services);

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_find_by_ids_latest_version_numerical_ordering(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_id = create_test_extension_service_with_ten_versions(&env).await?;

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.len() == 1);
    assert!(
        services[0]
            .clone()
            .latest_version_info
            .unwrap()
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr()
            == 11
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_delete(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_with_three_versions(&env).await?;
    let service_id = service_version.service_id;

    // The versions are in descending order, so version2 is the latest version
    println!("Active versions: {:?}", service_version.active_versions);
    let version3 = service_version.active_versions[0].clone();
    let version2 = service_version.active_versions[1].clone();
    let version1 = service_version.active_versions[2].clone();

    // Delete two versions, the service should still be found
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![version1.to_string(), version2.to_string()],
        }))
        .await;
    assert!(delete_resp.is_ok());

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.len() == 1);
    let service = services[0].clone();
    assert_eq!(service.version_ctr, 3);
    assert_eq!(
        service.latest_version_info.unwrap().version,
        version3.clone()
    );
    assert_eq!(service.active_versions.len(), 1);
    assert_eq!(
        service.active_versions[0]
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        3
    );

    // Try delete a version that does not exist, the request should pass and the service should still be found
    let fake_version = config_version::ConfigVersion::new(4);

    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![fake_version.to_string()],
        }))
        .await;
    assert!(delete_resp.is_ok());

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.len() == 1);
    let service = services[0].clone();
    assert_eq!(service.version_ctr, 3);
    assert_eq!(
        service
            .latest_version_info
            .unwrap()
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        3
    );
    assert_eq!(service.active_versions.len(), 1);
    assert_eq!(
        service.active_versions[0]
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        3
    );

    // Try delete the service with some invalid version
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec!["V1-T?".to_string(), version3.to_string()],
        }))
        .await;
    assert!(delete_resp.is_err());
    assert!(
        delete_resp
            .err()
            .unwrap()
            .to_string()
            .contains("Failed to parse version")
    );

    // Now delete the last version, the service should now be fully deleted and cannot be found
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![version3.to_string()],
        }))
        .await;
    assert!(delete_resp.is_ok());

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.is_empty());

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_delete_in_use(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let service_version = create_test_extension_service_with_three_versions(&env).await?;
    let service_id = service_version.service_id;

    // The versions are in descending order, so version2 is the latest version
    println!("Active versions: {:?}", service_version.active_versions);
    let version3 = service_version.active_versions[0].clone();
    let version1 = service_version.active_versions[2].clone();

    // Create two instances that each use a different version of the extension service
    let (_, _) = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .extension_services(rpc::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::InstanceDpuExtensionServiceConfig {
                service_id: service_id.clone(),
                version: version3.clone(),
            }],
        })
        .build_and_return()
        .await;

    // Now try to delete the extension service, this should fail since one of the versions is in use
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![],
        }))
        .await;
    assert!(delete_resp.is_err());
    let status = delete_resp.err().unwrap();
    assert!(status.message().contains("in use"));

    // Now try to delete the extension service with the version that is in use, this shoud fail
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![version3.clone()],
        }))
        .await;
    assert!(delete_resp.is_err());
    let status = delete_resp.err().unwrap();
    assert!(status.message().contains("in use"));

    // Now try to delete the extension service with the version that is not in use, this should succeed
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![version1.clone()],
        }))
        .await;
    assert!(delete_resp.is_ok());

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;

    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.len() == 1);
    assert_eq!(services[0].service_id, service_id);
    assert_eq!(services[0].active_versions.len(), 2);

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_delete_default(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_with_three_versions(&env).await?;
    let service_id = service_version.service_id;

    // Try delete the whole service by no providing the versions field
    // Delete two versions, the service should still be found
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![],
        }))
        .await;
    assert!(delete_resp.is_ok());

    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.is_empty());

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_create_update_delete_credential(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service_version = create_test_extension_service_with_three_versions(&env).await?;
    let service_id = service_version.service_id;

    // The versions are in descending order, so version2 is the latest version
    println!("Active versions: {:?}", service_version.active_versions);
    let version3 = service_version.active_versions[0].clone();
    let version2 = service_version.active_versions[1].clone();
    let version1 = service_version.active_versions[2].clone();

    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![version1.to_string(), version2.to_string()],
        }))
        .await;
    assert!(delete_resp.is_ok());

    // Update the extension service with a credential
    let update_resp = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            credential: Some(create_credential()),
            observability: Some(create_observability()),
            if_version_ctr_match: None,
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
        }))
        .await;
    assert!(update_resp.is_ok());

    let extension_service = update_resp.unwrap().into_inner();
    assert!(
        extension_service
            .latest_version_info
            .clone()
            .unwrap()
            .has_credential,
    );
    let version4 = extension_service
        .latest_version_info
        .unwrap()
        .version
        .clone();

    // Delete the extension service
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![version3.to_string()],
        }))
        .await;
    assert!(delete_resp.is_ok());

    // Find the extension service by name
    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.len() == 1);
    let service = services[0].clone();
    let latest_version = service.latest_version_info.unwrap();
    assert_eq!(service.version_ctr, 4);
    assert_eq!(service.active_versions, vec![version4.clone()]);
    assert_eq!(
        latest_version
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        4
    );
    assert!(latest_version.has_credential);

    // Verify the credential is stored correctly in Vault
    let credential_key = CredentialKey::ExtensionService {
        service_id: service_id.clone(),
        version: 4.to_string(),
    };

    let stored_credential = env
        .api
        .credential_manager
        .get_credentials(&credential_key)
        .await?;

    match stored_credential {
        Some(Credentials::UsernamePassword { username, password }) => {
            assert_eq!(
                username,
                "url: https://registry.test.com, username: test-username"
            );
            assert_eq!(password, "test-password");
        }
        _ => {
            return Err(eyre::eyre!("could not find the credential"));
        }
    }

    // Delete the version with credential
    let delete_resp = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![version4.to_string()],
        }))
        .await;
    assert!(delete_resp.is_ok());

    // Expect the extension service is no longer found
    let find_resp = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.clone()],
        }))
        .await;
    assert!(find_resp.is_ok());
    let services = find_resp.unwrap().into_inner().services;
    assert!(services.is_empty());

    // Expect the credential is deleted from Vault
    let credential_key = carbide_secrets::credentials::CredentialKey::ExtensionService {
        service_id: service_id.clone(),
        version: 4.to_string(),
    };
    let stored_credential = env
        .api
        .credential_manager
        .get_credentials(&credential_key)
        .await;
    assert!(stored_credential.is_ok());
    assert!(stored_credential.unwrap().is_none());

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_delete_credential_cleanup_failure(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    create_test_tenants(&env).await?;
    let first_service_version =
        create_test_extension_service(&env.api, "test-service", Some(create_credential())).await?;
    let service_id = first_service_version.service_id.clone();
    let first_version = first_service_version
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .parse::<ConfigVersion>()?;

    let second_service_version = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: Some(create_credential()),
            observability: None,
            if_version_ctr_match: Some(1),
        }))
        .await?
        .into_inner();
    let second_version = second_service_version
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .parse::<ConfigVersion>()?;
    let first_credential_key = CredentialKey::ExtensionService {
        service_id: service_id.clone(),
        version: first_version.version_nr().to_string(),
    };
    let second_credential_key = CredentialKey::ExtensionService {
        service_id: service_id.clone(),
        version: second_version.version_nr().to_string(),
    };
    let first_credential_before = env
        .api
        .credential_manager
        .get_credentials(&first_credential_key)
        .await?;
    let second_credential_before = env
        .api
        .credential_manager
        .get_credentials(&second_credential_key)
        .await?;

    let metrics = MetricsCapture::start();
    env.test_credential_manager
        .set_delete_credentials_failure(true);

    // The service version is committed as deleted before credential cleanup
    // runs. A credential-store failure must be visible without turning the RPC
    // into an error that suggests the database deletion can be retried.
    let delete_response = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            versions: vec![first_version.to_string(), second_version.to_string()],
        }))
        .await;

    assert!(delete_response.is_ok());
    assert_eq!(
        metrics.counter_delta(
            CREDENTIAL_CLEANUP_FAILURE_METRIC,
            &[("operation", "delete")],
        ),
        2.0,
    );

    let find_response = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id],
        }))
        .await?;
    assert!(find_response.into_inner().services.is_empty());

    let first_credential_after = env
        .api
        .credential_manager
        .get_credentials(&first_credential_key)
        .await?;
    let second_credential_after = env
        .api
        .credential_manager
        .get_credentials(&second_credential_key)
        .await?;
    assert_eq!(first_credential_after, first_credential_before);
    assert_eq!(second_credential_after, second_credential_before);

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_get_version_infos(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let service = create_test_extension_service_with_three_versions(&env).await?;
    let service_id = service.service_id;

    println!("Active versions: {:?}", service.active_versions);
    let version3 = service.active_versions[0].clone();
    let version2 = service.active_versions[1].clone();
    let version1 = service.active_versions[2].clone();

    // Get all version infos without specifying versions filter
    let get_resp = env
        .api
        .get_dpu_extension_service_versions_info(Request::new(
            rpc::GetDpuExtensionServiceVersionsInfoRequest {
                service_id: service_id.clone(),
                versions: vec![],
            },
        ))
        .await;

    assert!(get_resp.is_ok());
    let version_infos = get_resp.unwrap().into_inner().version_infos;

    // Should return all 3 versions in descending order
    assert_eq!(version_infos.len(), 3);
    assert_eq!(
        version_infos[0]
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        3
    );
    assert_eq!(
        version_infos[1]
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );
    assert_eq!(
        version_infos[2]
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        1
    );

    // Verify each version has the correct data
    assert_eq!(version_infos[0].data, TEST_SERVICE_DATA_VERSION_3);
    assert_eq!(version_infos[1].data, TEST_SERVICE_DATA_VERSION_2);
    assert_eq!(version_infos[2].data, TEST_SERVICE_DATA);

    // All versions should not have credentials
    assert!(!version_infos[0].has_credential);
    assert!(!version_infos[1].has_credential);
    assert!(!version_infos[2].has_credential);

    // Get all version infos by specifying versions filter
    let get_resp = env
        .api
        .get_dpu_extension_service_versions_info(Request::new(
            rpc::GetDpuExtensionServiceVersionsInfoRequest {
                service_id: service_id.clone(),
                versions: vec![
                    version1.to_string(),
                    version2.to_string(),
                    version3.to_string(),
                ],
            },
        ))
        .await;

    assert!(get_resp.is_ok());
    let version_infos = get_resp.unwrap().into_inner().version_infos;

    // Should return all 3 versions in descending order
    assert_eq!(version_infos.len(), 3);
    assert_eq!(
        version_infos[0]
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        3
    );
    assert_eq!(
        version_infos[1]
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );
    assert_eq!(
        version_infos[2]
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        1
    );

    // Verify each version has the correct data
    assert_eq!(version_infos[0].data, TEST_SERVICE_DATA_VERSION_3);
    assert_eq!(version_infos[1].data, TEST_SERVICE_DATA_VERSION_2);
    assert_eq!(version_infos[2].data, TEST_SERVICE_DATA);

    // All versions should not have credentials
    assert!(!version_infos[0].has_credential);
    assert!(!version_infos[1].has_credential);
    assert!(!version_infos[2].has_credential);

    // Get all version infos by specifying versions filter
    let get_resp = env
        .api
        .get_dpu_extension_service_versions_info(Request::new(
            rpc::GetDpuExtensionServiceVersionsInfoRequest {
                service_id: service_id.clone(),
                versions: vec![version2.to_string()],
            },
        ))
        .await;

    assert!(get_resp.is_ok());
    let version_infos = get_resp.unwrap().into_inner().version_infos;

    // Should return 1 version
    assert_eq!(version_infos.len(), 1);
    assert_eq!(
        version_infos[0]
            .version
            .parse::<config_version::ConfigVersion>()
            .unwrap()
            .version_nr(),
        2
    );

    // Verify each version has the correct data
    assert_eq!(version_infos[0].data, TEST_SERVICE_DATA_VERSION_2);

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_instances_by_extension_service(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh1 = create_managed_host(&env).await;
    let mh2 = create_managed_host(&env).await;

    // Create an extension service with two versions
    let service = create_test_extension_service_and_tenants(&env).await?;
    let service_id = service.service_id.clone();
    let version1 = service
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    // Update to create version 2
    let service = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_id: service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,

            if_version_ctr_match: Some(1),
        }))
        .await?
        .into_inner();
    let version2 = service
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    // Create instance1 using version1
    let (instance1, _) = mh1
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .extension_services(rpc::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::InstanceDpuExtensionServiceConfig {
                service_id: service_id.clone(),
                version: version1.clone(),
            }],
        })
        .build_and_return()
        .await;

    // Create instance2 using version2
    let (instance2, _) = mh2
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .extension_services(rpc::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::InstanceDpuExtensionServiceConfig {
                service_id: service_id.clone(),
                version: version2.clone(),
            }],
        })
        .build_and_return()
        .await;

    // Find instances by service without version filter - should return both instances
    let find_resp = env
        .api
        .find_instances_by_dpu_extension_service(Request::new(
            rpc::FindInstancesByDpuExtensionServiceRequest {
                service_id: service_id.clone(),
                version: None,
            },
        ))
        .await?;

    let instances = find_resp.into_inner().instances;
    assert_eq!(instances.len(), 2);

    // Verify both instances are returned
    let instance_ids: Vec<String> = instances.iter().map(|i| i.instance_id.clone()).collect();
    assert!(instance_ids.contains(&instance1.id.to_string()));
    assert!(instance_ids.contains(&instance2.id.to_string()));

    // Verify version information
    let instance1_info = instances
        .iter()
        .find(|i| i.instance_id == instance1.id.to_string())
        .unwrap();
    assert_eq!(instance1_info.service_id, service_id);
    assert_eq!(instance1_info.version, version1);
    assert!(instance1_info.removed.is_none());

    let instance2_info = instances
        .iter()
        .find(|i| i.instance_id == instance2.id.to_string())
        .unwrap();
    assert_eq!(instance2_info.service_id, service_id);
    assert_eq!(instance2_info.version, version2);
    assert!(instance2_info.removed.is_none());

    // Find instances by service with version1 filter - should return only instance1
    let find_resp = env
        .api
        .find_instances_by_dpu_extension_service(Request::new(
            rpc::FindInstancesByDpuExtensionServiceRequest {
                service_id: service_id.clone(),
                version: Some(version1.clone()),
            },
        ))
        .await?;
    let instances = find_resp.into_inner().instances;
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, instance1.id.to_string());
    assert_eq!(instances[0].service_id, service_id);
    assert_eq!(instances[0].version, version1);
    assert!(instances[0].removed.is_none());

    // Find instance by service with version2 filter - should return only instance2
    let find_resp = env
        .api
        .find_instances_by_dpu_extension_service(Request::new(
            rpc::FindInstancesByDpuExtensionServiceRequest {
                service_id: service_id.clone(),
                version: Some(version2.clone()),
            },
        ))
        .await?;
    let instances = find_resp.into_inner().instances;
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, instance2.id.to_string());
    assert_eq!(instances[0].service_id, service_id);
    assert_eq!(instances[0].version, version2);
    assert!(instances[0].removed.is_none());

    // Find instance by a non-existent version - should return empty list
    let find_resp = env
        .api
        .find_instances_by_dpu_extension_service(Request::new(
            rpc::FindInstancesByDpuExtensionServiceRequest {
                service_id: service_id.clone(),
                version: Some(ConfigVersion::new(999).to_string()),
            },
        ))
        .await?;
    let instances = find_resp.into_inner().instances;
    assert_eq!(instances.len(), 0);

    // Find instance by a non-existent service -- should return error
    let find_resp = env
        .api
        .find_instances_by_dpu_extension_service(Request::new(
            rpc::FindInstancesByDpuExtensionServiceRequest {
                service_id: Uuid::new_v4().to_string(),
                version: None,
            },
        ))
        .await;
    assert!(find_resp.is_err());
    let status = find_resp.err().unwrap();
    assert!(status.message().contains("not found") || status.message().contains("NotFound"));

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_instances_by_extension_service_multiple_services_per_instance(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Create two extension services
    let service1 = create_test_extension_service_and_tenants(&env).await?;
    let service1_id = service1.service_id.clone();
    let service1_version = service1
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    let service2 = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: None,
            service_name: "test-service-2".to_string(),
            description: Some("Second test service".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    let service2_id = service2.service_id.clone();
    let service2_version = service2
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    // Create an instance using both services
    let (instance, _) = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .extension_services(rpc::InstanceDpuExtensionServicesConfig {
            service_configs: vec![
                rpc::InstanceDpuExtensionServiceConfig {
                    service_id: service1_id.clone(),
                    version: service1_version.clone(),
                },
                rpc::InstanceDpuExtensionServiceConfig {
                    service_id: service2_id.clone(),
                    version: service2_version.clone(),
                },
            ],
        })
        .build_and_return()
        .await;

    // Find instances by service1 - should return the instance
    let find_resp = env
        .api
        .find_instances_by_dpu_extension_service(Request::new(
            rpc::FindInstancesByDpuExtensionServiceRequest {
                service_id: service1_id.clone(),
                version: None,
            },
        ))
        .await?;

    let instances = find_resp.into_inner().instances;
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, instance.id.to_string());
    assert_eq!(instances[0].service_id, service1_id);
    assert_eq!(instances[0].version, service1_version);

    // Find instances by service2 - should also return the same instance
    let find_resp = env
        .api
        .find_instances_by_dpu_extension_service(Request::new(
            rpc::FindInstancesByDpuExtensionServiceRequest {
                service_id: service2_id.clone(),
                version: None,
            },
        ))
        .await?;

    let instances = find_resp.into_inner().instances;
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].instance_id, instance.id.to_string());
    assert_eq!(instances[0].service_id, service2_id);
    assert_eq!(instances[0].version, service2_version);

    Ok(())
}
