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
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use ::rpc::forge::dpu_extension_service_observability_config::Config;
use ::rpc::forge::forge_server::Forge;
use ::rpc::forge::{
    self as rpc, DpuExtensionServiceObservabilityConfig,
    DpuExtensionServiceObservabilityConfigLogging,
};
use carbide_dpf::{
    DetachedDpfResourceObservation, DetachedDpuServiceChainDefinition,
    DetachedDpuServiceDefinition, DetachedDpuServiceInterfaceDefinition, DpfError,
    DpuServiceDaemonSetObservation, DpuServiceHelmChartObservation, DpuServiceObservation,
    ServiceVpcSlots, detached_dpu_service_chain_spec, detached_dpu_service_interface_spec,
};
use carbide_extension_service_controller::dpu_service::{
    dpu_service_mutable_patch, project_dpu_service, project_service_vpc_registration,
};
use carbide_extension_service_controller::io::ExtensionServiceStateControllerIO;
use carbide_instrument::testing::MetricsCapture;
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_uuid::extension_service::ExtensionServiceId;
use config_version::ConfigVersion;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::extension_service::{
    DPF_HELM_CHART_OWNER_LABEL, ExtensionServiceDeploymentType, ExtensionServiceInterfaceMac,
    ExtensionServiceLifecycleState, ExtensionServiceType, ServiceVpcAddressFamily,
};
use model::tenant::TenantOrganizationId;
use state_controller::controller::Enqueuer;
use state_controller::io::StateControllerIO;
use tonic::Request;
use uuid::Uuid;

use crate::api::Api;
use crate::tests::common::api_fixtures::{
    TestEnv, TestEnvOverrides, create_managed_host, create_managed_host_with_dpf, create_test_env,
    create_test_env_with_overrides, get_config,
};
use crate::tests::dpf::phase_observation;

const TEST_SERVICE_DATA: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27";
const TEST_SERVICE_DATA_VERSION_2: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: version-2\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27";
const TEST_SERVICE_DATA_VERSION_3: &str = "apiVersion: v1\nkind: Pod\nmetadata:\n  name: version-3\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27";
const TEST_DPF_HELM_CHART_SERVICE_DATA: &str = r#"{
  "repoURL": "oci://registry.example.com/charts",
  "chartName": "tenant-service",
  "chartVersion": "1.2.3",
  "serviceID": "tenant-service-v1",
  "deployInCluster": false,
  "security": {"privileged": false, "spiffe": {}},
  "values": {"serviceDaemonSet": {"labels": {"chart-path": "preserved"}}},
  "serviceDaemonSet": {
    "labels": {"app.kubernetes.io/name": "tenant-service"},
    "annotations": {"example.com/owner": "tenant"},
    "resources": {"nvidia.com/bf_sf": 1},
    "updateStrategy": {"type": "RollingUpdate", "rollingUpdate": {"maxUnavailable": 1}}
  }
}"#;
const TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2: &str = r#"{
  "repoURL": "oci://registry.example.com/charts",
  "chartName": "tenant-service",
  "chartVersion": "2.0.0",
  "serviceID": "tenant-service-v1",
  "deployInCluster": false,
  "security": {"privileged": true},
  "values": {"replicas": 2, "serviceDaemonSet": {"labels": {"chart-path": "still-preserved"}}},
  "serviceDaemonSet": {
    "labels": {"app.kubernetes.io/name": "tenant-service-v2"},
    "annotations": {},
    "resources": {"nvidia.com/bf_sf": "2"},
    "updateStrategy": {"type": "OnDelete"}
  }
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

/// Reads the separately indexed DPF identity so API tests verify what uniqueness persists.
/// Response JSON intentionally retains the caller-submitted serviceID and cannot prove this column.
async fn stored_dpf_service_id(
    pool: &sqlx::PgPool,
    service_id: ExtensionServiceId,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT dpf_service_id FROM extension_service_versions \
         WHERE service_id = $1 AND deleted IS NULL",
    )
    .bind(service_id)
    .fetch_one(pool)
    .await
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

/// Builds a controller fixture with IPv4 registration enabled and a fixed slot inventory.
/// Propagates the deployment map into the controller to prove configured startup policy reaches
/// reconciliation decisions.
async fn create_service_vpc_controller_test_env(
    db_pool: sqlx::PgPool,
    dpf_sdk: MockDpfOperations,
    slot_count: u32,
) -> TestEnv {
    // Enable DPF with the requested slot capacity and prove global admission from generic BF4
    // alone.
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = slot_count;
    config.site_global_vpc_vni = None;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families = Default::default();
    let bf4 = crate::cfg::file::DpfDeploymentConfig {
        bfb_url: None,
        bluefield_software: Some(crate::cfg::file::DpfBlueFieldSoftwareConfig {
            os_iso: "https://example.com/bf4.iso".to_string(),
            pldm_fw_bundle: BTreeMap::from([(
                "MT_0000000999".to_string(),
                "https://example.com/bf4.fwpkg".to_string(),
            )]),
        }),
        flavor_name: "bf4-service-vpc-flavor".to_string(),
        deployment_name: "bf4-service-vpc-deployment".to_string(),
        node_label_key: "carbide.nvidia.com/bf4-service-vpc".to_string(),
        service_vpc_enabled_address_families: [ServiceVpcAddressFamily::Ipv4].into_iter().collect(),
        ..Default::default()
    };
    config.dpf.deployments.bf4_generic = Some(bf4);
    config.dpf.deployments.bf4_astra = None;
    create_test_env_with_overrides(
        db_pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(dpf_sdk)),
    )
    .await
}

/// Creates one IPv4 DPF registration through the public API so parent-conflict tests exercise
/// normal admission and persistence.
async fn create_networked_dpf_registration(
    env: &TestEnv,
    service_id: ExtensionServiceId,
    name: &str,
) -> Result<(), eyre::Report> {
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: name.to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    Ok(())
}

/// Advances a DPF registration to Ready so persistence-only update tests can skip external DPF
/// setup.
/// Direct database state keeps persistence-only update tests independent from DPF reconciliation.
async fn mark_dpf_service_ready_for_update(
    env: &TestEnv,
    service_id: ExtensionServiceId,
) -> Result<(), eyre::Report> {
    let mut txn = env.pool.begin().await?;
    let service = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("created DPF registration exists");
    let state_change = service.status.controller_state.version.incremental_change();

    // This transition is normally owned by reconciliation. Recording both the
    // state and its history keeps the fixture equivalent to that completed work.
    assert_eq!(
        db::extension_service::try_update_controller_state(
            &mut txn,
            service_id,
            state_change.current,
            state_change.new,
            &ExtensionServiceLifecycleState::Ready,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    db::state_history::persist(
        &mut txn,
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
        &ExtensionServiceLifecycleState::Ready,
        state_change.new,
    )
    .await?;
    txn.commit().await?;

    Ok(())
}

/// Converts a desired DPUService into a DpuServiceObservation with apiserver identity for
/// lifecycle-controller tests.
/// Populating immutable fields and deletion preconditions keeps ownership and deletion checks
/// realistic.
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
        deploy_in_cluster: service.deploy_in_cluster,
        dpu_cluster_selector_present: false,
        interfaces: service.interfaces.clone(),
        paused: None,
        security: Some(carbide_dpf::DpuServiceSecurityObservation {
            privileged: Some(service.security.privileged),
            spiffe: service.security.spiffe,
        }),
        service_daemon_set: service.service_daemon_set.as_ref().map(|daemon_set| {
            DpuServiceDaemonSetObservation {
                node_selector: daemon_set.node_selector_labels.as_ref().map(|labels| {
                    serde_json::json!({
                        "nodeSelectorTerms": [{
                            "matchExpressions": labels.iter().map(|(key, value)| serde_json::json!({
                                "key": key,
                                "operator": "In",
                                "values": [value],
                            })).collect::<Vec<_>>(),
                        }],
                    })
                }),
                annotations: daemon_set.annotations.clone(),
                labels: daemon_set.labels.clone(),
                resources: daemon_set.resources.clone(),
                update_strategy: daemon_set.update_strategy.as_ref().map(|strategy| {
                    let mut value = serde_json::Map::new();
                    if let Some(strategy_type) = &strategy.strategy_type {
                        value.insert("type".to_string(), serde_json::json!(strategy_type));
                    }
                    if let Some(rolling_update) = &strategy.rolling_update {
                        let mut rolling = serde_json::Map::new();
                        if let Some(max_surge) = &rolling_update.max_surge {
                            rolling.insert("maxSurge".to_string(), serde_json::json!(max_surge));
                        }
                        if let Some(max_unavailable) = &rolling_update.max_unavailable {
                            rolling.insert(
                                "maxUnavailable".to_string(),
                                serde_json::json!(max_unavailable),
                            );
                        }
                        value.insert(
                            "rollingUpdate".to_string(),
                            serde_json::Value::Object(rolling),
                        );
                    }
                    serde_json::Value::Object(value)
                }),
            }
        }),
        service_id: service.service_id.clone(),
        config_ports_present: false,
        uid: Some(format!("uid-{}", service.name)),
        resource_version: Some("1".to_string()),
        is_deleting: false,
    }
}

/// Converts a desired dynamic interface into a Kubernetes-like observation using the generated CR
/// spec so ownership checks match production conversion.
fn service_interface_observation(
    interface: &DetachedDpuServiceInterfaceDefinition,
) -> DetachedDpfResourceObservation {
    DetachedDpfResourceObservation {
        name: Some(interface.name.clone()),
        namespace: Some(interface.namespace.clone()),
        labels: interface.labels.clone(),
        spec: detached_dpu_service_interface_spec(interface).unwrap(),
        uid: Some(format!("uid-{}", interface.name)),
        resource_version: Some("1".to_string()),
        is_deleting: false,
    }
}

/// Converts a desired dynamic chain into a Kubernetes-like observation for controller tests.
/// Seeds UID and resource version so deletion tests can exercise conditional finalization.
fn service_chain_observation(
    chain: &DetachedDpuServiceChainDefinition,
) -> DetachedDpfResourceObservation {
    DetachedDpfResourceObservation {
        name: Some(chain.name.clone()),
        namespace: Some(chain.namespace.clone()),
        labels: chain.labels.clone(),
        spec: detached_dpu_service_chain_spec(chain).unwrap(),
        uid: Some(format!("uid-{}", chain.name)),
        resource_version: Some("1".to_string()),
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: name.to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential,
        observability: None,
        deployment_type: 0,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
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
                service_vpc_interfaces: None,
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
                service_vpc_interfaces: None,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(create_observability()),
        deployment_type: 0,
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
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
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

/// Verifies create normalizes the accepted V1 definition and persists Creating without invoking
/// DPF.
/// Protects durable acceptance from depending on synchronous controller work.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_persists_normalized_creating_state_without_dpf_call(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    // Submit non-contract field order and whitespace to prove persistence uses the parsed model's
    // normalized representation.
    let data = r#"{
        "security": {"privileged": false},
        "chartVersion": "1.2.3",
        "serviceID": "tenant-service-v1",
        "deployInCluster": false,
        "repoURL": "oci://registry.example.com/charts",
        "chartName": "tenant-service",
        "serviceDaemonSet": {
            "updateStrategy": {"rollingUpdate": {"maxUnavailable": 1}, "type": "RollingUpdate"},
            "resources": {"nvidia.com/bf_sf": 1},
            "annotations": {},
            "labels": {"app.kubernetes.io/name": "tenant-service"}
        }
    }"#;
    let response = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "accepted-dpf-service".to_string(),
            description: Some("durable acceptance only".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: data.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
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

    // Confirm the handler commits the sole V1 and Creating state without invoking reconciliation,
    // leaving DPF work to the periodic controller scan.
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
    assert_eq!(version.data, latest_version.data);
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

/// Verifies enabled but unsupported IPv6 retains its durable MAC while waiting and remains safely
/// deletable.
/// This isolates implementation qualification from operator enablement before any DPF parent can
/// escape.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_waits_then_deletes_before_reconciliation(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Make every external operation observable so waiting and deletion cannot create a resource.
    let service_id = ExtensionServiceId::new();
    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service().times(0);
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_delete_dpu_service().times(0);
    // Enable IPv6 with one compatible slot; IPv6 implementation support is the behavior-driving
    // difference.
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = 1;
    config.site_global_vpc_vni = None;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        [ServiceVpcAddressFamily::Ipv6].into_iter().collect();
    let env = create_test_env_with_overrides(
        db_pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    create_test_tenants(&env).await?;

    // Registration accepts the family declaration and persists it with Creating state.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            // Store All as this fixture's placement policy without resolving target DPUs until
            // activation.
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            // IPv6 reaches the implementation guard because the fixture explicitly enables it.
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv6 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "ipv6-networked-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            // Only DPF Helm chart services can register service-VPC interfaces.
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner();
    assert_eq!(lifecycle_state(&created), "creating");
    assert_eq!(created.service_vpc_interfaces.len(), 1);

    // The controller records a retryable wait and makes no DPF call.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("networked registration remains durable");
    assert_eq!(
        record.service_vpc_interfaces[0].address_family,
        ServiceVpcAddressFamily::Ipv6
    );
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        record.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason.contains("Ipv6") && reason.contains("not implemented")
    ));
    assert_eq!(
        db::extension_service::find_interface_macs(&mut txn, service_id)
            .await?
            .len(),
        1
    );
    txn.commit().await?;

    // Public deletion must atomically move the durable Creating declaration to Deleting.
    env.api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            versions: vec![],
        }))
        .await?;
    let mut txn = env.pool.begin().await?;
    let deleting = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("soft-deleted networked registration remains controller-visible");
    assert!(deleting.deleted.is_some());
    assert_eq!(
        deleting.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleting
    );
    txn.commit().await?;

    // Confirmed external absence completes deletion without a create or delete call.
    env.run_extension_service_controller_iteration().await;

    // A fresh persistence read proves the controller committed the terminal state.
    let mut txn = env.pool.begin().await?;
    let deleted = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("terminal networked registration remains stored");
    assert_eq!(
        deleted.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleted
    );
    assert!(
        db::extension_service::find_interface_macs(&mut txn, service_id)
            .await?
            .is_empty()
    );
    txn.commit().await?;

    Ok(())
}

/// Verifies MAC allocation advances deterministically after a global uniqueness collision.
/// The retry must remain in the create transaction so no networked declaration can persist without
/// identity.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_recovers_from_mac_collision(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Use the public API fixture so assignment failure would roll back the declaration transaction.
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;
    let blocker_id = ExtensionServiceId::new();
    let target_id = ExtensionServiceId::new();

    // Keep the blocker's chart identity distinct to isolate MAC uniqueness.
    let blocker_data =
        TEST_DPF_HELM_CHART_SERVICE_DATA.replace("tenant-service-v1", "mac-collision-blocker");

    // Create an unnetworked owner, then reserve the target's first deterministic candidate under
    // it.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: Some(blocker_id.to_string()),
            service_name: "mac-collision-blocker".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: blocker_data,
            credential: None,
            observability: None,
        }))
        .await?;
    let first_candidate = ExtensionServiceInterfaceMac::candidate(target_id, 0, 0);
    let blocker_assignment = ExtensionServiceInterfaceMac {
        service_id: blocker_id,
        ..first_candidate
    };
    let mut txn = env.pool.begin().await?;
    db::extension_service::insert_interface_mac(&mut txn, blocker_assignment).await?;
    txn.commit().await?;

    // Creating the target must skip the occupied candidate and commit the next stable value.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(target_id.to_string()),
            service_name: "mac-collision-target".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    assert_eq!(lifecycle_state(&created), "creating");

    // Reload both assignments to prove the collision owner was preserved and the target advanced
    // once.
    let mut connection = env.pool.acquire().await?;
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, blocker_id).await?,
        vec![blocker_assignment]
    );
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, target_id).await?,
        vec![ExtensionServiceInterfaceMac::candidate(target_id, 0, 1)]
    );

    Ok(())
}

/// Verifies enabled IPv4 reconciliation creates every two-slot parent in dependency order before
/// Ready.
/// Verifies DPUServiceInterface and DPUServiceChain creation precedes the consuming DPUService
/// while `project_service_vpc_registration` tests own field-level resource checks.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_reconciles_two_slot_inventory(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Record the consumed interface and resource kinds without repeating field-level graph
    // assertions.
    let service_id = ExtensionServiceId::new();
    let created_interfaces = Arc::new(Mutex::new(Vec::new()));
    let created_service = Arc::new(Mutex::new(None));
    let creation_order = Arc::new(Mutex::new(Vec::new()));
    let owner_selector = format!("{}={service_id}", DPF_HELM_CHART_OWNER_LABEL);
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning({
            let owner_selector = owner_selector.clone();
            move |selector| {
                assert_eq!(selector, Some(owner_selector.as_str()));
                Ok(Vec::new())
            }
        });
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(move |selector| {
            assert_eq!(selector, Some(owner_selector.as_str()));
            Ok(Vec::new())
        });
    mock.expect_get_detached_dpu_service_interface()
        .times(3)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_chain()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(3)
        .returning({
            let created_interfaces = Arc::clone(&created_interfaces);
            let creation_order = Arc::clone(&creation_order);
            move |interface| {
                creation_order.lock().unwrap().push("interface");
                created_interfaces.lock().unwrap().push(interface.clone());
                Ok(service_interface_observation(interface))
            }
        });
    mock.expect_create_detached_dpu_service_chain()
        .times(2)
        .returning({
            let creation_order = Arc::clone(&creation_order);
            move |chain| {
                creation_order.lock().unwrap().push("chain");
                Ok(service_chain_observation(chain))
            }
        });
    // Record DPUService creation separately because it completes the dormant registration
    // resource set.
    mock.expect_create_dpu_service().times(1).returning({
        let created_service = Arc::clone(&created_service);
        let creation_order = Arc::clone(&creation_order);
        move |service| {
            creation_order.lock().unwrap().push("service");
            *created_service.lock().unwrap() = Some(service.clone());
            Ok(dpu_service_observation(service))
        }
    });
    let env = create_service_vpc_controller_test_env(db_pool, mock, 2).await;
    create_test_tenants(&env).await?;

    // Persist one IPv4 requirement; its MAC is assigned in the same request transaction.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "networked-two-slot-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    assert_eq!(lifecycle_state(&created), "creating");

    // One controller pass can create all absent parents in dependency order and publish Ready.
    env.run_extension_service_controller_iteration().await;
    let persisted = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![service_id.to_string()],
        }))
        .await?
        .into_inner()
        .services
        .pop()
        .expect("reconciled registration remains visible");
    assert_eq!(lifecycle_state(&persisted), "ready");

    // Every interface precedes every chain, and the consumer is published only after all parents.
    assert_eq!(
        *creation_order.lock().unwrap(),
        vec![
            "interface",
            "interface",
            "interface",
            "chain",
            "chain",
            "service",
        ]
    );
    let service_interface_name = created_interfaces
        .lock()
        .unwrap()
        .first()
        .expect("the service interface is created first")
        .name
        .clone();
    let service = created_service
        .lock()
        .unwrap()
        .clone()
        .expect("DPUService is created after its parents");
    assert_eq!(service.interfaces, Some(vec![service_interface_name]));
    let mut connection = env.pool.acquire().await?;
    let mac = db::extension_service::find_interface_macs(&mut connection, service_id)
        .await?
        .pop()
        .expect("networked registration retains its MAC");
    let networks = service
        .service_daemon_set
        .as_ref()
        .and_then(|daemon_set| daemon_set.annotations.as_ref())
        .and_then(|annotations| annotations.get("k8s.v1.cni.cncf.io/networks"))
        .expect("networked DPUService carries its Multus selection");
    assert!(networks.contains("\"interface\":\"svc0\""));
    assert!(networks.contains(&mac.mac_address.to_string()));

    Ok(())
}

/// Verifies partial parent creation schedules immediate retries that reuse durable interfaces and
/// delay DPUService publication until its chain exists.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_retries_partial_parent_creation(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Stateful observations model failures after a partial interface phase and after all interfaces
    // exist.
    let service_id = ExtensionServiceId::new();
    let live_interfaces = Arc::new(Mutex::new(Vec::new()));
    let live_chains = Arc::new(Mutex::new(Vec::new()));
    let interface_attempts = Arc::new(AtomicUsize::new(0));
    let chain_attempts = Arc::new(AtomicUsize::new(0));
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(3)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(3)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            move |_| Ok(live_interfaces.lock().unwrap().clone())
        });
    mock.expect_list_detached_dpu_service_chains()
        .times(3)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            move |_| Ok(live_chains.lock().unwrap().clone())
        });
    mock.expect_get_detached_dpu_service_interface()
        .times(3)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_chain()
        .times(3)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(3)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            let interface_attempts = Arc::clone(&interface_attempts);
            move |interface| {
                // The second desired interface fails once after the first is already durable.
                if interface_attempts.fetch_add(1, Ordering::Relaxed) == 1 {
                    return Err(DpfError::timeout(
                        "create DPUServiceInterface",
                        "injected later-interface failure",
                    ));
                }
                let observed = service_interface_observation(interface);
                live_interfaces.lock().unwrap().push(observed.clone());
                Ok(observed)
            }
        });
    mock.expect_create_detached_dpu_service_chain()
        .times(2)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            let chain_attempts = Arc::clone(&chain_attempts);
            move |chain| {
                // Fail only the first chain create so retry must reuse the already-created
                // interfaces.
                if chain_attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                    return Err(DpfError::timeout(
                        "create DPUServiceChain",
                        "injected transient failure",
                    ));
                }
                let observed = service_chain_observation(chain);
                live_chains.lock().unwrap().push(observed.clone());
                Ok(observed)
            }
        });
    // Permit DPUService creation only after both prerequisite failures recover; one call proves
    // neither partial pass published the consumer.
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Register the desired resources and run one pass; the injected second-interface failure
    // leaves only the first prerequisite durable.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "networked-partial-parent-retry".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    // The later-interface failure is retryable, and no chain or consumer is published prematurely.
    let mut connection = env.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("partially reconciled registration remains durable");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPUServiceInterface creation failed; retrying"
    ));
    assert_eq!(live_interfaces.lock().unwrap().len(), 1);
    assert!(live_chains.lock().unwrap().is_empty());
    drop(connection);

    // Process only queued work so the retained first interface leads directly to the injected chain
    // failure.
    env.run_queued_extension_service_controller_iterations()
        .await;
    let mut connection = env.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("partially reconciled registration remains durable");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPUServiceChain creation failed; retrying"
    ));
    assert_eq!(live_interfaces.lock().unwrap().len(), 2);
    assert!(live_chains.lock().unwrap().is_empty());
    drop(connection);

    // Consume the second immediate retry so the chain and DPUService converge without a periodic
    // scan.
    env.run_queued_extension_service_controller_iterations()
        .await;
    let mut connection = env.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("recovered registration remains durable");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(live_interfaces.lock().unwrap().len(), 2);
    assert_eq!(live_chains.lock().unwrap().len(), 1);
    assert_eq!(interface_attempts.load(Ordering::Relaxed), 3);
    assert_eq!(chain_attempts.load(Ordering::Relaxed), 2);

    Ok(())
}

/// Accepts a same-owner service interface found by exact reread after POST conflict, allowing the
/// remaining parent graph to converge in the same iteration.
#[crate::sqlx_test]
async fn test_networked_registration_accepts_owned_interface_create_conflict(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Build the deterministic resource names and matching interface returned by the conflict
    // reread.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let desired = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let service_interface = desired.interfaces[0].clone();
    let patch_interface = desired.interfaces[1].clone();
    let chain = desired.chains[0].clone();
    let interface_reads = Arc::new(AtomicUsize::new(0));

    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(3)
        .returning({
            let interface_reads = Arc::clone(&interface_reads);
            let service_interface = service_interface.clone();
            let patch_name = patch_interface.name.clone();
            move |name| {
                let read = interface_reads.fetch_add(1, Ordering::Relaxed);
                match read {
                    0 => {
                        assert_eq!(name, service_interface.name);
                        Ok(None)
                    }
                    1 => {
                        assert_eq!(name, patch_name);
                        Ok(None)
                    }
                    2 => {
                        assert_eq!(name, service_interface.name);
                        Ok(Some(service_interface_observation(&service_interface)))
                    }
                    _ => unreachable!("unexpected interface reread"),
                }
            }
        });
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(2)
        .returning({
            let service_name = service_interface.name.clone();
            move |interface| {
                if interface.name == service_name {
                    Err(DpfError::already_exists(
                        "DPUServiceInterface",
                        interface.name.clone(),
                    ))
                } else {
                    Ok(service_interface_observation(interface))
                }
            }
        });
    mock.expect_create_detached_dpu_service_chain()
        .times(1)
        .returning(move |_| Ok(service_chain_observation(&chain)));
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;
    // Persist through the public API, then reconcile the interface POST conflict and exact reread.
    create_networked_dpf_registration(&env, service_id, "owned-interface-create-conflict").await?;

    env.run_extension_service_controller_iteration().await;

    // Reload the registration to prove the exact reread authorized adoption and consumer
    // publication.
    let mut connection = env.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after owned conflict");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(interface_reads.load(Ordering::Relaxed), 3);
    Ok(())
}

/// Rejects a foreign service interface found by exact reread after POST conflict, preventing
/// parent creation from continuing over an unowned attachment.
#[crate::sqlx_test]
async fn test_networked_registration_rejects_foreign_interface_create_conflict(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Build desired parents and replace only the conflicting interface's owner label.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let desired = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let service_interface = desired.interfaces[0].clone();
    let patch_interface = desired.interfaces[1].clone();
    let mut foreign_interface = service_interface_observation(&service_interface);
    foreign_interface.labels.insert(
        model::extension_service::DPF_HELM_CHART_OWNER_LABEL.to_string(),
        ExtensionServiceId::new().to_string(),
    );
    let interface_reads = Arc::new(AtomicUsize::new(0));

    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(3)
        .returning({
            let interface_reads = Arc::clone(&interface_reads);
            let service_name = service_interface.name.clone();
            let patch_name = patch_interface.name.clone();
            move |name| match interface_reads.fetch_add(1, Ordering::Relaxed) {
                0 => {
                    assert_eq!(name, service_name);
                    Ok(None)
                }
                1 => {
                    assert_eq!(name, patch_name);
                    Ok(None)
                }
                2 => {
                    assert_eq!(name, service_name);
                    Ok(Some(foreign_interface.clone()))
                }
                _ => unreachable!("unexpected interface reread"),
            }
        });
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(1)
        .returning(|interface| {
            Err(DpfError::already_exists(
                "DPUServiceInterface",
                interface.name.clone(),
            ))
        });
    mock.expect_create_detached_dpu_service_chain().times(0);
    mock.expect_create_dpu_service().times(0);
    mock.expect_patch_detached_dpu_service_interface().times(0);
    mock.expect_delete_detached_dpu_service_interface().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;
    // Persist through the public API, then reconcile the foreign interface returned after POST
    // conflict.
    create_networked_dpf_registration(&env, service_id, "foreign-interface-create-conflict")
        .await?;

    env.run_extension_service_controller_iteration().await;

    // Reload durable state to prove an unowned interface conflict fails before creating any
    // dependent chain or consumer.
    let mut connection = env.pool.acquire().await?;
    let failed = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after foreign interface conflict");
    assert_eq!(
        failed.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );
    assert_eq!(interface_reads.load(Ordering::Relaxed), 3);
    Ok(())
}

/// Rejects a foreign chain found by exact reread after POST conflict, preventing parent creation
/// from publishing a consumer over unowned wiring.
#[crate::sqlx_test]
async fn test_networked_registration_rejects_foreign_chain_create_conflict(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Build desired parents and replace only the conflicting chain's owner label.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let desired = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let chain = desired.chains[0].clone();
    let chain_name = chain.name.clone();
    let mut foreign_chain = service_chain_observation(&chain);
    foreign_chain.labels.insert(
        model::extension_service::DPF_HELM_CHART_OWNER_LABEL.to_string(),
        ExtensionServiceId::new().to_string(),
    );
    let chain_reads = Arc::new(AtomicUsize::new(0));

    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_chain()
        .times(2)
        .returning({
            let chain_reads = Arc::clone(&chain_reads);
            let chain_name = chain_name.clone();
            move |name| {
                assert_eq!(name, chain_name);
                if chain_reads.fetch_add(1, Ordering::Relaxed) == 0 {
                    Ok(None)
                } else {
                    Ok(Some(foreign_chain.clone()))
                }
            }
        });
    mock.expect_create_detached_dpu_service_interface()
        .times(2)
        .returning(|interface| Ok(service_interface_observation(interface)));
    mock.expect_create_detached_dpu_service_chain()
        .times(1)
        .returning(move |_| {
            Err(DpfError::already_exists(
                "DPUServiceChain",
                chain_name.clone(),
            ))
        });
    mock.expect_create_dpu_service().times(0);
    mock.expect_patch_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_chain().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;
    // Persist through the public API, then reconcile the foreign chain returned after POST
    // conflict.
    create_networked_dpf_registration(&env, service_id, "foreign-chain-create-conflict").await?;

    env.run_extension_service_controller_iteration().await;

    // Reload durable state to prove an unowned chain conflict fails without mutation or consumer
    // creation.
    let mut connection = env.pool.acquire().await?;
    let failed = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after foreign conflict");
    assert_eq!(
        failed.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );
    assert_eq!(chain_reads.load(Ordering::Relaxed), 2);
    Ok(())
}

/// Waits when exact reread finds an owned interface already terminating, preventing
/// deterministic-name reuse before Kubernetes confirms absence.
#[crate::sqlx_test]
async fn test_networked_registration_waits_for_terminating_interface_create_conflict(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Build an owned service interface whose deletion timestamp is visible only after the create
    // conflict.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let desired = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let service_interface = desired.interfaces[0].clone();
    let patch_name = desired.interfaces[1].name.clone();
    let mut terminating = service_interface_observation(&service_interface);
    terminating.is_deleting = true;
    let interface_reads = Arc::new(AtomicUsize::new(0));

    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(3)
        .returning({
            let interface_reads = Arc::clone(&interface_reads);
            let service_name = service_interface.name.clone();
            move |name| {
                let read = interface_reads.fetch_add(1, Ordering::Relaxed);
                match read {
                    0 => {
                        assert_eq!(name, service_name);
                        Ok(None)
                    }
                    1 => {
                        assert_eq!(name, patch_name);
                        Ok(None)
                    }
                    2 => {
                        assert_eq!(name, service_name);
                        Ok(Some(terminating.clone()))
                    }
                    _ => unreachable!("unexpected interface reread"),
                }
            }
        });
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(1)
        .returning(move |_| {
            Err(DpfError::already_exists(
                "DPUServiceInterface",
                service_interface.name.clone(),
            ))
        });
    mock.expect_create_detached_dpu_service_chain().times(0);
    mock.expect_create_dpu_service().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;
    // Persist through the public API, then reconcile the terminating interface returned after
    // conflict.
    create_networked_dpf_registration(&env, service_id, "terminating-interface-create-conflict")
        .await?;

    env.run_extension_service_controller_iteration().await;

    // Reload the registration to prove finalization blocks every dependent parent and the consumer.
    let mut connection = env.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable while the interface terminates");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPUServiceInterface finalization is in progress; retrying"
    ));
    assert_eq!(interface_reads.load(Ordering::Relaxed), 3);
    Ok(())
}

/// Verifies a desired interface retained by a finalizer cannot satisfy registration readiness.
/// Reusing its deterministic name is safe only after Kubernetes confirms the terminating object is
/// absent.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_waits_for_desired_interface_finalizer(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Build a complete one-slot graph so the deleting interface is the only behavior-driving
    // difference.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projection = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let service = dpu_service_observation(&projection.service);
    let stable_interface = service_interface_observation(&projection.interfaces[0]);
    let mut terminating_interface = service_interface_observation(&projection.interfaces[1]);
    // Hold the desired slot interface in finalization so its deterministic name cannot be reused
    // yet.
    terminating_interface.is_deleting = true;
    let chain = service_chain_observation(&projection.chains[0]);
    let interface_lists = Arc::new(AtomicUsize::new(0));
    let interface_creates = Arc::new(AtomicUsize::new(0));

    // First observation retains the terminating interface; the second confirms its absence.
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(2)
        .returning(move |_| Ok(Some(service.clone())));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(2)
        .returning({
            let interface_lists = Arc::clone(&interface_lists);
            move |_| {
                if interface_lists.fetch_add(1, Ordering::Relaxed) == 0 {
                    Ok(vec![
                        stable_interface.clone(),
                        terminating_interface.clone(),
                    ])
                } else {
                    Ok(vec![stable_interface.clone()])
                }
            }
        });
    mock.expect_list_detached_dpu_service_chains()
        .times(2)
        .returning(move |_| Ok(vec![chain.clone()]));
    mock.expect_get_detached_dpu_service_interface()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(1)
        .returning({
            let interface_creates = Arc::clone(&interface_creates);
            move |interface| {
                interface_creates.fetch_add(1, Ordering::Relaxed);
                Ok(service_interface_observation(interface))
            }
        });
    mock.expect_create_detached_dpu_service_chain().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist the matching desired registration whose patch interface is terminating externally.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_id: Some(service_id.to_string()),
            service_name: "terminating-interface-registration".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;

    // A matching but terminating interface must persist a wait and suppress same-name creation.
    env.run_extension_service_controller_iteration().await;
    let mut connection = env.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable during finalization");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason.contains("interface finalization")
    ));
    assert_eq!(interface_creates.load(Ordering::Relaxed), 0);
    drop(connection);

    // Confirmed absence permits one replacement and only then completes registration.
    env.run_extension_service_controller_iteration().await;
    let mut connection = env.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after replacement");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(interface_creates.load(Ordering::Relaxed), 1);

    Ok(())
}

/// Verifies a desired chain retained by a finalizer cannot satisfy registration readiness.
/// The controller must observe absence before creating the same deterministic chain name again.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_waits_for_desired_chain_finalizer(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Build a complete one-slot graph so the deleting chain is the only behavior-driving
    // difference.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projection = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let service = dpu_service_observation(&projection.service);
    let interfaces = projection
        .interfaces
        .iter()
        .map(service_interface_observation)
        .collect::<Vec<_>>();
    let mut terminating_chain = service_chain_observation(&projection.chains[0]);
    // Hold the desired chain in finalization so creation must wait for confirmed absence.
    terminating_chain.is_deleting = true;
    let chain_lists = Arc::new(AtomicUsize::new(0));
    let chain_creates = Arc::new(AtomicUsize::new(0));

    // Ready and its queued Updating retry both retain the chain; the later scan confirms absence.
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(3)
        .returning(move |_| Ok(Some(service.clone())));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(3)
        .returning(move |_| Ok(interfaces.clone()));
    mock.expect_list_detached_dpu_service_chains()
        .times(3)
        .returning({
            let chain_lists = Arc::clone(&chain_lists);
            move |_| {
                if chain_lists.fetch_add(1, Ordering::Relaxed) < 2 {
                    Ok(vec![terminating_chain.clone()])
                } else {
                    Ok(Vec::new())
                }
            }
        });
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_chain()
        .times(1)
        .returning({
            let chain_creates = Arc::clone(&chain_creates);
            move |chain| {
                chain_creates.fetch_add(1, Ordering::Relaxed);
                Ok(service_chain_observation(chain))
            }
        });
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_patch_dpu_service().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist the matching desired registration whose only chain is terminating externally.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_id: Some(service_id.to_string()),
            service_name: "terminating-chain-registration".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    // Start from Ready so the first scan must publish the Updating barrier before replacement.
    mark_dpf_service_ready_for_update(&env, service_id).await?;

    // Ready must first publish an Updating barrier before observing finalization again.
    env.run_extension_service_controller_iteration().await;
    let mut connection = env.pool.acquire().await?;
    let updating = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable during finalization");
    assert_eq!(
        updating.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        updating.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    assert_eq!(chain_creates.load(Ordering::Relaxed), 0);
    drop(connection);

    // The transition-driven retry sees the same finalizer and performs no same-name create.
    env.run_queued_extension_service_controller_iterations()
        .await;
    let mut connection = env.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable while finalizer is present");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason.contains("chain finalization")
    ));
    assert_eq!(chain_creates.load(Ordering::Relaxed), 0);
    drop(connection);

    // Confirmed absence permits one replacement and only then completes registration.
    env.run_extension_service_controller_iteration().await;
    let mut connection = env.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after replacement");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(chain_creates.load(Ordering::Relaxed), 1);

    Ok(())
}

/// Verifies owner-scoped lists do not hide a foreign object at a desired deterministic name.
/// Exact GET must preserve the permanent ownership failure before any parent mutation occurs.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_rejects_foreign_parent_after_scoped_list(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projection = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let foreign_name = projection.interfaces[0].name.clone();
    let mut foreign = service_interface_observation(&projection.interfaces[0]);
    // Change only the owner label so exact lookup observes a foreign same-name interface.
    foreign.labels.insert(
        DPF_HELM_CHART_OWNER_LABEL.to_string(),
        ExtensionServiceId::new().to_string(),
    );

    // Scoped lists omit the foreign object; exact lookup exposes it before creation starts.
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(2)
        .returning(move |name| {
            if name == foreign_name.as_str() {
                Ok(Some(foreign.clone()))
            } else {
                Ok(None)
            }
        });
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    mock.expect_create_dpu_service().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist one networked declaration, then reconcile its occupied deterministic name through
    // exact interface lookup.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_id: Some(service_id.to_string()),
            service_name: "foreign-parent-registration".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    // A durable failure proves the foreign resource was rejected rather than adopted or retried.
    let mut connection = env.pool.acquire().await?;
    let failed = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains visible after ownership failure");
    assert_eq!(
        failed.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );

    Ok(())
}

/// Verifies owner-scoped chain listing cannot hide a foreign desired-name resource.
/// Exact lookup must fail permanently before NICo creates or deletes any detached parent.
#[crate::sqlx_test]
async fn test_networked_dpf_registration_rejects_foreign_chain_after_scoped_list(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projection = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let foreign_name = projection.chains[0].name.clone();
    let mut foreign = service_chain_observation(&projection.chains[0]);
    // Change only the owner label so exact lookup observes a foreign same-name chain.
    foreign.labels.insert(
        DPF_HELM_CHART_OWNER_LABEL.to_string(),
        ExtensionServiceId::new().to_string(),
    );

    // Scoped lists omit the foreign chain; exact lookup exposes it before parent mutation.
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(move |name| {
            assert_eq!(name, foreign_name.as_str());
            Ok(Some(foreign.clone()))
        });
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_interface().times(0);
    mock.expect_delete_detached_dpu_service_chain().times(0);
    mock.expect_create_dpu_service().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist one declaration, then reconcile its occupied deterministic name through exact chain
    // lookup.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_id: Some(service_id.to_string()),
            service_name: "foreign-chain-registration".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    // Confirm Failed persists the ownership rejection without adopting or mutating the same-name
    // resource.
    let mut connection = env.pool.acquire().await?;
    let failed = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains visible after chain ownership failure");
    assert_eq!(
        failed.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );

    Ok(())
}

/// Verifies disabled IPv4 keeps its durable declaration and resumes with the same MAC after
/// rollout.
/// This protects retryable admission semantics across process restart without exposing parents
/// while disabled.
#[crate::sqlx_test]
async fn test_disabled_networked_registration_resumes_after_enabled_restart(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Start with the deployment family omitted so the first controller has no admitted DPF work.
    let service_id = ExtensionServiceId::new();
    let initial_env = create_dpf_controller_test_env(db_pool, MockDpfOperations::new()).await;
    create_test_tenants(&initial_env).await?;

    // The declaration and MAC commit even though startup policy defers every external resource.
    initial_env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "disabled-networked-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    initial_env
        .run_extension_service_controller_iteration()
        .await;
    let mut connection = initial_env.pool.acquire().await?;
    let original_mac = db::extension_service::find_interface_macs(&mut connection, service_id)
        .await?
        .pop()
        .expect("disabled registration retains allocated identity");
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("disabled registration remains durable");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason.contains("disabled")
    ));
    drop(connection);

    // Restart Core with IPv4 and one slot enabled; the same durable row now creates its parents.
    let restart_pool = initial_env.pool.clone();
    drop(initial_env);
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(2)
        .returning(|interface| Ok(service_interface_observation(interface)));
    mock.expect_create_detached_dpu_service_chain()
        .times(1)
        .returning(|chain| Ok(service_chain_observation(chain)));
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    // Admit IPv4 with one compatible slot so the retained Creating row can resume.
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = 1;
    config.site_global_vpc_vni = None;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        [ServiceVpcAddressFamily::Ipv4].into_iter().collect();
    let mut overrides = TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock));
    // Reuse the persisted network fixtures instead of recreating them during restart.
    overrides.create_network_segments = Some(false);
    let restarted = create_test_env_with_overrides(restart_pool, overrides).await;
    restarted.run_extension_service_controller_iteration().await;

    // The resumed registration reaches Ready without reallocating its ordinal-zero identity.
    let mut connection = restarted.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("resumed registration remains durable");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, service_id).await?,
        vec![original_mac]
    );

    Ok(())
}

/// Verifies a Ready registration repairs slot growth after family admission is disabled.
/// This protects accepted topology across rollout because unchanged Ready state alone would not
/// prove repair ran.
#[crate::sqlx_test]
async fn test_ready_networked_registration_repairs_expanded_slots_after_family_disable(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Establish the durable identity under the original one-slot startup inventory.
    let service_id = ExtensionServiceId::new();
    let initial_env =
        create_service_vpc_controller_test_env(db_pool, MockDpfOperations::new(), 1).await;
    create_test_tenants(&initial_env).await?;

    // Persist a networked registration and stand in for its original one-slot convergence.
    initial_env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "ready-slot-growth-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    mark_dpf_service_ready_for_update(&initial_env, service_id).await?;
    let mut connection = initial_env.pool.acquire().await?;
    let mac = db::extension_service::find_interface_macs(&mut connection, service_id)
        .await?
        .pop()
        .expect("networked registration has stable identity");
    drop(connection);
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let original = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        mac,
        ServiceVpcSlots::new(1)?,
    )?;
    let interfaces = Arc::new(Mutex::new(
        original
            .interfaces
            .iter()
            .map(service_interface_observation)
            .collect::<Vec<_>>(),
    ));
    let chains = Arc::new(Mutex::new(
        original
            .chains
            .iter()
            .map(service_chain_observation)
            .collect::<Vec<_>>(),
    ));
    let observed_service = dpu_service_observation(&original.service);

    // Restart with two slots and family admission disabled; accepted Ready state still requires
    // repair.
    let restart_pool = initial_env.pool.clone();
    drop(initial_env);
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(2)
        .returning(move |_| Ok(Some(observed_service.clone())));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(2)
        .returning({
            let interfaces = Arc::clone(&interfaces);
            move |_| Ok(interfaces.lock().unwrap().clone())
        });
    mock.expect_list_detached_dpu_service_chains()
        .times(2)
        .returning({
            let chains = Arc::clone(&chains);
            move |_| Ok(chains.lock().unwrap().clone())
        });
    mock.expect_get_detached_dpu_service_interface()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_chain()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(1)
        .returning({
            let interfaces = Arc::clone(&interfaces);
            move |interface| {
                let observed = service_interface_observation(interface);
                interfaces.lock().unwrap().push(observed.clone());
                Ok(observed)
            }
        });
    mock.expect_create_detached_dpu_service_chain()
        .times(1)
        .returning({
            let chains = Arc::clone(&chains);
            move |chain| {
                let observed = service_chain_observation(chain);
                chains.lock().unwrap().push(observed.clone());
                Ok(observed)
            }
        });
    mock.expect_patch_dpu_service().times(0);
    // Leave family admission disabled while increasing only the compatible slot inventory.
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = 2;
    config.site_global_vpc_vni = None;
    let mut overrides = TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock));
    // Reuse the persisted network fixtures instead of recreating them during restart.
    overrides.create_network_segments = Some(false);
    let restarted = create_test_env_with_overrides(restart_pool, overrides).await;

    // The first pass publishes the non-Ready barrier before mutating the external parent graph.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let updating = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable during inventory repair");
    assert_eq!(
        updating.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        updating.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    assert_eq!(interfaces.lock().unwrap().len(), 2);
    assert_eq!(chains.lock().unwrap().len(), 1);
    drop(connection);

    // The transition itself queued the repair; process it without another periodic scan.
    let enqueuer = Enqueuer::<ExtensionServiceStateControllerIO>::new(restarted.pool.clone());
    assert!(!enqueuer.enqueue_object(&service_id).await?);
    restarted
        .run_queued_extension_service_controller_iterations()
        .await;

    // Successful parent creation restores Ready without an unnecessary DPUService patch.
    let mut connection = restarted.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("repaired registration remains durable");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(interfaces.lock().unwrap().len(), 3);
    assert_eq!(chains.lock().unwrap().len(), 2);
    let expected_patch = format!("extsvc-{service_id}-if0-slot1-patch");
    let expected_chain = format!("extsvc-{service_id}-if0-slot1-chain");
    assert!(
        interfaces
            .lock()
            .unwrap()
            .iter()
            .any(|resource| { resource.name.as_deref() == Some(expected_patch.as_str()) })
    );
    assert!(
        chains
            .lock()
            .unwrap()
            .iter()
            .any(|resource| { resource.name.as_deref() == Some(expected_chain.as_str()) })
    );

    Ok(())
}

/// Verifies mutable DPUService drift makes a networked Ready registration visibly non-Ready before
/// repair.
/// The barrier prevents consumers from treating stale tenant runtime settings as a converged
/// registration.
#[crate::sqlx_test]
async fn test_ready_networked_registration_repairs_mutable_dpu_service_drift(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Persist a Ready one-slot registration and its allocated attachment identity.
    let service_id = ExtensionServiceId::new();
    let initial_env =
        create_service_vpc_controller_test_env(db_pool, MockDpfOperations::new(), 1).await;
    create_test_tenants(&initial_env).await?;
    initial_env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            // Networking keeps Ready on the registration reconciler's periodic repair path.
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "ready-mutable-drift-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    mark_dpf_service_ready_for_update(&initial_env, service_id).await?;
    let mac = db::extension_service::find_interface_macs(
        &mut *initial_env.pool.acquire().await?,
        service_id,
    )
    .await?
    .pop()
    .expect("networked registration has stable identity");

    // Change only a patch-owned security field in the observed DPUService.
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projection = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        mac,
        ServiceVpcSlots::new(1)?,
    )?;
    let mut drifted_service = dpu_service_observation(&projection.service);
    drifted_service
        .security
        .as_mut()
        .expect("projected service includes security")
        .privileged = Some(!projection.service.security.privileged);
    let expected_patch_v1 = dpu_service_mutable_patch(&projection.service, &drifted_service, "1");
    let expected_patch_v2 = dpu_service_mutable_patch(&projection.service, &drifted_service, "2");
    let expected_patch_v3 = dpu_service_mutable_patch(&projection.service, &drifted_service, "3");
    let converged_service = dpu_service_observation(&projection.service);
    let interfaces = projection
        .interfaces
        .iter()
        .map(service_interface_observation)
        .collect::<Vec<_>>();
    let chains = projection
        .chains
        .iter()
        .map(service_chain_observation)
        .collect::<Vec<_>>();

    // The Ready pass uses its existing observation only; the queued Updating pass verifies parents
    // and patches.
    let restart_pool = initial_env.pool.clone();
    drop(initial_env);
    let expected_owner_selector = format!("{DPF_HELM_CHART_OWNER_LABEL}={service_id}");
    let live_service = Arc::new(Mutex::new(drifted_service));
    let patch_attempts = Arc::new(AtomicUsize::new(0));
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service().times(4).returning({
        let live_service = Arc::clone(&live_service);
        move |_| Ok(Some(live_service.lock().unwrap().clone()))
    });
    mock.expect_list_detached_dpu_service_interfaces()
        .times(3)
        .withf({
            let expected_owner_selector = expected_owner_selector.clone();
            move |selector| *selector == Some(expected_owner_selector.as_str())
        })
        .returning(move |_| Ok(interfaces.clone()));
    mock.expect_list_detached_dpu_service_chains()
        .times(3)
        .withf(move |selector| *selector == Some(expected_owner_selector.as_str()))
        .returning(move |_| Ok(chains.clone()));
    mock.expect_get_detached_dpu_service_interface().times(0);
    mock.expect_get_detached_dpu_service_chain().times(0);
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    let expected_service_name = format!("extsvc-{service_id}");
    mock.expect_patch_dpu_service().times(3).returning({
        let live_service = Arc::clone(&live_service);
        let patch_attempts = Arc::clone(&patch_attempts);
        move |name, patch| {
            assert_eq!(name, expected_service_name.as_str());
            match patch_attempts.fetch_add(1, Ordering::Relaxed) {
                0 => {
                    assert_eq!(&patch, &expected_patch_v1);
                    // Simulate a competing status writer so the retry must use a fresh revision.
                    live_service.lock().unwrap().resource_version = Some("2".to_string());
                    Err(DpfError::PatchConflict {
                        kind: "DPUService",
                        name: name.to_string(),
                    })
                }
                1 => {
                    assert_eq!(&patch, &expected_patch_v2);
                    // Transport success without actual convergence must remain retryable.
                    live_service.lock().unwrap().resource_version = Some("3".to_string());
                    Ok(live_service.lock().unwrap().clone())
                }
                2 => {
                    assert_eq!(&patch, &expected_patch_v3);
                    // Only a converged PATCH response can restore the durable Ready promise.
                    *live_service.lock().unwrap() = converged_service.clone();
                    Ok(converged_service.clone())
                }
                attempt => panic!("unexpected DPUService patch attempt {attempt}"),
            }
        }
    });
    // Preserve one-slot IPv4 admission so only mutable DPUService drift triggers repair.
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = 1;
    config.site_global_vpc_vni = None;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        [ServiceVpcAddressFamily::Ipv4].into_iter().collect();
    let mut overrides = TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock));
    // Reuse the persisted network fixtures instead of recreating them during restart.
    overrides.create_network_segments = Some(false);
    let restarted = create_test_env_with_overrides(restart_pool, overrides).await;

    // Ready first transitions without patching, making repair an explicit persisted lifecycle
    // barrier.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let updating = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable during mutable repair");
    assert_eq!(
        updating.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    drop(connection);

    // The first Updating pass receives a stale-revision failure and must persist a retryable Wait.
    restarted
        .run_queued_extension_service_controller_iterations()
        .await;
    let mut connection = restarted.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after a patch retry");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPUService update failed; retrying"
    ));
    drop(connection);

    // The next scan rereads revision 2, but a nonconverged success response must remain Updating.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after a nonconverged patch response");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPUService update response has not converged; retrying"
    ));
    drop(connection);

    // A fresh revision-fenced retry returns the desired object and finally restores Ready.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after mutable repair");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(patch_attempts.load(Ordering::Relaxed), 3);

    Ok(())
}

/// Verifies mutable parent drift leaves Ready before revision-fenced repair preserves chain wiring
/// and service-interface placement.
#[crate::sqlx_test]
async fn test_ready_networked_registration_repairs_mutable_parent_drift(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Persist a Ready one-slot registration and retain the MAC used to reconstruct its desired
    // graph.
    let service_id = ExtensionServiceId::new();
    let initial_env =
        create_service_vpc_controller_test_env(db_pool, MockDpfOperations::new(), 1).await;
    create_test_tenants(&initial_env).await?;
    initial_env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "ready-parent-drift-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    mark_dpf_service_ready_for_update(&initial_env, service_id).await?;
    let mac = db::extension_service::find_interface_macs(
        &mut *initial_env.pool.acquire().await?,
        service_id,
    )
    .await?
    .pop()
    .expect("networked registration has stable identity");
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projection = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        mac,
        ServiceVpcSlots::new(1)?,
    )?;

    // Change only service-interface placement and chain selector/MTU while retaining attachment
    // identity.
    let mut alternate_interface = projection.interfaces[0].clone();
    alternate_interface.node_selector_labels.insert(
        "example.com/stale-service".to_string(),
        "enabled".to_string(),
    );
    let mut alternate_chain = projection.chains[0].clone();
    alternate_chain
        .node_selector_labels
        .insert("example.com/stale-slot".to_string(), "enabled".to_string());
    alternate_chain.service_mtu += 1;
    let mut interfaces = projection
        .interfaces
        .iter()
        .map(service_interface_observation)
        .collect::<Vec<_>>();
    interfaces[0] = service_interface_observation(&alternate_interface);
    let mut chains = projection
        .chains
        .iter()
        .map(service_chain_observation)
        .collect::<Vec<_>>();
    chains[0] = service_chain_observation(&alternate_chain);
    let live_interfaces = Arc::new(Mutex::new(interfaces));
    let live_chains = Arc::new(Mutex::new(chains));
    let interface_patch_attempts = Arc::new(AtomicUsize::new(0));
    let chain_patch_attempts = Arc::new(AtomicUsize::new(0));

    // The Ready pass observes drift; the queued Updating pass repairs from those revision-bearing
    // observations.
    let restart_pool = initial_env.pool.clone();
    drop(initial_env);
    let mut mock = MockDpfOperations::new();
    let observed_service = dpu_service_observation(&projection.service);
    mock.expect_get_dpu_service()
        .times(4)
        .returning(move |_| Ok(Some(observed_service.clone())));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(4)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            move |_| Ok(live_interfaces.lock().unwrap().clone())
        });
    mock.expect_list_detached_dpu_service_chains()
        .times(4)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            move |_| Ok(live_chains.lock().unwrap().clone())
        });
    mock.expect_get_detached_dpu_service_interface().times(0);
    mock.expect_get_detached_dpu_service_chain().times(0);
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_interface().times(0);
    mock.expect_delete_detached_dpu_service_chain().times(0);
    mock.expect_patch_dpu_service().times(0);
    let desired_interface = projection.interfaces[0].clone();
    mock.expect_patch_detached_dpu_service_interface()
        .times(2)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            let interface_patch_attempts = Arc::clone(&interface_patch_attempts);
            move |name, patch| {
                assert_eq!(name, desired_interface.name);
                let attempt = interface_patch_attempts.fetch_add(1, Ordering::Relaxed);
                assert_eq!(patch[0]["op"], "replace");
                assert_eq!(patch[0]["value"], if attempt == 0 { "1" } else { "2" });
                assert_eq!(
                    patch[1]["value"],
                    detached_dpu_service_interface_spec(&desired_interface).unwrap()
                );
                if attempt == 0 {
                    live_interfaces.lock().unwrap()[0].resource_version = Some("2".to_string());
                    return Err(DpfError::PatchConflict {
                        kind: "DPUServiceInterface",
                        name: name.to_string(),
                    });
                }
                let mut repaired = service_interface_observation(&desired_interface);
                repaired.resource_version = Some("3".to_string());
                live_interfaces.lock().unwrap()[0] = repaired.clone();
                Ok(repaired)
            }
        });
    let desired_chain = projection.chains[0].clone();
    mock.expect_patch_detached_dpu_service_chain()
        .times(2)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            let chain_patch_attempts = Arc::clone(&chain_patch_attempts);
            move |name, patch| {
                assert_eq!(name, desired_chain.name);
                let attempt = chain_patch_attempts.fetch_add(1, Ordering::Relaxed);
                assert_eq!(patch[0]["op"], "replace");
                assert_eq!(patch[0]["value"], if attempt == 0 { "1" } else { "2" });
                assert_eq!(
                    patch[1]["value"],
                    detached_dpu_service_chain_spec(&desired_chain).unwrap()
                );
                if attempt == 0 {
                    live_chains.lock().unwrap()[0].resource_version = Some("2".to_string());
                    return Err(DpfError::PatchConflict {
                        kind: "DPUServiceChain",
                        name: name.to_string(),
                    });
                }
                let mut repaired = service_chain_observation(&desired_chain);
                repaired.resource_version = Some("3".to_string());
                live_chains.lock().unwrap()[0] = repaired.clone();
                Ok(repaired)
            }
        });
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = 1;
    config.site_global_vpc_vni = None;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        [ServiceVpcAddressFamily::Ipv4].into_iter().collect();
    let mut overrides = TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock));
    // Reuse persisted network fixtures so restart exercises only registration reconciliation.
    overrides.create_network_segments = Some(false);
    let restarted = create_test_env_with_overrides(restart_pool, overrides).await;

    // Ready publishes the durable barrier before either parent PATCH is attempted.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let updating = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable during parent repair");
    assert_eq!(
        updating.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert_eq!(interface_patch_attempts.load(Ordering::Relaxed), 0);
    assert_eq!(chain_patch_attempts.load(Ordering::Relaxed), 0);
    drop(connection);

    // The queued Updating pass loses the chain race and remains durable with no interface update.
    restarted
        .run_queued_extension_service_controller_iterations()
        .await;
    let mut connection = restarted.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after the chain conflict");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "service-VPC chain update failed; retrying"
    ));
    assert_eq!(chain_patch_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(interface_patch_attempts.load(Ordering::Relaxed), 0);
    drop(connection);

    // A fresh chain revision succeeds before the interface loses its own independent revision race.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after the interface conflict");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "service-VPC interface update failed; retrying"
    ));
    assert_eq!(chain_patch_attempts.load(Ordering::Relaxed), 2);
    assert_eq!(interface_patch_attempts.load(Ordering::Relaxed), 1);
    drop(connection);

    // The next observation supplies the interface's refreshed revision and completes both repairs.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after parent repair");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(interface_patch_attempts.load(Ordering::Relaxed), 2);
    assert_eq!(chain_patch_attempts.load(Ordering::Relaxed), 2);

    Ok(())
}

/// Fails a Ready registration whose owned service interface names a different NAD, preventing
/// in-place rewiring of tenant connectivity.
#[crate::sqlx_test]
async fn test_ready_networked_registration_rejects_service_interface_network_drift(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Persist a Ready registration and recover the MAC needed to reconstruct its expected DPF
    // resources.
    let service_id = ExtensionServiceId::new();
    let initial_env =
        create_service_vpc_controller_test_env(db_pool, MockDpfOperations::new(), 1).await;
    create_test_tenants(&initial_env).await?;
    initial_env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "ready-interface-network-drift".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    mark_dpf_service_ready_for_update(&initial_env, service_id).await?;
    let mac = db::extension_service::find_interface_macs(
        &mut *initial_env.pool.acquire().await?,
        service_id,
    )
    .await?
    .pop()
    .expect("networked registration has stable identity");
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let desired = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        mac,
        ServiceVpcSlots::new(1)?,
    )?;

    // Change only the service-side NAD so labels and every mutable interface field remain valid.
    let mut rewired = desired.interfaces[0].clone();
    let carbide_dpf::DetachedDpuServiceInterface::Service { network, .. } = &mut rewired.interface
    else {
        panic!("the first interface is service-side");
    };
    *network = format!("{}/foreign-service-vpc", carbide_dpf::NAMESPACE);
    let mut observed_interfaces = desired
        .interfaces
        .iter()
        .map(service_interface_observation)
        .collect::<Vec<_>>();
    observed_interfaces[0] = service_interface_observation(&rewired);

    // Restart with the drifted interface to force permanent rejection before any parent mutation.
    let restart_pool = initial_env.pool.clone();
    drop(initial_env);
    let mut mock = MockDpfOperations::new();
    let observed_service = dpu_service_observation(&desired.service);
    mock.expect_get_dpu_service()
        .times(1)
        .returning(move |_| Ok(Some(observed_service.clone())));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .return_once(move |_| Ok(observed_interfaces));
    let observed_chains = desired
        .chains
        .iter()
        .map(service_chain_observation)
        .collect::<Vec<_>>();
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .return_once(move |_| Ok(observed_chains));
    mock.expect_get_detached_dpu_service_interface().times(0);
    mock.expect_get_detached_dpu_service_chain().times(0);
    mock.expect_patch_detached_dpu_service_interface().times(0);
    mock.expect_delete_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_patch_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_chain().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    mock.expect_patch_dpu_service().times(0);
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = 1;
    config.site_global_vpc_vni = None;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        [ServiceVpcAddressFamily::Ipv4].into_iter().collect();
    let mut overrides = TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock));
    overrides.create_network_segments = Some(false);
    let restarted = create_test_env_with_overrides(restart_pool, overrides).await;

    restarted.run_extension_service_controller_iteration().await;

    // Reload durable state to prove immutable NAD drift takes the permanent failure path.
    let mut connection = restarted.pool.acquire().await?;
    let failed = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("registration remains durable after ownership rejection");
    assert_eq!(
        failed.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );
    Ok(())
}

/// Verifies an endpoint-free instance reference permits disabled-family repair and parent shrink
/// without claiming active or terminating slot safety.
#[crate::sqlx_test]
async fn test_endpoint_free_reference_repairs_shrunk_slots_after_family_disable(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Establish a Ready two-slot registration and the durable attachment consumed by repair policy.
    let service_id = ExtensionServiceId::new();
    let mut initial_mock = MockDpfOperations::new();
    initial_mock
        .expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    initial_mock
        .expect_register_dpu_node()
        .returning(|_| Ok(()));
    initial_mock
        .expect_release_maintenance_hold()
        .returning(|_| Ok(()));
    initial_mock
        .expect_is_reboot_required()
        .returning(|_| Ok(false));
    initial_mock
        .expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(carbide_dpf::DpuPhase::Ready)));
    initial_mock
        .expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(carbide_dpf::DpuDeploymentType::Bf3));
    initial_mock
        .expect_verify_node_labels()
        .returning(|_, _| Ok(true));
    initial_mock
        .expect_get_service_versions_for_dpu()
        .returning(|_| Ok(vec![]));
    let initial_env = create_service_vpc_controller_test_env(db_pool, initial_mock, 2).await;
    create_test_tenants(&initial_env).await?;

    // Provision through DPF before marking the service Ready so host setup cannot attempt its
    // placement.
    let managed_host = create_managed_host_with_dpf(&initial_env).await;

    initial_env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "referenced-slot-shrink-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    mark_dpf_service_ready_for_update(&initial_env, service_id).await?;

    // Seed attachment membership with no service interface so the instance references the
    // registration without occupying a slot.
    let attachment = serde_json::json!({
        "service_configs": [{
            "id": Uuid::new_v4(),
            "service_id": service_id,
            "version": ConfigVersion::initial(),
            "removed": null
        }]
    });
    sqlx::query(
        "INSERT INTO instances (machine_id, os_ipxe_script, network_config, nvlink_config, extension_services_config) \
         VALUES ($1, '#!ipxe', '{\"interfaces\": [], \"service_interfaces\": []}'::jsonb, '{\"gpu_configs\": []}'::jsonb, $2)",
    )
    .bind(managed_host.id)
    .bind(sqlx::types::Json(attachment))
    .execute(&initial_env.pool)
    .await?;

    // Materialize the old graph, with only the now-obsolete slot-one chain held by a finalizer.
    let mac = db::extension_service::find_interface_macs(
        &mut *initial_env.pool.acquire().await?,
        service_id,
    )
    .await?
    .pop()
    .expect("networked registration has stable identity");
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let original = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        mac,
        ServiceVpcSlots::new(2)?,
    )?;
    let obsolete_patch_name = original.interfaces[2].name.clone();
    let obsolete_patch_uid = format!("uid-{obsolete_patch_name}");
    let obsolete_chain_name = original.chains[1].name.clone();
    let interfaces = Arc::new(Mutex::new(
        original
            .interfaces
            .iter()
            .map(service_interface_observation)
            .collect::<Vec<_>>(),
    ));
    let mut original_chains = original
        .chains
        .iter()
        .map(service_chain_observation)
        .collect::<Vec<_>>();
    // Keep the obsolete second-slot chain finalizing so its dependent interface cannot retire
    // early.
    original_chains[1].is_deleting = true;
    let chains = Arc::new(Mutex::new(original_chains));
    let patch_delete_calls = Arc::new(AtomicUsize::new(0));

    // Restart with one slot and disabled admission; no DPUService remains, so the reference enables
    // repair.
    let restart_pool = initial_env.pool.clone();
    drop(initial_env);
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(4)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(3)
        .returning({
            let interfaces = Arc::clone(&interfaces);
            move |_| Ok(interfaces.lock().unwrap().clone())
        });
    mock.expect_list_detached_dpu_service_chains()
        .times(3)
        .returning({
            let chains = Arc::clone(&chains);
            let obsolete_chain_name = obsolete_chain_name.clone();
            move |_| {
                let mut chains = chains.lock().unwrap();
                let observed = chains.clone();
                // Return the finalizing chain once, then model its removal before the next pass.
                chains.retain(|chain| chain.name.as_deref() != Some(obsolete_chain_name.as_str()));
                Ok(observed)
            }
        });
    mock.expect_get_detached_dpu_service_interface().times(0);
    mock.expect_get_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_interface()
        .times(1)
        .returning({
            let interfaces = Arc::clone(&interfaces);
            let patch_delete_calls = Arc::clone(&patch_delete_calls);
            move |name, preconditions| {
                assert_eq!(name, obsolete_patch_name);
                assert_eq!(preconditions.uid, obsolete_patch_uid);
                assert_eq!(preconditions.resource_version, "1");
                patch_delete_calls.fetch_add(1, Ordering::Relaxed);
                // The mock has no asynchronous finalizer, so accepted deletion removes the patch
                // now.
                interfaces
                    .lock()
                    .unwrap()
                    .retain(|interface| interface.name.as_deref() != Some(name));
                Ok(())
            }
        });
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    mock.expect_patch_dpu_service().times(0);
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    // Shrink to one compatible slot and leave family admission empty; the durable reference permits
    // repair.
    let mut config = get_config();
    config.dpf.enabled = true;
    config.dpu_config.service_vpc_slot_count = 1;
    config.site_global_vpc_vni = None;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families = Default::default();
    let mut overrides = TestEnvOverrides::with_config(config).with_dpf_sdk(Arc::new(mock));
    // Reuse the persisted network fixtures instead of recreating them during restart.
    overrides.create_network_segments = Some(false);
    let restarted = create_test_env_with_overrides(restart_pool, overrides).await;

    // Missing consumer state first publishes the non-Ready barrier and queues repair.
    restarted.run_extension_service_controller_iteration().await;
    let mut connection = restarted.pool.acquire().await?;
    let updating = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("referenced registration remains durable during shrink repair");
    assert_eq!(
        updating.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    drop(connection);
    let enqueuer = Enqueuer::<ExtensionServiceStateControllerIO>::new(restarted.pool.clone());
    assert!(!enqueuer.enqueue_object(&service_id).await?);

    // The queued pass observes the obsolete chain finalizer and cannot delete its patch yet.
    restarted
        .run_queued_extension_service_controller_iterations()
        .await;
    // Reload the row so this proves the finalizer branch persisted its specific Wait outcome.
    let mut connection = restarted.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("referenced registration remains durable while chain finalization is pending");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason.contains("chain finalization")
    ));
    drop(connection);
    assert_eq!(patch_delete_calls.load(Ordering::Relaxed), 0);
    assert_eq!(interfaces.lock().unwrap().len(), 3);

    // Once the chain disappears, one pass removes its patch and the next recreates the consumer.
    restarted.run_extension_service_controller_iteration().await;
    assert_eq!(patch_delete_calls.load(Ordering::Relaxed), 1);
    restarted.run_extension_service_controller_iteration().await;

    // Ready now represents the configured one-slot graph despite disabled new admission.
    let mut connection = restarted.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("repaired referenced registration remains durable");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(interfaces.lock().unwrap().len(), 2);
    assert_eq!(chains.lock().unwrap().len(), 1);
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, service_id).await?,
        vec![mac]
    );

    Ok(())
}

/// Verifies networked-to-unnetworked replacement retries when stale delete identity protects a
/// replacement.
/// The MAC remains reserved, and the same-name chain is deleted only after refreshed preconditions
/// are observed.
#[crate::sqlx_test]
async fn test_networked_to_unnetworked_replacement_retries_and_preserves_mac(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Materialize the old one-slot IPv4 graph so every replacement deletion is observable.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let old_registration = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let live_service = Arc::new(Mutex::new(Some(dpu_service_observation(
        &old_registration.service,
    ))));
    let live_interfaces = Arc::new(Mutex::new(
        old_registration
            .interfaces
            .iter()
            .map(service_interface_observation)
            .collect::<Vec<_>>(),
    ));
    let live_chains = Arc::new(Mutex::new(
        old_registration
            .chains
            .iter()
            .map(service_chain_observation)
            .collect::<Vec<_>>(),
    ));
    let deletion_order = Arc::new(Mutex::new(Vec::new()));
    let chain_delete_attempts = Arc::new(AtomicUsize::new(0));

    // Model immediate finalization except for a same-name chain replacement racing the first
    // delete.
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service().times(5).returning({
        let live_service = Arc::clone(&live_service);
        move |_| Ok(live_service.lock().unwrap().clone())
    });
    mock.expect_delete_dpu_service().times(1).returning({
        let live_service = Arc::clone(&live_service);
        let deletion_order = Arc::clone(&deletion_order);
        move |name, _| {
            *live_service.lock().unwrap() = None;
            deletion_order
                .lock()
                .unwrap()
                .push(format!("service:{name}"));
            Ok(())
        }
    });
    mock.expect_list_detached_dpu_service_chains()
        .times(4)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            move |_| Ok(live_chains.lock().unwrap().clone())
        });
    mock.expect_list_detached_dpu_service_interfaces()
        .times(4)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            move |_| Ok(live_interfaces.lock().unwrap().clone())
        });
    mock.expect_get_detached_dpu_service_interface().times(0);
    mock.expect_get_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_chain()
        .times(2)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            let deletion_order = Arc::clone(&deletion_order);
            let chain_delete_attempts = Arc::clone(&chain_delete_attempts);
            move |name, preconditions| {
                let mut chains = live_chains.lock().unwrap();
                let chain = chains
                    .iter_mut()
                    .find(|chain| chain.name.as_deref() == Some(name))
                    .expect("the observed chain remains live");
                assert_eq!(chain.uid.as_deref(), Some(preconditions.uid.as_str()));
                assert_eq!(
                    chain.resource_version.as_deref(),
                    Some(preconditions.resource_version.as_str())
                );
                if chain_delete_attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                    // A same-name generation wins the race, so the apiserver rejects the stale
                    // delete identity.
                    chain.uid = Some("replacement-chain-uid".to_owned());
                    chain.resource_version = Some("2".to_owned());
                    return Err(DpfError::InvalidState(
                        "delete preconditions no longer match the live chain".to_owned(),
                    ));
                }
                chains.retain(|chain| chain.name.as_deref() != Some(name));
                deletion_order.lock().unwrap().push(format!("chain:{name}"));
                Ok(())
            }
        });
    mock.expect_delete_detached_dpu_service_interface()
        .times(2)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            let deletion_order = Arc::clone(&deletion_order);
            move |name, _| {
                live_interfaces
                    .lock()
                    .unwrap()
                    .retain(|interface| interface.name.as_deref() != Some(name));
                deletion_order
                    .lock()
                    .unwrap()
                    .push(format!("interface:{name}"));
                Ok(())
            }
        });
    mock.expect_create_dpu_service().times(1).returning({
        let live_service = Arc::clone(&live_service);
        move |service| {
            let observation = dpu_service_observation(service);
            *live_service.lock().unwrap() = Some(observation.clone());
            Ok(observation)
        }
    });
    mock.expect_patch_dpu_service().times(0);
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist and accept IPv4, then remove networking as a complete topology replacement.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "networked-to-unnetworked".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    mark_dpf_service_ready_for_update(&env, service_id).await?;
    let original_mac =
        db::extension_service::find_interface_macs(&mut *env.pool.acquire().await?, service_id)
            .await?
            .pop()
            .expect("accepted IPv4 registration has durable identity");
    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: vec![],
            }),
            service_id: created.service_id,
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(created.version_ctr),
        }))
        .await?
        .into_inner();
    assert_eq!(lifecycle_state(&updated), "updating");

    // The first pass removes the old consumer; the next pass races with a same-name chain
    // replacement.
    env.run_extension_service_controller_iteration().await;
    env.run_extension_service_controller_iteration().await;

    // Stale preconditions preserve the replacement chain and MAC for an identity-refreshed retry.
    let mut connection = env.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("replacement remains durable after transient deletion failure");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "service-VPC chain deletion failed; retrying"
    ));
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, service_id).await?,
        vec![original_mac]
    );
    drop(connection);
    assert_eq!(chain_delete_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(live_chains.lock().unwrap().len(), 1);
    assert_eq!(
        live_chains.lock().unwrap()[0].uid.as_deref(),
        Some("replacement-chain-uid")
    );
    assert_eq!(
        live_interfaces.lock().unwrap().len(),
        2,
        "the failed chain delete preserves both dependent interfaces"
    );

    // Retry the chain, delete both interfaces in the same reconciliation phase, and create the
    // baseline service to complete replacement.
    for _ in 0..3 {
        env.run_extension_service_controller_iteration().await;
        let mut connection = env.pool.acquire().await?;
        assert_eq!(
            db::extension_service::find_interface_macs(&mut connection, service_id).await?,
            vec![original_mac]
        );
    }

    // Completion retains the stable allocation while exposing only the unnetworked service.
    let mut connection = env.pool.acquire().await?;
    let ready = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("completed replacement remains durable");
    assert_eq!(
        ready.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, service_id).await?,
        vec![original_mac]
    );
    drop(connection);
    assert_eq!(chain_delete_attempts.load(Ordering::Relaxed), 2);
    assert!(live_chains.lock().unwrap().is_empty());
    assert!(live_interfaces.lock().unwrap().is_empty());
    let replacement = live_service
        .lock()
        .unwrap()
        .clone()
        .expect("unnetworked DPUService is created");
    assert!(replacement.interfaces.is_none());
    assert_eq!(replacement.service_id.as_deref(), Some("tenant-service-v1"));
    assert_eq!(
        *deletion_order.lock().unwrap(),
        vec![
            format!("service:extsvc-{service_id}"),
            format!("chain:extsvc-{service_id}-if0-slot0-chain"),
            format!("interface:extsvc-{service_id}-if0-svc"),
            format!("interface:extsvc-{service_id}-if0-slot0-patch"),
        ]
    );

    Ok(())
}

/// Verifies an authorized topology update cannot conceal unrelated immutable placement drift.
/// The controller must fail closed before deleting any owned consumer or parent resource.
#[crate::sqlx_test]
async fn test_topology_replacement_refuses_placement_drift_without_deleting(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let mac = ExtensionServiceInterfaceMac::candidate(service_id, 0, 0);
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let old_registration = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        mac,
        ServiceVpcSlots::new(1)?,
    )?;

    // Keep the old topology recognizable except for placement, which is never authorized for
    // replacement.
    let mut drifted_service = dpu_service_observation(&old_registration.service);
    drifted_service
        .service_daemon_set
        .as_mut()
        .expect("networked projection includes a daemon set")
        .node_selector = Some(serde_json::json!({
        "nodeSelectorTerms": [{"matchExpressions": []}],
    }));
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .return_once(move |_| Ok(Some(drifted_service)));
    mock.expect_delete_dpu_service().times(0);
    mock.expect_delete_detached_dpu_service_chain().times(0);
    mock.expect_delete_detached_dpu_service_interface().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist an accepted networked registration so creation reserves the original configuration's
    // MAC identity.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "replacement-with-placement-drift".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    mark_dpf_service_ready_for_update(&env, service_id).await?;

    // Removing networking is a valid topology change, but cannot authorize deleting placement
    // drift.
    env.api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: vec![],
            }),
            service_id: created.service_id,
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(created.version_ctr),
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    // Persisted failure proves the immutable precedence branch ran before any destructive
    // operation.
    let mut connection = env.pool.acquire().await?;
    let failed = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("failed replacement remains durable");
    assert_eq!(
        failed.status.controller_state.value,
        ExtensionServiceLifecycleState::Failed
    );
    assert!(matches!(
        failed.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));

    Ok(())
}

/// Verifies an unreferenced IPv4-to-IPv6 update retires the old family graph before waiting.
/// Unsupported IPv6 must not leak new parents, while the durable MAC survives the incomplete
/// replacement.
#[crate::sqlx_test]
async fn test_ipv4_to_ipv6_replacement_finalizes_old_graph_without_creating_ipv6(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Materialize a one-slot IPv4 graph whose family-specific parents can be retired independently.
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let old_registration = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let live_service = Arc::new(Mutex::new(Some(dpu_service_observation(
        &old_registration.service,
    ))));
    let live_interfaces = Arc::new(Mutex::new(
        old_registration
            .interfaces
            .iter()
            .map(service_interface_observation)
            .collect::<Vec<_>>(),
    ));
    let live_chains = Arc::new(Mutex::new(
        old_registration
            .chains
            .iter()
            .map(service_chain_observation)
            .collect::<Vec<_>>(),
    ));

    // Each pass observes the current graph; deletes finalize immediately, and every IPv6 create is
    // forbidden.
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service().times(4).returning({
        let live_service = Arc::clone(&live_service);
        move |_| Ok(live_service.lock().unwrap().clone())
    });
    mock.expect_delete_dpu_service().times(1).returning({
        let live_service = Arc::clone(&live_service);
        move |_, _| {
            *live_service.lock().unwrap() = None;
            Ok(())
        }
    });
    mock.expect_list_detached_dpu_service_chains()
        .times(3)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            move |_| Ok(live_chains.lock().unwrap().clone())
        });
    mock.expect_list_detached_dpu_service_interfaces()
        .times(3)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            move |_| Ok(live_interfaces.lock().unwrap().clone())
        });
    mock.expect_get_detached_dpu_service_chain()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_interface()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_delete_detached_dpu_service_chain()
        .times(1)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            move |name, _| {
                live_chains
                    .lock()
                    .unwrap()
                    .retain(|chain| chain.name.as_deref() != Some(name));
                Ok(())
            }
        });
    let old_service_interface_name = old_registration.interfaces[0].name.clone();
    mock.expect_delete_detached_dpu_service_interface()
        .times(1)
        .withf(move |name, _| name == old_service_interface_name.as_str())
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            move |name, _| {
                live_interfaces
                    .lock()
                    .unwrap()
                    .retain(|interface| interface.name.as_deref() != Some(name));
                Ok(())
            }
        });
    mock.expect_create_dpu_service().times(0);
    mock.expect_patch_dpu_service().times(0);
    mock.expect_create_detached_dpu_service_interface().times(0);
    mock.expect_create_detached_dpu_service_chain().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist accepted IPv4 state, then request IPv6 while no attachment references block
    // replacement.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            // IPv4 identifies the live topology that replacement must retire.
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "ipv4-to-ipv6".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?
        .into_inner();
    mark_dpf_service_ready_for_update(&env, service_id).await?;
    let original_mac =
        db::extension_service::find_interface_macs(&mut *env.pool.acquire().await?, service_id)
            .await?
            .pop()
            .expect("accepted IPv4 registration has durable identity");
    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                // IPv6 changes immutable attachment identity but remains intentionally
                // unimplemented.
                interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                    address_family: rpc::ServiceVpcAddressFamily::Ipv6 as i32,
                }],
            }),
            service_id: created.service_id,
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(created.version_ctr),
        }))
        .await?
        .into_inner();
    assert_eq!(lifecycle_state(&updated), "updating");

    // Service, chain, and service interface retire over separate passes without releasing the MAC.
    for _ in 0..4 {
        env.run_extension_service_controller_iteration().await;
        assert_eq!(
            db::extension_service::find_interface_macs(
                &mut *env.pool.acquire().await?,
                service_id,
            )
            .await?,
            vec![original_mac]
        );
    }

    // The family-neutral patch remains reusable, while IPv6 waits before creating any replacement
    // parent.
    let mut connection = env.pool.acquire().await?;
    let waiting = db::extension_service::find_by_ids(&mut connection, &[service_id], false, false)
        .await?
        .pop()
        .expect("IPv6 replacement remains durable while implementation is deferred");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason.contains("Ipv6") && reason.contains("not implemented")
    ));
    drop(connection);
    assert!(live_service.lock().unwrap().is_none());
    assert!(live_chains.lock().unwrap().is_empty());
    assert_eq!(live_interfaces.lock().unwrap().len(), 1);
    assert_eq!(
        live_interfaces.lock().unwrap()[0].name.as_deref(),
        Some(old_registration.interfaces[1].name.as_str())
    );

    Ok(())
}

/// Verifies registration rejects a missing family, multiple interfaces, and
/// non-DPF delivery because the current allocator cannot realize those
/// contracts, preventing invalid definitions from becoming durable state.
#[crate::sqlx_test]
async fn test_service_vpc_registration_rejects_invalid_shapes(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;
    let unspecified_id = ExtensionServiceId::new();
    let too_many_id = ExtensionServiceId::new();
    let wrong_type_id = ExtensionServiceId::new();

    // The zero enum value means "not provided" and must not become IPv4.
    let unspecified = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Unspecified as i32,
            }],
            service_id: Some(unspecified_id.to_string()),
            service_name: "unspecified-family".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await
        .expect_err("unspecified family is not a service requirement");
    assert_eq!(unspecified.code(), tonic::Code::InvalidArgument);

    // Multiple requirements exceed the current API's single-interface contract.
    let too_many = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![
                rpc::ServiceVpcInterfaceRequirement {
                    address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
                },
                rpc::ServiceVpcInterfaceRequirement {
                    address_family: rpc::ServiceVpcAddressFamily::Ipv6 as i32,
                },
            ],
            service_id: Some(too_many_id.to_string()),
            service_name: "too-many-interfaces".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await
        .expect_err("MVP supports at most one service interface");
    assert_eq!(too_many.code(), tonic::Code::InvalidArgument);

    // Kubernetes Pod delivery has no DPF-owned VRF in which to place this interface.
    let wrong_type = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: None,
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(wrong_type_id.to_string()),
            service_name: "pod-network-requirement".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
            data: TEST_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await
        .expect_err("service VPC interfaces require DPF Helm delivery");
    assert_eq!(wrong_type.code(), tonic::Code::InvalidArgument);

    // Rejected requests must not leave registrations that later become visible.
    let persisted = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![
                unspecified_id.to_string(),
                too_many_id.to_string(),
                wrong_type_id.to_string(),
            ],
        }))
        .await?
        .into_inner();
    assert!(persisted.services.is_empty());

    Ok(())
}

/// Verifies complete networked updates enter reconciliation and preserve MAC identity through
/// removal.
/// Omitted definitions remain invalid so whole-definition semantics cannot silently retain stale
/// requirements.
#[crate::sqlx_test]
async fn test_networked_dpf_definition_update_requires_reconciliation(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Keep one complete requirement value across update variants so only omission/removal differs.
    let service_id = ExtensionServiceId::new();
    let requirements = vec![rpc::ServiceVpcInterfaceRequirement {
        address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
    }];
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    // Establish a Ready networked registration so this test isolates public update persistence.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: requirements.clone(),
            service_id: Some(service_id.to_string()),
            service_name: "networked-data-update".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner();
    mark_dpf_service_ready_for_update(&env, service_id).await?;
    let initial_mac =
        db::extension_service::find_interface_macs(&mut *env.pool.acquire().await?, service_id)
            .await?
            .pop()
            .expect("networked create persists ordinal-zero identity");
    assert_eq!(
        stored_dpf_service_id(&env.pool, service_id).await?,
        Some("tenant-service-v1".to_string())
    );

    // Omitting a nonempty definition is incomplete and must not silently reuse
    // the stored requirements.
    let omitted = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: None,
            service_id: created.service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(created.version_ctr),
        }))
        .await
        .expect_err("networked updates require the complete interface definition");
    assert_eq!(omitted.code(), tonic::Code::InvalidArgument);
    assert!(omitted.message().contains("must be provided"));

    // A fresh read proves the incomplete request changed no stored definition.
    let unchanged = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![created.service_id.clone()],
        }))
        .await?
        .into_inner()
        .services
        .pop()
        .expect("service remains visible after rejected update");
    assert_eq!(unchanged.version_ctr, created.version_ctr);
    assert_eq!(unchanged.service_vpc_interfaces, requirements);

    // A complete replacement is accepted and explicitly requests asynchronous reconciliation.
    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: requirements.clone(),
            }),
            service_id: created.service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(created.version_ctr),
        }))
        .await?
        .into_inner();
    assert_eq!(updated.version_ctr, created.version_ctr + 1);
    assert_eq!(lifecycle_state(&updated), "updating");
    assert_eq!(updated.service_vpc_interfaces, requirements);

    // A fresh read proves the normalized stable V1 and complete requirements committed together.
    let persisted = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![created.service_id.clone()],
        }))
        .await?
        .into_inner()
        .services
        .pop()
        .expect("updated service remains visible");
    assert_eq!(persisted.version_ctr, updated.version_ctr);
    assert_eq!(lifecycle_state(&persisted), "updating");
    assert_eq!(
        persisted
            .latest_version_info
            .expect("stable DPF version is returned")
            .data,
        model::extension_service::DpfHelmChartServiceData::parse(
            TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2,
        )?
        .normalized_json()?
    );
    assert_eq!(persisted.service_vpc_interfaces, requirements);

    // Stand in for completed DPF reconciliation before exercising subsequent Ready-only updates.
    mark_dpf_service_ready_for_update(&env, service_id).await?;

    // Metadata does not alter the deployed or network definition, so it remains
    // editable without entering controller reconciliation.
    let metadata_updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: requirements.clone(),
            }),
            service_id: created.service_id.clone(),
            service_name: Some("networked-metadata-update".to_string()),
            description: None,
            data: String::new(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(updated.version_ctr),
        }))
        .await?
        .into_inner();
    assert_eq!(metadata_updated.version_ctr, updated.version_ctr);
    assert_eq!(lifecycle_state(&metadata_updated), "ready");
    assert_eq!(metadata_updated.service_vpc_interfaces, requirements);

    // Read through the API before the next mutation to prove the metadata-only
    // path committed without changing the registered network definition.
    let persisted = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![created.service_id.clone()],
        }))
        .await?
        .into_inner()
        .services
        .pop()
        .expect("metadata-updated service remains visible");
    assert_eq!(persisted.service_name, "networked-metadata-update");
    assert_eq!(lifecycle_state(&persisted), "ready");
    assert_eq!(persisted.service_vpc_interfaces, requirements);

    // Explicitly clearing the requirements produces an unnetworked definition
    // that the existing controller can reconcile, preserving a recovery path.
    let cleared = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: vec![],
            }),
            service_id: created.service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(updated.version_ctr),
        }))
        .await?
        .into_inner();
    assert_eq!(cleared.version_ctr, updated.version_ctr + 1);
    assert_eq!(lifecycle_state(&cleared), "updating");
    assert!(cleared.service_vpc_interfaces.is_empty());

    // A fresh read proves the complete empty definition was persisted.
    let persisted = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![created.service_id],
        }))
        .await?
        .into_inner()
        .services
        .pop()
        .expect("cleared service remains visible");
    assert!(persisted.service_vpc_interfaces.is_empty());
    let retained_mac =
        db::extension_service::find_interface_macs(&mut *env.pool.acquire().await?, service_id)
            .await?
            .pop()
            .expect("removing the requirement retains its durable identity");
    assert_eq!(retained_mac, initial_mac);
    assert_eq!(
        stored_dpf_service_id(&env.pool, service_id)
            .await?
            .as_deref(),
        Some("tenant-service-v1")
    );

    // Re-adding ordinal zero after another completed reconciliation must recover the same row.
    mark_dpf_service_ready_for_update(&env, service_id).await?;
    let readded = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: requirements.clone(),
            }),
            service_id: service_id.to_string(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(cleared.version_ctr),
        }))
        .await?
        .into_inner();
    assert_eq!(lifecycle_state(&readded), "updating");
    assert_eq!(readded.service_vpc_interfaces, requirements);
    assert_eq!(
        stored_dpf_service_id(&env.pool, service_id).await?,
        Some("tenant-service-v1".to_string())
    );
    assert_eq!(
        db::extension_service::find_interface_macs(&mut *env.pool.acquire().await?, service_id,)
            .await?,
        vec![initial_mac]
    );

    Ok(())
}

/// Verifies an unnetworked Ready registration atomically commits its service-VPC requirement,
/// MAC assignment, and asynchronous Updating state.
#[crate::sqlx_test]
async fn test_dpf_interface_requirements_can_be_added_for_reconciliation(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Use API-only setup to isolate update persistence before the controller builds DPF resources.
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;
    let service_id = ExtensionServiceId::new();

    // Begin with an unnetworked Ready registration and no interface identity.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: Some(service_id.to_string()),
            service_name: "add-interface-requirement".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner();
    mark_dpf_service_ready_for_update(&env, service_id).await?;
    // The nonempty requirement is the behavior-driving change; the remaining fields preserve the
    // Helm definition.
    let requirements = vec![rpc::ServiceVpcInterfaceRequirement {
        address_family: rpc::ServiceVpcAddressFamily::Ipv6 as i32,
    }];
    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: requirements.clone(),
            }),
            service_id: created.service_id.clone(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: Some(created.version_ctr),
        }))
        .await?
        .into_inner();
    assert_eq!(updated.version_ctr, created.version_ctr + 1);
    assert_eq!(lifecycle_state(&updated), "updating");
    assert_eq!(updated.service_vpc_interfaces, requirements);

    // A fresh public read and database identity read prove both halves committed atomically.
    let persisted = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![created.service_id.clone()],
        }))
        .await?
        .into_inner()
        .services
        .pop()
        .expect("updated service remains visible");
    assert_eq!(persisted.service_vpc_interfaces, requirements);
    assert_eq!(persisted.version_ctr, updated.version_ctr);
    assert_eq!(lifecycle_state(&persisted), "updating");
    let mut connection = env.pool.acquire().await?;
    let assignments =
        db::extension_service::find_interface_macs(&mut connection, service_id).await?;
    assert_eq!(assignments.len(), 1);
    assert_eq!(assignments[0].interface_ordinal, 0);

    Ok(())
}

/// Verifies active, terminating, and soft-deleted-instance attachments prevent
/// removing a DPF service contract through requirement updates or public
/// deletion, because each can still own resources that depend on it.
#[crate::sqlx_test]
async fn test_dpf_contract_removal_rejects_existing_attachments(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Start from a Ready networked registration, where removing the requirement
    // would otherwise be the supported path back to an unnetworked definition.
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;
    let service_id = ExtensionServiceId::new();
    let requirements = vec![rpc::ServiceVpcInterfaceRequirement {
        address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
    }];
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: requirements.clone(),
            service_id: Some(service_id.to_string()),
            service_name: "remove-attached-interface-requirement".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner();
    let registered_version: ConfigVersion = created
        .latest_version_info
        .as_ref()
        .expect("created DPF service has an initial version")
        .version
        .parse()?;
    mark_dpf_service_ready_for_update(&env, service_id).await?;

    // Public Service VPC activation is planned in issue #6125, so seed only
    // the durable attachment state consumed by the update exclusion check.
    let managed_host = create_managed_host(&env).await;
    let instance_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO instances (machine_id, os_ipxe_script, network_config, nvlink_config) \
         VALUES ($1, '#!ipxe', '{\"interfaces\": []}'::jsonb, '{\"gpu_configs\": []}'::jsonb) \
         RETURNING id",
    )
    .bind(managed_host.id)
    .fetch_one(&env.pool)
    .await?;
    let cases = [
        // An active attachment is already using the current service definition.
        ("active attachment", serde_json::Value::Null, None),
        // Service-interface records may still refer to a terminating attachment
        // until cleanup completes.
        (
            "terminating attachment",
            serde_json::json!(chrono::Utc::now()),
            None,
        ),
        // Soft deletion does not release an instance's attachments, so the
        // include-deleted lookup must continue to protect their contract.
        (
            "attachment on a soft-deleted instance",
            serde_json::Value::Null,
            Some(chrono::Utc::now()),
        ),
    ];
    for (scenario, removed, instance_deleted) in cases {
        // Persist the attachment-removal and instance-deletion state that
        // distinguishes this case. The other fields form a valid reference to
        // the service version whose contract must remain protected.
        let attachment = serde_json::json!({
            "service_configs": [{
                "id": uuid::Uuid::new_v4(),
                "service_id": service_id,
                "version": registered_version,
                "removed": removed
            }]
        });
        sqlx::query(
            "UPDATE instances SET extension_services_config = $1, deleted = $2 WHERE id = $3",
        )
        .bind(sqlx::types::Json(attachment))
        .bind(instance_deleted)
        .bind(instance_id)
        .execute(&env.pool)
        .await?;

        // Removing the requirement must be rejected before it changes either
        // the stable DPF data or the registration record.
        let error = env
            .api
            .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
                service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                    interfaces: vec![],
                }),
                service_id: created.service_id.clone(),
                service_name: None,
                description: None,
                data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
                credential: None,
                observability: None,
                if_version_ctr_match: Some(created.version_ctr),
            }))
            .await
            .expect_err(scenario);
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
        assert!(
            error
                .message()
                .contains("active or terminating attachments"),
            "{scenario}: {error}"
        );

        // A fresh API read proves the rejected replacement left the definition intact.
        let persisted = env
            .api
            .find_dpu_extension_services_by_ids(Request::new(
                rpc::DpuExtensionServicesByIdsRequest {
                    service_ids: vec![created.service_id.clone()],
                },
            ))
            .await?
            .into_inner()
            .services
            .pop()
            .expect("service remains visible after rejected update");
        assert_eq!(persisted.service_vpc_interfaces, requirements, "{scenario}");
        assert_eq!(persisted.version_ctr, created.version_ctr, "{scenario}");
        assert_eq!(lifecycle_state(&persisted), "ready", "{scenario}");
    }

    // Exercise the public DPF deletion handler with the strongest retained
    // form: a terminating attachment on a soft-deleted instance.
    let attachment = serde_json::json!({
        "service_configs": [{
            "id": uuid::Uuid::new_v4(),
            "service_id": service_id,
            "version": registered_version,
            "removed": chrono::Utc::now()
        }]
    });
    sqlx::query(
        "UPDATE instances SET extension_services_config = $1, deleted = NOW() WHERE id = $2",
    )
    .bind(sqlx::types::Json(attachment))
    .bind(instance_id)
    .execute(&env.pool)
    .await?;
    let error = env
        .api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: created.service_id.clone(),
            versions: vec![],
        }))
        .await
        .expect_err("terminating attachment on a deleted instance must block DPF deletion");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "extension service is in use by instances; detach before deleting"
    );

    // A fresh public read proves the rejected delete preserved the registration.
    let persisted = env
        .api
        .find_dpu_extension_services_by_ids(Request::new(rpc::DpuExtensionServicesByIdsRequest {
            service_ids: vec![created.service_id.clone()],
        }))
        .await?
        .into_inner()
        .services
        .pop()
        .expect("service remains visible after rejected deletion");
    assert_eq!(persisted.service_vpc_interfaces, requirements);
    assert_eq!(persisted.version_ctr, created.version_ctr);
    assert_eq!(lifecycle_state(&persisted), "ready");

    Ok(())
}

/// Verifies mutable DPF fields replace stable V1 while retaining identity and the observed
/// revision.
/// The revision guard prevents stale reconciliation from overwriting a newer Kubernetes object.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_update_replaces_v1_and_requests_reconciliation(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let initial_data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let initial_projection = project_dpu_service(
        service_id,
        carbide_dpf::NAMESPACE,
        &initial_data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
    let updated_data_json = TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2.to_string();
    let updated_data =
        model::extension_service::DpfHelmChartServiceData::parse(&updated_data_json)?;
    let updated_projection = project_dpu_service(
        service_id,
        carbide_dpf::NAMESPACE,
        &updated_data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
    let existing = dpu_service_observation(&initial_projection);
    let expected_name = updated_projection.name.clone();
    // Preserve the observed resource version so the expected patch proves stale-update fencing.
    let expected_patch = dpu_service_mutable_patch(&updated_projection, &existing, "1");
    let patched = dpu_service_observation(&updated_projection);

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
        .returning(move |_, _| Ok(patched.clone()));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: Some(service_id.to_string()),
            service_name: "update-dpf-service".to_string(),
            description: Some("before update".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner();
    assert_eq!(created.service_id, service_id.to_string());

    // Create and reconcile the original V1 to Ready before the API accepts replacement data.
    env.run_extension_service_controller_iteration().await;

    // Reject a stale revision before exercising the accepted V1 replacement.
    let stale_update = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: None,
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

    // Replace V1 through the public API and request asynchronous reconciliation.
    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: None,
            service_id: service_id.to_string(),
            service_name: Some("updated-dpf-service".to_string()),
            description: Some("after update".to_string()),
            data: updated_data_json.clone(),
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
        model::extension_service::DpfHelmChartServiceData::parse(&updated_data_json)?
            .normalized_json()?
    );

    // Reload database state to prove the updated V1 and Updating transition committed together.
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

    // V1 replacement keeps the immutable ID in its separate column.
    let stored_dpf_id: Option<String> = sqlx::query_scalar(
        "SELECT dpf_service_id FROM extension_service_versions WHERE service_id = $1 AND deleted IS NULL",
    )
    .bind(service_id)
    .fetch_one(&env.pool)
    .await?;
    assert_eq!(stored_dpf_id.as_deref(), Some("tenant-service-v1"));

    // A second data update is rejected before it can change V1 because DPF
    // reconciliation of the first replacement has not completed yet.
    let error = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: None,
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

    // The controller reads the old owned object and records Ready only after the PATCH response
    // proves that the mutable DPUService material converged in place.
    env.run_extension_service_controller_iteration().await;

    // Reload state and history to prove patch acceptance completed the update without a new version.
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

/// Verifies global deletion retains the MAC through failed cleanup, then releases it without
/// touching another owner.
/// The dependency order and owner selector prevent identity reuse or cross-registration parent
/// deletion.
#[crate::sqlx_test]
async fn test_networked_dpf_delete_releases_mac_only_after_owned_parents_disappear(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // A one-slot graph exercises the consumer, chain, service interface, and patch interface
    // together.
    let service_id = ExtensionServiceId::new();
    let unrelated_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projected = project_service_vpc_registration(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(service_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    // A second owner's complete graph shares the namespace and must remain byte-for-byte unchanged.
    let unrelated_projection = project_service_vpc_registration(
        unrelated_id,
        carbide_dpf::NAMESPACE,
        &data,
        ServiceVpcAddressFamily::Ipv4,
        ExtensionServiceInterfaceMac::candidate(unrelated_id, 0, 0),
        ServiceVpcSlots::new(1)?,
    )?;
    let existing_service = dpu_service_observation(&projected.service);
    // Mark the second observation as finalizing so accepted deletion cannot complete the lifecycle
    // early.
    let mut finalizing_service = existing_service.clone();
    finalizing_service.is_deleting = true;
    let unrelated_interfaces = unrelated_projection
        .interfaces
        .iter()
        .map(service_interface_observation)
        .collect::<Vec<_>>();
    let unrelated_chains = unrelated_projection
        .chains
        .iter()
        .map(service_chain_observation)
        .collect::<Vec<_>>();
    let live_interfaces = Arc::new(Mutex::new(
        projected
            .interfaces
            .iter()
            .map(service_interface_observation)
            .chain(unrelated_interfaces.iter().cloned())
            .collect::<Vec<_>>(),
    ));
    let live_chains = Arc::new(Mutex::new(
        projected
            .chains
            .iter()
            .map(service_chain_observation)
            .chain(unrelated_chains.iter().cloned())
            .collect::<Vec<_>>(),
    ));
    let get_calls = Arc::new(AtomicUsize::new(0));
    let chain_delete_attempts = Arc::new(AtomicUsize::new(0));
    let expected_name = projected.service.name.clone();

    // Model owner-scoped reads, service finalization, one transient chain failure, and immediate
    // parent removal.
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service().times(6).returning({
        let get_calls = Arc::clone(&get_calls);
        // Return live, then finalizing, then absent so cleanup cannot start before finalization.
        move |_| match get_calls.fetch_add(1, Ordering::Relaxed) {
            0 => Ok(Some(existing_service.clone())),
            1 => Ok(Some(finalizing_service.clone())),
            _ => Ok(None),
        }
    });
    mock.expect_delete_dpu_service()
        .times(1)
        .withf(move |name, _| name == expected_name)
        .returning(|_, _| Ok(()));
    mock.expect_list_detached_dpu_service_chains()
        .times(4)
        .withf({
            let expected = format!("{DPF_HELM_CHART_OWNER_LABEL}={service_id}");
            move |selector| *selector == Some(expected.as_str())
        })
        .returning({
            let live_chains = Arc::clone(&live_chains);
            let owner = service_id.to_string();
            move |_| {
                Ok(live_chains
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|chain| chain.labels.get(DPF_HELM_CHART_OWNER_LABEL) == Some(&owner))
                    .cloned()
                    .collect())
            }
        });
    mock.expect_list_detached_dpu_service_interfaces()
        .times(2)
        .withf({
            let expected = format!("{DPF_HELM_CHART_OWNER_LABEL}={service_id}");
            move |selector| *selector == Some(expected.as_str())
        })
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            let owner = service_id.to_string();
            move |_| {
                Ok(live_interfaces
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|interface| {
                        interface.labels.get(DPF_HELM_CHART_OWNER_LABEL) == Some(&owner)
                    })
                    .cloned()
                    .collect())
            }
        });
    mock.expect_delete_detached_dpu_service_chain()
        .times(2)
        .returning({
            let live_chains = Arc::clone(&live_chains);
            let chain_delete_attempts = Arc::clone(&chain_delete_attempts);
            move |name, _| {
                // Fail the first delete without mutating inventory so the retry sees the same
                // chain.
                if chain_delete_attempts.fetch_add(1, Ordering::Relaxed) == 0 {
                    return Err(DpfError::timeout(
                        "delete DPUServiceChain",
                        "injected transient failure",
                    ));
                }
                // The successful retry removes only the requested chain, preserving the other
                // owner's graph.
                live_chains
                    .lock()
                    .unwrap()
                    .retain(|chain| chain.name.as_deref() != Some(name));
                Ok(())
            }
        });
    mock.expect_delete_detached_dpu_service_interface()
        .times(2)
        .returning({
            let live_interfaces = Arc::clone(&live_interfaces);
            move |name, _| {
                // Accepted deletion removes only the requested interface, preserving the other
                // owner's graph.
                live_interfaces
                    .lock()
                    .unwrap()
                    .retain(|interface| interface.name.as_deref() != Some(name));
                Ok(())
            }
        });
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    // Give another service a distinct assignment so global cleanup must prove owner scoping.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: None,
            service_vpc_interfaces: vec![],
            service_id: Some(unrelated_id.to_string()),
            service_name: "unrelated-mac-owner".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
            data: TEST_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    let unrelated_assignment = ExtensionServiceInterfaceMac::candidate(unrelated_id, 0, 0);
    let mut txn = env.pool.begin().await?;
    db::extension_service::insert_interface_mac(&mut txn, unrelated_assignment).await?;
    txn.commit().await?;

    // Persist the matching networked declaration and stand in for completed create reconciliation.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "networked-delete-finalizers".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?;
    mark_dpf_service_ready_for_update(&env, service_id).await?;
    let released_assignment =
        db::extension_service::find_interface_macs(&mut *env.pool.acquire().await?, service_id)
            .await?
            .pop()
            .expect("networked registration has a MAC to release");
    // Request deletion only after Ready so subsequent controller passes own external teardown and
    // MAC release.
    env.api
        .delete_dpu_extension_service(Request::new(rpc::DeleteDpuExtensionServiceRequest {
            service_id: service_id.to_string(),
            versions: vec![],
        }))
        .await?;

    // Delete acceptance and service finalization retain the allocation before parent cleanup
    // starts.
    for _ in 0..2 {
        env.run_extension_service_controller_iteration().await;
        let mut connection = env.pool.acquire().await?;
        assert_eq!(
            db::extension_service::find_interface_macs(&mut connection, service_id)
                .await?
                .len(),
            1
        );
    }

    // A failed chain delete leaves the complete parent graph and MAC intact for an idempotent
    // retry.
    env.run_extension_service_controller_iteration().await;
    let mut connection = env.pool.acquire().await?;
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, service_id).await?,
        vec![released_assignment]
    );
    drop(connection);
    assert_eq!(chain_delete_attempts.load(Ordering::Relaxed), 1);
    assert_eq!(live_chains.lock().unwrap().len(), 2);
    assert_eq!(live_interfaces.lock().unwrap().len(), 4);

    // Successful chain and batched interface deletes still retain the MAC until absence is
    // re-observed.
    for _ in 0..2 {
        env.run_extension_service_controller_iteration().await;
        let mut connection = env.pool.acquire().await?;
        assert_eq!(
            db::extension_service::find_interface_macs(&mut connection, service_id).await?,
            vec![released_assignment]
        );
    }
    assert_eq!(chain_delete_attempts.load(Ordering::Relaxed), 2);
    assert_eq!(*live_chains.lock().unwrap(), unrelated_chains);
    assert_eq!(*live_interfaces.lock().unwrap(), unrelated_interfaces);

    // Only a final empty graph observation releases the service allocation.
    env.run_extension_service_controller_iteration().await;
    let mut connection = env.pool.acquire().await?;
    assert!(
        db::extension_service::find_interface_macs(&mut connection, service_id)
            .await?
            .is_empty()
    );
    assert_eq!(
        db::extension_service::find_interface_macs(&mut connection, unrelated_id).await?,
        vec![unrelated_assignment]
    );
    let deleted = db::extension_service::find_by_ids(&mut connection, &[service_id], true, false)
        .await?
        .pop()
        .expect("terminal registration remains stored");
    assert_eq!(
        deleted.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleted
    );
    drop(connection);

    // A different durable service can claim the released value only after terminal cleanup.
    let reuse_owner = ExtensionServiceId::new();
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: None,
            service_vpc_interfaces: vec![],
            service_id: Some(reuse_owner.to_string()),
            service_name: "released-mac-reuse-owner".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
            data: TEST_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;
    let reused_assignment = ExtensionServiceInterfaceMac {
        service_id: reuse_owner,
        ..released_assignment
    };
    let mut txn = env.pool.begin().await?;
    db::extension_service::insert_interface_mac(&mut txn, reused_assignment).await?;
    txn.commit().await?;
    assert_eq!(
        db::extension_service::find_interface_macs(&mut *env.pool.acquire().await?, reuse_owner,)
            .await?,
        vec![reused_assignment]
    );

    Ok(())
}

/// Verifies stale consumer deletion is retried with refreshed identity and then polled through
/// finalization.
/// A same-name replacement must survive the stale attempt, and delete acceptance cannot imply
/// completion.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_delete_waits_for_dpf_finalization(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let projected = project_dpu_service(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
    let expected_name = projected.name.clone();
    let existing = dpu_service_observation(&projected);
    let mut replacement = existing.clone();
    replacement.uid = Some("replacement-service-uid".to_string());
    replacement.resource_version = Some("2".to_string());
    let mut finalizing = replacement.clone();
    finalizing.is_deleting = true;
    let get_calls = Arc::new(AtomicUsize::new(0));
    let delete_calls = Arc::new(AtomicUsize::new(0));

    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    mock.expect_get_dpu_service().times(4).returning({
        let get_calls = Arc::clone(&get_calls);
        move |_| match get_calls.fetch_add(1, Ordering::Relaxed) {
            0 => Ok(Some(existing.clone())),
            1 => Ok(Some(replacement.clone())),
            2 => Ok(Some(finalizing.clone())),
            _ => Ok(None),
        }
    });
    mock.expect_delete_dpu_service()
        .times(2)
        .withf(move |name, _| name == expected_name.as_str())
        .returning({
            let delete_calls = Arc::clone(&delete_calls);
            move |_, preconditions| {
                if delete_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                    assert_eq!(preconditions.uid, format!("uid-{}", projected.name));
                    assert_eq!(preconditions.resource_version, "1");
                    return Err(DpfError::InvalidState(
                        "simulated stale DPUService revision".to_string(),
                    ));
                }
                assert_eq!(preconditions.uid, "replacement-service-uid");
                assert_eq!(preconditions.resource_version, "2");
                Ok(())
            }
        });
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    // Create and reconcile a Ready DPUService so deletion begins from an observed external
    // consumer.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: Some(service_id.to_string()),
            service_name: "delete-dpf-service".to_string(),
            description: Some("delete controller test".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    // Commit deletion intent first; the soft-deleted registration remains controller-visible
    // while active versions disappear and DPF finalization is still pending.
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
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await;
    let error = recreate_while_deleting
        .expect_err("a pending DPF Helm chart deletion must reserve its service name");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    // The first stale delete observes a same-name replacement and remains retryable.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let waiting = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("deleting registration remains durable after stale consumer deletion");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleting
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPUService deletion failed; retrying"
    ));
    txn.commit().await?;
    assert_eq!(delete_calls.load(Ordering::Relaxed), 1);

    // A refreshed delete is accepted but must remain durably Deleting until absence is observed.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let waiting = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("deleting registration remains durable after delete acceptance");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleting
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPF accepted DPUService deletion; awaiting finalization"
    ));
    txn.commit().await?;
    assert_eq!(delete_calls.load(Ordering::Relaxed), 2);

    // A deletion timestamp preserves the same durable state while DPF finalizers run.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let waiting = db::extension_service::find_by_ids(&mut txn, &[service_id], true, false)
        .await?
        .pop()
        .expect("deleting registration remains durable during finalization");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Deleting
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPF DPUService finalization is in progress; retrying"
    ));
    txn.commit().await?;

    // Confirmed absence lets the controller commit Deleted; reload the row before proving the
    // reserved service name can be reused.
    env.run_extension_service_controller_iteration().await;

    // Reload the terminal state and history to prove observed CR absence completed deletion.
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

    // Only completed DPF deletion releases the name for a new registration.
    let recreated = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
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
    let projected = project_dpu_service(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
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
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: Some(service_id.to_string()),
            service_name: "unowned-delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
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
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: Some(service_id.to_string()),
            service_name: "restart-delete-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
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
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "metadata-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner();
    let service_id: ExtensionServiceId = created.service_id.parse()?;
    env.run_extension_service_controller_iteration().await;

    let updated = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: None,
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
                dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
                service_vpc_interfaces: vec![],
                service_id: Some(service_id.to_string()),
                service_name: name.to_string(),
                description: None,
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
                data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
                credential,
                observability,
                deployment_type: 0,
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

/// Verifies invalid or unsupported DPF fields fail before durable state is created.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_rejects_invalid_data(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    for (name, data, expected_message) in [
        // Required chart identity must be complete before reconciliation.
        (
            "dpf-missing-chart-version",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "security":{"privileged":false}
            }"#,
            "missing field `chartVersion`",
        ),
        // Unknown top-level fields must not silently disappear during normalization.
        (
            "dpf-unknown-field",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "security":{"privileged":false},
                "unsupported":true
            }"#,
            "unknown field `unsupported`",
        ),
        // Helm values cannot bypass NICo's ownership of workload placement.
        (
            "dpf-reserved-node-selector",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "deployInCluster":false,
                "security":{"privileged":false},
                "values":{"serviceDaemonSet":{"nodeSelector":{"tenant":"value"}}}
            }"#,
            "tenant values may not set NICo-owned field serviceDaemonSet.nodeSelector",
        ),
        // The typed DaemonSet contract excludes direct placement for the same reason.
        (
            "dpf-explicit-node-selector",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "security":{"privileged":false},
                "serviceDaemonSet":{"nodeSelector":{}}
            }"#,
            "unknown field `nodeSelector`",
        ),
        // DPF names this field updateStrategy, so the SDD spelling must not be accepted.
        (
            "dpf-misspelled-upgrade-strategy",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "security":{"privileged":false},
                "serviceDaemonSet":{"upgradeStrategy":{"type":"RollingUpdate"}}
            }"#,
            "unknown field `upgradeStrategy`",
        ),
        // The workload identity is mandatory even when the rest of the chart is valid.
        (
            "dpf-missing-service-id",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "deployInCluster":false,
                "security":{"privileged":false}
            }"#,
            "serviceID must not be empty",
        ),
        // An explicitly supplied service identity must contain a usable value.
        (
            "dpf-empty-service-id",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"",
                "security":{"privileged":false}
            }"#,
            "serviceID must not be empty",
        ),
        // An escaped NUL decodes to a character PostgreSQL cannot store as an identity.
        (
            "dpf-nul-service-id",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant\u0000service",
                "deployInCluster":false,
                "security":{"privileged":false}
            }"#,
            "serviceID must not contain NUL characters",
        ),
        // NICo requires the DPF deployment location to be explicit before persistence.
        (
            "dpf-missing-deployment-location",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "security":{"privileged":false}
            }"#,
            "deployInCluster must be explicitly set to false",
        ),
        // NICo manages DPU workloads and does not permit host-cluster deployment.
        (
            "dpf-host-cluster-deployment",
            r#"{
                "repoURL":"oci://registry.example.com/charts",
                "chartName":"tenant-service",
                "chartVersion":"1.2.3",
                "serviceID":"tenant-service-v1",
                "deployInCluster":true,
                "security":{"privileged":false}
            }"#,
            "deployInCluster must be explicitly set to false",
        ),
    ] {
        let service_id = ExtensionServiceId::new();
        let error = env
            .api
            .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
                dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
                service_vpc_interfaces: vec![],
                service_id: Some(service_id.to_string()),
                service_name: name.to_string(),
                description: None,
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
                data: data.to_string(),
                credential: None,
                observability: None,
                deployment_type: 0,
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

    // Register the original name with one explicit DPF identity.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "duplicate-dpf-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?;

    // Reuse the case-insensitive name with a different DPF identity so this
    // assertion exercises name uniqueness rather than service-ID uniqueness.
    let error = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "Duplicate-Dpf-Service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA
                .replace("tenant-service-v1", "tenant-service-v2"),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await
        .expect_err("DPF Helm service names must be unique within a tenant");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    Ok(())
}

/// Verifies networking does not change the chart-provided DPF identity.
/// Networked and unnetworked registrations share its case-insensitive uniqueness domain.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_rejects_duplicate_service_id_across_network_shapes(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;

    // Prepare different NICo names and tenants that intentionally claim
    // case variants of the same explicit DPF serviceID.
    let first_request = rpc::CreateDpuExtensionServiceRequest {
        service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
            address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
        }],
        dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
        service_id: None,
        service_name: "first-dpf-service".to_string(),
        description: None,
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
        data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
    };
    let second_request = rpc::CreateDpuExtensionServiceRequest {
        service_vpc_interfaces: vec![],
        service_name: "second-dpf-service".to_string(),
        tenant_organization_id: "another_org".to_string(),
        data: TEST_DPF_HELM_CHART_SERVICE_DATA.replace("tenant-service-v1", "TENANT-SERVICE-V1"),
        ..first_request.clone()
    };

    // Persist the first service so it owns the shared DPF service ID.
    env.api
        .create_dpu_extension_service(Request::new(first_request))
        .await
        .expect("the first service must claim the DPF service ID");

    // A second tenant must not be able to claim the same DPF service ID.
    let rejected = env
        .api
        .create_dpu_extension_service(Request::new(second_request))
        .await
        .expect_err("the duplicate DPF service ID must be rejected");

    // Match the established database-conflict contract without exposing the
    // stored chart data.
    assert_eq!(rejected.code(), tonic::Code::FailedPrecondition);
    assert!(rejected.message().contains("dpf_service_id"));
    assert!(rejected.message().contains("TENANT-SERVICE-V1"));

    Ok(())
}

/// Verifies updates cannot change the DPF workload identity and rejected updates
/// leave the stable V1 definition and lifecycle state unchanged.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_update_rejects_changed_service_id(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| Ok(dpu_service_observation(service)));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    // Create and reconcile the original service so data updates are eligible.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_vpc_interfaces: vec![],
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            service_id: Some(service_id.to_string()),
            service_name: "immutable-service-id".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?;
    env.run_extension_service_controller_iteration().await;

    // Change mutable chart data together with the service ID; the identity
    // change must reject the entire request.
    let changed_identity = TEST_DPF_HELM_CHART_SERVICE_DATA_VERSION_2
        .replace("tenant-service-v1", "tenant-service-v2");
    let error = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: None,
            service_id: service_id.to_string(),
            service_name: None,
            description: None,
            data: changed_identity,
            credential: None,
            observability: None,
            if_version_ctr_match: Some(1),
        }))
        .await
        .expect_err("a DPF service ID change must be rejected");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert!(error.message().contains("serviceID cannot be changed"));

    // Re-read durable state to prove the rejected update had no side effects.
    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("original service remains active");
    let version =
        db::extension_service::find_version_info_of_known_service(&mut txn, service_id, None)
            .await?;
    assert_eq!(record.version_ctr, 1);
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert_eq!(
        version.data,
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?
            .normalized_json()?
    );
    txn.commit().await?;

    Ok(())
}

/// Seeds a DPF Helm service through persistence so controller tests can choose its UUID without
/// exercising API admission.
async fn seed_dpf_helm_chart_service(
    env: &TestEnv,
    name: &str,
) -> Result<ExtensionServiceId, eyre::Report> {
    let service_id = ExtensionServiceId::new();
    seed_dpf_helm_chart_service_with_id(env, service_id, name).await?;
    Ok(service_id)
}

/// Seeds a DPF Helm service with a caller-selected UUID so deterministic DPF resource names match
/// controller fixtures.
async fn seed_dpf_helm_chart_service_with_id(
    env: &TestEnv,
    service_id: ExtensionServiceId,
    name: &str,
) -> Result<(), eyre::Report> {
    // Commit the direct database fixture before the extension-service controller can observe the
    // row.
    let tenant: TenantOrganizationId = "best_org".parse()?;
    let mut txn = env.pool.begin().await?;
    db::extension_service::create(
        &mut txn,
        ConfigVersion::initial(),
        &service_id,
        &ExtensionServiceType::DpfHelmChart,
        model::extension_service::ExtensionServiceDeploymentType::DemandDriven,
        Some(model::extension_service::DpuTarget::All),
        name,
        &tenant,
        Some("controller fixture"),
        &[],
        TEST_DPF_HELM_CHART_SERVICE_DATA,
        Some("tenant-service-v1"),
        None,
        false,
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

/// Verifies predecessor-format nonnetworked definitions retain their established Ready quiet path.
/// Predecessor rows may omit required V1 fields and must not fail when no service-VPC inventory
/// exists.
#[crate::sqlx_test]
async fn test_ready_nonnetworked_legacy_dpf_definition_skips_registration_parsing(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_dpf_controller_test_env(db_pool, MockDpfOperations::new()).await;
    create_test_tenants(&env).await?;
    let service_id = ExtensionServiceId::new();
    // Omit serviceID and deployInCluster, then persist the predecessor row directly and mark it
    // Ready so reconciliation encounters the representation accepted before those fields became
    // required.
    let legacy_data = r#"{
        "repoURL":"oci://registry.example.com/charts",
        "chartName":"legacy-service",
        "chartVersion":"1.0.0",
        "security":{"privileged":false}
    }"#;

    let tenant: TenantOrganizationId = "best_org".parse()?;
    let mut txn = env.pool.begin().await?;
    db::extension_service::create(
        &mut txn,
        ConfigVersion::initial(),
        &service_id,
        &ExtensionServiceType::DpfHelmChart,
        ExtensionServiceDeploymentType::DemandDriven,
        Some(model::extension_service::DpuTarget::All),
        "legacy-nonnetworked-service",
        &tenant,
        None,
        &[],
        legacy_data,
        None,
        None,
        false,
    )
    .await?;
    txn.commit().await?;
    mark_dpf_service_ready_for_update(&env, service_id).await?;

    // Periodic Ready reconciliation must neither parse the legacy JSON nor contact DPF.
    env.run_extension_service_controller_iteration().await;

    // Reload durable state to prove the compatibility path did not demote the service to Failed.
    let mut txn = env.pool.begin().await?;
    let persisted = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("legacy service remains visible");
    assert_eq!(
        persisted.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    assert!(matches!(
        persisted.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::DoNothing { .. })
    ));
    assert!(persisted.service_vpc_interfaces.is_empty());
    assert_eq!(stored_dpf_service_id(&env.pool, service_id).await?, None);
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

/// Verifies Creating publishes an Updating barrier when the created DPUService has mutable drift.
/// This prevents Ready from promising convergence before the queued pass applies the fenced patch.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_reconciliation_waits_for_mutable_convergence(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Build an absent one-slot graph, then return an ownership-valid consumer with mutable drift.
    let service_id = ExtensionServiceId::new();
    let mut mock = MockDpfOperations::new();
    mock.expect_get_dpu_service()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_list_detached_dpu_service_interfaces()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_list_detached_dpu_service_chains()
        .times(1)
        .returning(|_| Ok(Vec::new()));
    mock.expect_get_detached_dpu_service_interface()
        .times(2)
        .returning(|_| Ok(None));
    mock.expect_get_detached_dpu_service_chain()
        .times(1)
        .returning(|_| Ok(None));
    mock.expect_create_detached_dpu_service_interface()
        .times(2)
        .returning(|interface| Ok(service_interface_observation(interface)));
    mock.expect_create_detached_dpu_service_chain()
        .times(1)
        .returning(|chain| Ok(service_chain_observation(chain)));
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| {
            let mut created = dpu_service_observation(service);
            // Flip only a mutable security field so creation succeeds but convergence still
            // requires Updating.
            created
                .security
                .as_mut()
                .expect("projected service includes security")
                .privileged = Some(!service.security.privileged);
            Ok(created)
        });
    mock.expect_patch_dpu_service().times(0);
    let env = create_service_vpc_controller_test_env(db_pool, mock, 1).await;
    create_test_tenants(&env).await?;

    // Persist one IPv4 requirement so reconciliation takes the complete networked create path.
    env.api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            // Store All as this fixture's placement policy without resolving target DPUs until
            // activation.
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
            // Use the IPv4 requirement to create service-VPC parents and the consuming DPUService.
            service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: Some(service_id.to_string()),
            service_name: "controller-networked-create-drift".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            // DPF Helm chart is the only service type that accepts a network requirement.
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
        }))
        .await?;

    // The Creating pass records the barrier without applying Updating-owned side effects.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let record = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record remains available during mutable convergence");
    assert_eq!(
        record.status.controller_state.value,
        ExtensionServiceLifecycleState::Updating
    );
    assert!(matches!(
        record.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Transition { .. })
    ));
    txn.commit().await?;

    Ok(())
}

/// Verifies baseline deployment type is saved in the database and the service becomes Ready
/// after DPF accepts its DPUService. The missing selector allows all DPUs;
/// the owner label identifies the service for later updates and deletion.
/// Valid Helm data ensures service VPC updates fail on the baseline restriction,
/// so an unrelated parsing error cannot mask a missing guard.
#[crate::sqlx_test]
async fn test_baseline_dpf_helm_chart_create_reconciles_without_placement_selector(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Expect one selector-free publication while retaining NICo ownership.
    let service_id = ExtensionServiceId::new();
    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service()
        .times(1)
        .returning(|service| {
            assert!(
                service
                    .service_daemon_set
                    .as_ref()
                    .expect("baseline retains daemon-set settings")
                    .node_selector_labels
                    .is_none()
            );
            assert!(
                service
                    .labels
                    .contains_key(model::extension_service::DPF_HELM_CHART_OWNER_LABEL)
            );
            Ok(dpu_service_observation(service))
        });
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;

    // Persist intent through the API before the controller can publish to DPF.
    let created = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: Some(service_id.to_string()),
            service_name: "baseline-controller-create".to_string(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart as i32,
            deployment_type: rpc::DpuExtensionServiceDeploymentType::Baseline as i32,
            dpu_target: None,
            tenant_organization_id: "best_org".to_string(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            ..Default::default()
        }))
        .await?
        .into_inner();
    assert_eq!(
        created.deployment_type,
        rpc::DpuExtensionServiceDeploymentType::Baseline as i32
    );
    assert!(created.dpu_target.is_none());

    // Publish the DPUService only after registration commits.
    env.run_extension_service_controller_iteration().await;

    // Reload durable state to prove DPF acceptance completed reconciliation.
    let mut txn = env.pool.begin().await?;
    let stored = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("baseline registration remains durable");
    assert_eq!(
        stored.deployment_type,
        ExtensionServiceDeploymentType::Baseline
    );
    assert_eq!(
        stored.status.controller_state.value,
        ExtensionServiceLifecycleState::Ready
    );
    txn.commit().await?;

    // Use valid Helm data to isolate the baseline restriction on service VPC updates.
    let rejected = env
        .api
        .update_dpu_extension_service(Request::new(rpc::UpdateDpuExtensionServiceRequest {
            service_vpc_interfaces: Some(rpc::ServiceVpcInterfaceRequirements {
                interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                    address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
                }],
            }),
            service_id: service_id.to_string(),
            service_name: None,
            description: None,
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            if_version_ctr_match: None,
        }))
        .await
        .expect_err("baseline updates must reject service VPC requirements");
    // Require the baseline-specific error so unrelated validation cannot satisfy the test.
    assert_eq!(rejected.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        rejected.message(),
        "service VPC interfaces are unsupported for baseline helm services"
    );

    Ok(())
}

/// Verifies create rejects unsupported deployment types, targets, and service
/// VPC interfaces before saving a service that the controller would deploy.
/// Specific errors distinguish each restriction from unrelated validation failures.
#[crate::sqlx_test]
async fn test_extension_service_create_rejects_invalid_deployment_type_combinations(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    // Enable DPF and create the tenant so unrelated prerequisites are satisfied.
    let env = create_dpf_enabled_test_env(db_pool).await;
    create_test_tenants(&env).await?;
    let baseline = rpc::DpuExtensionServiceDeploymentType::Baseline as i32;

    let cases = [
        // ALL is scoped to an instance host, so even this target conflicts with baseline.
        (
            "baseline-with-target",
            rpc::CreateDpuExtensionServiceRequest {
                deployment_type: baseline,
                dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart as i32,
                data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
                ..Default::default()
            },
            "dpu_target is unsupported for baseline helm services",
        ),
        // Reject baseline for Kubernetes Pod services; only DPF Helm charts
        // support deployment to all DPUs at creation time.
        (
            "baseline-pod",
            rpc::CreateDpuExtensionServiceRequest {
                deployment_type: baseline,
                service_type: rpc::DpuExtensionServiceType::KubernetesPod as i32,
                data: TEST_SERVICE_DATA.to_string(),
                ..Default::default()
            },
            "baseline deployment_type is supported only for DPF helm chart services",
        ),
        // Supply a valid target so an incorrect DemandDriven fallback cannot
        // satisfy this case through a missing-target error.
        (
            "unknown-deployment-type",
            rpc::CreateDpuExtensionServiceRequest {
                deployment_type: 99,
                dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::All as i32),
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart as i32,
                data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
                ..Default::default()
            },
            "invalid deployment_type",
        ),
        // Reject service VPC interfaces for baseline because NICo creates those
        // interfaces through instance attachment.
        (
            "baseline-with-service-vpc",
            rpc::CreateDpuExtensionServiceRequest {
                deployment_type: baseline,
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart as i32,
                data: TEST_DPF_HELM_CHART_SERVICE_DATA.to_string(),
                service_vpc_interfaces: vec![rpc::ServiceVpcInterfaceRequirement {
                    address_family: rpc::ServiceVpcAddressFamily::Ipv4 as i32,
                }],
                ..Default::default()
            },
            "service VPC interfaces are unsupported for baseline helm services",
        ),
    ];

    for (name, mut request, expected_message) in cases {
        // Use valid IDs, names, and tenant metadata so each request fails
        // because of the unsupported combination being tested.
        let service_id = ExtensionServiceId::new();
        request.service_id = Some(service_id.to_string());
        request.service_name = name.to_string();
        request.tenant_organization_id = "best_org".to_string();

        // Require the intended rejection rather than any InvalidArgument response.
        let rejected = env
            .api
            .create_dpu_extension_service(Request::new(request))
            .await
            .expect_err("invalid deployment-type combinations must fail");
        assert_eq!(rejected.code(), tonic::Code::InvalidArgument, "{name}");
        assert_eq!(rejected.message(), expected_message, "{name}");

        // Check that no service was saved, so the controller cannot deploy
        // a service from the rejected request.
        let mut txn = env.pool.begin().await?;
        assert!(
            db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
                .await?
                .is_empty(),
            "{name}"
        );
        txn.commit().await?;
    }

    Ok(())
}

#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_reconciliation_accepts_owned_already_exists(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let expected = project_dpu_service(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
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

/// Verifies a terminating object reread after an owned create conflict remains unavailable.
/// Waiting before retry prevents Ready from naming an object Kubernetes has committed to delete.
#[crate::sqlx_test]
async fn test_dpf_helm_chart_create_reconciliation_waits_for_terminating_already_exists(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let service_id = ExtensionServiceId::new();
    let data =
        model::extension_service::DpfHelmChartServiceData::parse(TEST_DPF_HELM_CHART_SERVICE_DATA)?;
    let expected = project_dpu_service(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
    let expected_name = expected.name.clone();
    let mut terminating = dpu_service_observation(&expected);
    // Make the conflicting object finalizing so the reread cannot safely adopt it.
    terminating.is_deleting = true;
    let create_calls = Arc::new(AtomicUsize::new(0));

    // The first create loses to a terminating object; the retry succeeds after it disappears.
    let mut mock = MockDpfOperations::new();
    mock.expect_create_dpu_service().times(2).returning({
        let create_calls = Arc::clone(&create_calls);
        move |service| {
            if create_calls.fetch_add(1, Ordering::Relaxed) == 0 {
                Err(DpfError::already_exists(
                    "DPUService",
                    expected_name.clone(),
                ))
            } else {
                Ok(dpu_service_observation(service))
            }
        }
    });
    mock.expect_get_dpu_service()
        .times(1)
        .returning(move |_| Ok(Some(terminating.clone())));
    let env = create_dpf_controller_test_env(db_pool, mock).await;
    create_test_tenants(&env).await?;
    // Seed the known UUID directly to isolate controller conflict handling from API admission.
    seed_dpf_helm_chart_service_with_id(&env, service_id, "controller-finalizing-conflict").await?;

    // The conflict reread persists a retry outcome rather than accepting terminating state.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let waiting = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record remains available while finalization is pending");
    assert_eq!(
        waiting.status.controller_state.value,
        ExtensionServiceLifecycleState::Creating
    );
    assert!(matches!(
        waiting.status.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { ref reason, .. })
            if reason == "DPF DPUService finalization is in progress; retrying"
    ));
    txn.commit().await?;

    // A later create proves the wait remains retryable and converges after finalization.
    env.run_extension_service_controller_iteration().await;
    let mut txn = env.pool.begin().await?;
    let ready = db::extension_service::find_by_ids(&mut txn, &[service_id], false, false)
        .await?
        .pop()
        .expect("controller record remains available after retry");
    assert_eq!(
        ready.status.controller_state.value,
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
    let expected = project_dpu_service(
        service_id,
        carbide_dpf::NAMESPACE,
        &data,
        ExtensionServiceDeploymentType::DemandDriven,
    );
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
            if reason == "DPF SDK is unavailable; retrying extension-service registration"
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
        model::extension_service::ExtensionServiceDeploymentType::DemandDriven,
        None,
        "controller-non-dpf",
        &tenant,
        None,
        &[],
        TEST_SERVICE_DATA,
        None,
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
    // Model a stale Creating writer after deletion wins the database race, proving its old version
    // cannot overwrite Deleted with Ready.
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(create_observability()),
        deployment_type: 0,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: Some(create_credential()),
        observability: Some(create_observability()),
        deployment_type: 0,
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
                service_vpc_interfaces: None,
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
                service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test empty data
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: "".to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid data format (not YAML or JSON)
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test".to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid credential registry URL
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        dpu_target: None,
        service_vpc_interfaces: vec![],
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
        deployment_type: 0,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid observability config
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        dpu_target: None,
        service_vpc_interfaces: vec![],
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
        deployment_type: 0,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Test invalid observability config name
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        dpu_target: None,
        service_vpc_interfaces: vec![],
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
        deployment_type: 0,
    };

    let create_resp = env
        .api
        .create_dpu_extension_service(Request::new(extension_service))
        .await;
    assert!(create_resp.is_err());

    // Fail to create an an extension with too many observability configs
    let extension_service = rpc::CreateDpuExtensionServiceRequest {
        dpu_target: None,
        service_vpc_interfaces: vec![],
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
        deployment_type: 0,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
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
        deployment_type: 0,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,

        tenant_organization_id: "best_org".to_string(),
        deployment_type: 0,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "Test-Service".to_string(),
        description: Some("Test service".to_string()),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,

        tenant_organization_id: "best_org".to_string(),
        deployment_type: 0,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "test-service".to_string(),
        description: Some("Test service".to_string()),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,

        tenant_organization_id: "another_org".to_string(),
        deployment_type: 0,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "other-test-service".to_string(),
        description: Some("Other test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
        dpu_target: None,
        service_vpc_interfaces: vec![],
        service_id: None,
        service_name: "other-test-service".to_string(),
        description: Some("Other test service".to_string()),
        tenant_organization_id: "best_org".to_string(),
        service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
        data: TEST_SERVICE_DATA.to_string(),
        credential: None,
        observability: None,
        deployment_type: 0,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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
                service_vpc_ids: vec![],
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
            service_vpc_interfaces: None,
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
            service_vpc_interfaces: None,
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

/// Verifies service-reference lookup prefers an active attachment independent of stored order,
/// returns one retained tombstone when only terminating versions remain, selects exact versions,
/// includes soft-deleted instances, and gives attachment removal timestamps precedence over the
/// instance-deletion fallback, keeping lifecycle consumers tied to stored owners throughout detach
/// and instance deletion.
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
            service_vpc_interfaces: None,
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
                service_vpc_ids: vec![],
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
                service_vpc_ids: vec![],
            }],
        })
        .build_and_return()
        .await;

    // Upgrade instance1 through the public API to retain its terminating v1 attachment beside the
    // active v2 attachment selected by unversioned lookup.
    env.api
        .update_instance_config(Request::new(rpc::InstanceConfigUpdateRequest {
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(crate::tests::common::api_fixtures::instance::default_tenant_config()),
                os: Some(crate::tests::common::api_fixtures::instance::default_os_config()),
                network: Some(
                    crate::tests::common::api_fixtures::instance::single_interface_network_config(
                        segment_id,
                    ),
                ),
                dpu_extension_services: Some(rpc::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::InstanceDpuExtensionServiceConfig {
                        service_id: service_id.clone(),
                        version: version2.clone(),
                        service_vpc_ids: vec![],
                    }],
                }),
                ..Default::default()
            }),
            instance_id: Some(instance1.id),
            metadata: Some(rpc::Metadata {
                name: "version-filtered-service-lookup".to_string(),
                description: "tests/extension_service".to_string(),
                labels: vec![],
            }),
        }))
        .await?;

    // Preserve distinct timestamps so each lookup proves which removal source won.
    let parsed_service_id: ExtensionServiceId = service_id.parse()?;
    let parsed_version1: ConfigVersion = version1.parse()?;
    let parsed_version2: ConfigVersion = version2.parse()?;
    let upgraded = db::instance::find_by_id(&env.pool, instance1.id)
        .await?
        .expect("updated instance remains stored");
    let attachment_removed_at = upgraded
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| {
            attachment.service_id == parsed_service_id && attachment.version == parsed_version1
        })
        .and_then(|attachment| attachment.removed)
        .expect("replaced v1 attachment is terminating");
    let instance_deleted_at = attachment_removed_at + chrono::Duration::seconds(1);

    // Store the terminating attachment first so active preference cannot depend on JSON order.
    let mut tombstone_first_config = upgraded.config.extension_services.clone();
    assert_eq!(tombstone_first_config.service_configs.len(), 2);
    assert!(tombstone_first_config.service_configs[0].removed.is_none());
    assert!(tombstone_first_config.service_configs[1].removed.is_some());
    tombstone_first_config.service_configs.reverse();
    let mut txn = env.db_txn().await;
    assert_eq!(
        db::instance::update_extension_services_config(
            txn.as_mut(),
            upgraded.id,
            upgraded.extension_services_config_version,
            &upgraded.config.extension_services,
            &tombstone_first_config,
            false,
        )
        .await?,
        db::ConditionalWrite::Applied(()),
    );
    sqlx::query("UPDATE instances SET deleted = $2 WHERE id = $1")
        .bind(instance1.id)
        .bind(instance_deleted_at)
        .execute(txn.as_mut())
        .await?;
    txn.commit().await?;
    let attachment_removed_at = attachment_removed_at.to_string();

    // Use the persisted TIMESTAMPTZ representation expected from the API.
    let instance_deleted_at = db::instance::find_by_id(&env.pool, instance1.id)
        .await?
        .expect("deleted instance remains stored")
        .deleted
        .expect("instance deletion timestamp was persisted")
        .to_string();

    // Unfiltered lookup reports each active attachment, including the one whose
    // instance-level deletion supplies its removal timestamp.
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

    // The upgraded, deleted instance resolves to active v2, not its earlier v1 row.
    let instance1_info = instances
        .iter()
        .find(|i| i.instance_id == instance1.id.to_string())
        .unwrap();
    assert_eq!(instance1_info.service_id, service_id);
    assert_eq!(instance1_info.version, version2);
    assert_eq!(
        instance1_info.removed.as_deref(),
        Some(instance_deleted_at.as_str())
    );

    // The second instance remains an active v2 control with no removal source.
    let instance2_info = instances
        .iter()
        .find(|i| i.instance_id == instance2.id.to_string())
        .unwrap();
    assert_eq!(instance2_info.service_id, service_id);
    assert_eq!(instance2_info.version, version2);
    assert!(instance2_info.removed.is_none());

    // Exact v1 lookup must skip active v2 and preserve the earlier attachment timestamp.
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
    assert_eq!(
        instances[0].removed.as_deref(),
        Some(attachment_removed_at.as_str())
    );

    // Exact v2 lookup returns both active attachments and applies deletion
    // fallback only to instance1.
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
    assert_eq!(instances.len(), 2);
    let instance1_info = instances
        .iter()
        .find(|info| info.instance_id == instance1.id.to_string())
        .expect("deleted instance1 retains its active v2 attachment");
    assert_eq!(instance1_info.service_id, service_id);
    assert_eq!(instance1_info.version, version2);
    assert_eq!(
        instance1_info.removed.as_deref(),
        Some(instance_deleted_at.as_str())
    );
    let instance2_info = instances
        .iter()
        .find(|info| info.instance_id == instance2.id.to_string())
        .expect("active instance2 retains its v2 attachment");
    assert_eq!(instance2_info.service_id, service_id);
    assert_eq!(instance2_info.version, version2);
    assert!(instance2_info.removed.is_none());

    // Switch instance2 from v2 to v1 through the public API so final detach retains two historical
    // versions without depending on direct fixture construction.
    let current = instance2.rpc_instance().await.into_inner();
    let mut switched_config = current.config.expect("active instance has config");
    switched_config.dpu_extension_services = Some(rpc::InstanceDpuExtensionServicesConfig {
        service_configs: vec![rpc::InstanceDpuExtensionServiceConfig {
            service_id: service_id.clone(),
            version: version1.clone(),
            service_vpc_ids: vec![],
        }],
    });
    env.api
        .update_instance_config(Request::new(rpc::InstanceConfigUpdateRequest {
            instance_id: Some(instance2.id),
            metadata: current.metadata,
            config: Some(switched_config),
            ..Default::default()
        }))
        .await?;

    // Detach the active v1 attachment while preserving the earlier v2 tombstone.
    let current = instance2.rpc_instance().await.into_inner();
    let mut detached_config = current.config.expect("updated instance has config");
    detached_config.dpu_extension_services = Some(rpc::InstanceDpuExtensionServicesConfig {
        service_configs: vec![],
    });
    env.api
        .update_instance_config(Request::new(rpc::InstanceConfigUpdateRequest {
            instance_id: Some(instance2.id),
            metadata: current.metadata,
            config: Some(detached_config),
            ..Default::default()
        }))
        .await?;

    // Reload both retained tombstones so unversioned and exact lookups can be checked against the
    // durable version/timestamp pairs without imposing an order between terminating versions.
    let detached = db::instance::find_by_id(&env.pool, instance2.id)
        .await?
        .expect("detached instance remains stored");
    let detached_attachments = detached
        .config
        .extension_services
        .service_configs
        .iter()
        .filter(|attachment| attachment.service_id == parsed_service_id)
        .collect::<Vec<_>>();
    assert_eq!(detached_attachments.len(), 2);
    assert!(
        detached_attachments
            .iter()
            .all(|attachment| attachment.removed.is_some())
    );
    let detached_version1_removed_at = detached_attachments
        .iter()
        .find(|attachment| attachment.version == parsed_version1)
        .and_then(|attachment| attachment.removed)
        .expect("detached v1 attachment is terminating")
        .to_string();
    let detached_version2_removed_at = detached_attachments
        .iter()
        .find(|attachment| attachment.version == parsed_version2)
        .and_then(|attachment| attachment.removed)
        .expect("detached v2 attachment is terminating")
        .to_string();

    // Unversioned lookup returns exactly one retained tombstone whose timestamp matches its
    // selected version, without promising which terminating version wins.
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
    let instance2_infos = instances
        .iter()
        .filter(|info| info.instance_id == instance2.id.to_string())
        .collect::<Vec<_>>();
    assert_eq!(instance2_infos.len(), 1);
    let instance2_info = instance2_infos[0];
    assert_eq!(instance2_info.service_id, service_id);
    assert!(
        (instance2_info.version == version1
            && instance2_info.removed.as_deref() == Some(detached_version1_removed_at.as_str()))
            || (instance2_info.version == version2
                && instance2_info.removed.as_deref()
                    == Some(detached_version2_removed_at.as_str()))
    );

    let exact_version_cases = [
        // Exact v1 selects the attachment removed by the final detach, distinguishing it from v2.
        (version1.clone(), detached_version1_removed_at.as_str()),
        // Exact v2 selects the earlier replacement tombstone, distinguishing it from v1.
        (version2.clone(), detached_version2_removed_at.as_str()),
    ];
    for (requested_version, expected_removed_at) in exact_version_cases {
        let find_resp = env
            .api
            .find_instances_by_dpu_extension_service(Request::new(
                rpc::FindInstancesByDpuExtensionServiceRequest {
                    service_id: service_id.clone(),
                    version: Some(requested_version.clone()),
                },
            ))
            .await?;
        let instance2_infos = find_resp
            .into_inner()
            .instances
            .into_iter()
            .filter(|info| info.instance_id == instance2.id.to_string())
            .collect::<Vec<_>>();
        assert_eq!(instance2_infos.len(), 1);
        assert_eq!(instance2_infos[0].service_id, service_id);
        assert_eq!(instance2_infos[0].version, requested_version);
        assert_eq!(
            instance2_infos[0].removed.as_deref(),
            Some(expected_removed_at),
        );
    }

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
            dpu_target: None,
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "test-service-2".to_string(),
            description: Some("Second test service".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::DpuExtensionServiceType::KubernetesPod.into(),
            data: TEST_SERVICE_DATA_VERSION_2.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
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
                    service_vpc_ids: vec![],
                },
                rpc::InstanceDpuExtensionServiceConfig {
                    service_id: service2_id.clone(),
                    version: service2_version.clone(),
                    service_vpc_ids: vec![],
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

#[crate::sqlx_test]
async fn test_rejected_helm_placement_observation_does_not_advance_readiness(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    use std::collections::BTreeMap;

    use chrono::{Duration, Utc};
    use model::extension_service::{DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DpfHelmChartIdentity};
    use model::instance::status::extension_service::{
        ExtensionServiceDeploymentStatus, ExtensionServiceStatusObservation,
        InstanceExtensionServiceStatusObservation,
    };
    use model::machine::{InstanceState, ManagedHostState};

    use crate::tests::common::api_fixtures::create_managed_host_with_dpf;
    use crate::tests::common::api_fixtures::instance::{
        default_os_config, single_interface_network_config,
    };

    let service_id = ExtensionServiceId::new();
    let placement_labels = BTreeMap::from([(
        DpfHelmChartIdentity::from_service_id(service_id).placement_label_key,
        DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.to_string(),
    )]);
    let label_reads = Arc::new(AtomicUsize::new(0));
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(carbide_dpf::DpuPhase::Ready)));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(carbide_dpf::DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_get_service_versions_for_dpu()
        .returning(|_| Ok(vec![]));
    mock.expect_create_dpu_service()
        .returning(|service| Ok(dpu_service_observation(service)));
    mock.expect_merge_dpu_device_node_labels()
        .returning(|_, _| Ok(()));
    let reads = label_reads.clone();
    mock.expect_get_dpu_device_node_labels()
        .returning(move |_| {
            reads.fetch_add(1, Ordering::SeqCst);
            Ok(placement_labels.clone())
        });

    let mut site_config = get_config();
    site_config.dpf.enabled = true;
    site_config.dpf.deployments.bf3.bfb_url = Some("http://example.com/test.bfb".into());
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(site_config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    create_test_tenants(&env).await?;
    let mh = create_managed_host_with_dpf(&env).await;
    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            rpc::VpcCreationRequest::builder("best_org")
                .metadata(rpc::Metadata {
                    name: "rejected-placement".into(),
                    ..Default::default()
                })
                .rpc(),
        )
        .await;
    let service = env
        .api
        .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
            service_id: Some(service_id.to_string()),
            service_name: "rejected-placement".into(),
            service_type: rpc::DpuExtensionServiceType::DpfHelmChart as i32,
            dpu_target: Some(rpc::DpuExtensionServiceDpuTarget::Primary as i32),
            tenant_organization_id: "best_org".into(),
            data: TEST_DPF_HELM_CHART_SERVICE_DATA.into(),
            ..Default::default()
        }))
        .await?
        .into_inner();
    env.run_extension_service_controller_iteration().await;
    env.api
        .allocate_instance(Request::new(rpc::InstanceAllocationRequest {
            machine_id: Some(mh.host().id),
            config: Some(rpc::InstanceConfig {
                tenant: Some(rpc::TenantConfig {
                    tenant_organization_id: "best_org".into(),
                    ..Default::default()
                }),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                dpu_extension_services: Some(rpc::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::InstanceDpuExtensionServiceConfig {
                        service_id: service_id.to_string(),
                        version: service.latest_version_info.unwrap().version,
                        service_vpc_ids: vec![],
                    }],
                }),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await?;

    let waiting_state = ManagedHostState::Assigned {
        instance_state: InstanceState::WaitingForExtensionServicesConfig,
    };
    let mut txn = env.db_txn().await;
    db::machine::update_state(txn.as_mut(), &mh.id.into(), &waiting_state).await?;
    let snapshot = mh.snapshot(&mut txn).await;
    let instance = snapshot.instance.as_ref().unwrap();
    let config = &instance.config.extension_services.service_configs[0];
    // A future timestamp forces rejection of this pass's `Running` candidate.
    // This tests the rejection branch, not overlapping controller passes.
    let accepted_observation = InstanceExtensionServiceStatusObservation {
        config_version: instance.extension_services_config_version,
        instance_config_version: None,
        observed_at: Utc::now() + Duration::days(1),
        extension_service_statuses: vec![ExtensionServiceStatusObservation {
            service_id,
            service_type: ExtensionServiceType::DpfHelmChart,
            service_name: String::new(),
            version: config.version,
            dpu_target: config.dpu_target,
            removed: None,
            overall_state: ExtensionServiceDeploymentStatus::Error,
            components: vec![],
            message: "placement could not be verified".into(),
        }],
    };
    assert_eq!(
        db::machine::update_extension_service_status_observation(
            txn.as_mut(),
            &mh.dpu().id,
            ExtensionServiceType::DpfHelmChart,
            &accepted_observation,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    txn.commit().await?;

    env.run_machine_state_controller_iteration().await;

    assert_eq!(label_reads.load(Ordering::SeqCst), 1);
    let mut txn = env.db_txn().await;
    let persisted = mh.snapshot(&mut txn).await;
    assert_eq!(persisted.host_snapshot.current_state(), &waiting_state);
    assert_eq!(
        persisted.host_snapshot.current_version(),
        snapshot.host_snapshot.current_version()
    );
    assert_eq!(
        persisted.host_snapshot.controller_state_outcome,
        snapshot.host_snapshot.controller_state_outcome
    );
    assert_eq!(
        persisted.dpu_snapshots[0]
            .status
            .extension_service_status_observations
            .for_service_type(ExtensionServiceType::DpfHelmChart),
        Some(&accepted_observation)
    );
    txn.commit().await?;
    Ok(())
}

#[crate::sqlx_test]
async fn test_helm_target_placement_status_and_detach(
    pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    use std::collections::{BTreeMap, HashMap};
    use std::sync::Mutex;

    use model::extension_service::{DPF_HELM_CHART_PLACEMENT_LABEL_VALUE, DpfHelmChartIdentity};

    use crate::tests::common::api_fixtures::create_managed_host_with_dpf_multi;

    let labels = Arc::new(Mutex::new(
        HashMap::<String, BTreeMap<String, String>>::new(),
    ));
    let failed_device = Arc::new(Mutex::new(None::<String>));
    let mut mock = MockDpfOperations::new();
    mock.expect_register_dpu_device()
        .returning(|_, _, _| Ok(()));
    mock.expect_register_dpu_node().returning(|_| Ok(()));
    mock.expect_release_maintenance_hold().returning(|_| Ok(()));
    mock.expect_is_reboot_required().returning(|_| Ok(false));
    mock.expect_get_dpu_phase_observation()
        .returning(|_, _| Ok(phase_observation(carbide_dpf::DpuPhase::Ready)));
    mock.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(carbide_dpf::DpuDeploymentType::Bf3));
    mock.expect_verify_node_labels().returning(|_, _| Ok(true));
    mock.expect_get_service_versions_for_dpu()
        .returning(|_| Ok(vec![]));
    mock.expect_create_dpu_service()
        .returning(|service| Ok(dpu_service_observation(service)));
    let writes = labels.clone();
    let failure = failed_device.clone();
    mock.expect_merge_dpu_device_node_labels()
        .returning(move |device, delta| {
            if failure.lock().unwrap().as_deref() == Some(device) {
                return Err(DpfError::timeout("placement", "injected placement failure"));
            }
            let mut labels = writes.lock().unwrap();
            let labels = labels.entry(device.to_owned()).or_default();
            for (key, value) in delta {
                if let Some(value) = value {
                    labels.insert(key, value);
                } else {
                    labels.remove(&key);
                }
            }
            Ok(())
        });
    let reads = labels.clone();
    mock.expect_get_dpu_device_node_labels()
        .returning(move |device| {
            Ok(reads
                .lock()
                .unwrap()
                .get(device)
                .cloned()
                .unwrap_or_default())
        });
    let mut site_config = get_config();
    site_config.dpf.enabled = true;
    site_config.dpf.deployments.bf3.bfb_url = Some("http://example.com/test.bfb".into());
    let env = create_test_env_with_overrides(
        pool.clone(),
        TestEnvOverrides::with_config(site_config).with_dpf_sdk(Arc::new(mock)),
    )
    .await;
    let mh = create_managed_host_with_dpf_multi(&env, 3).await;
    let segments = env.create_vpc_and_tenant_segments(2).await;
    let mut txn = env.db_txn().await;
    let host = mh.snapshot(&mut txn).await.host_snapshot;
    txn.commit().await?;
    let primary = host.primary_attached_dpu_machine_id().unwrap();
    let (_, mapping) = host.get_dpu_device_and_id_mappings()?;
    let secondary = *mapping
        .values()
        .flatten()
        .find(|id| **id != primary)
        .unwrap();
    let used_dpus = [primary, secondary];
    let mut network = rpc::InstanceNetworkConfig::default();
    for (dpu, segment) in used_dpus.iter().zip(segments) {
        let locator = host.get_device_locator_for_dpu_id(dpu)?;
        let mut interface =
            crate::tests::common::api_fixtures::instance::single_interface_network_config(segment)
                .interfaces
                .remove(0);
        interface.device = Some(locator.device);
        interface.device_instance = locator.device_instance as u32;
        network.interfaces.push(interface);
    }
    let instance = mh.instance_builer(&env).network(network).build().await;
    let tenant = instance
        .rpc_instance()
        .await
        .into_inner()
        .config
        .unwrap()
        .tenant
        .unwrap()
        .tenant_organization_id;
    env.api
        .create_tenant(Request::new(rpc::CreateTenantRequest {
            organization_id: tenant.clone(),
            metadata: Some(rpc::Metadata {
                name: tenant.clone(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await?;
    let mut registrations = Vec::new();
    for (name, target) in [
        // Primary placement covers only the host's primary attached DPU.
        ("primary-policy", 0),
        // All placement covers every attached DPU, including inactive ones.
        ("all-policy", 2),
        // All-active placement excludes attached DPUs unused by the instance.
        ("all-active-policy", 1),
    ] {
        let service = env
            .api
            .create_dpu_extension_service(Request::new(rpc::CreateDpuExtensionServiceRequest {
                service_name: name.into(),
                service_type: rpc::DpuExtensionServiceType::DpfHelmChart as i32,
                dpu_target: Some(target),
                tenant_organization_id: tenant.clone(),
                data: TEST_DPF_HELM_CHART_SERVICE_DATA
                    .replace("tenant-service-v1", &format!("tenant-service-v{target}")),
                deployment_type: 0,
                ..Default::default()
            }))
            .await?
            .into_inner();
        registrations.push(service);
    }
    env.run_extension_service_controller_iteration().await;
    let current = instance.rpc_instance().await.into_inner();
    let mut config = current.config.unwrap();
    config.dpu_extension_services = Some(rpc::InstanceDpuExtensionServicesConfig {
        service_configs: registrations
            .iter()
            .map(|service| rpc::InstanceDpuExtensionServiceConfig {
                service_id: service.service_id.clone(),
                version: service
                    .latest_version_info
                    .as_ref()
                    .unwrap()
                    .version
                    .clone(),
                service_vpc_ids: vec![],
            })
            .collect(),
    });
    env.api
        .update_instance_config(Request::new(rpc::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            metadata: current.metadata.clone(),
            config: Some(config.clone()),
            ..Default::default()
        }))
        .await?;
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    txn.commit().await?;
    let attached = &snapshot
        .instance
        .as_ref()
        .unwrap()
        .config
        .extension_services
        .service_configs;
    assert_eq!(
        attached
            .iter()
            .map(|config| config.dpu_target)
            .collect::<Vec<_>>(),
        vec![
            Some(model::extension_service::DpuTarget::Primary),
            Some(model::extension_service::DpuTarget::All),
            Some(model::extension_service::DpuTarget::AllActive)
        ]
    );
    let primary = snapshot
        .host_snapshot
        .primary_attached_dpu_machine_id()
        .unwrap();
    let devices: Vec<_> = snapshot
        .dpu_snapshots
        .iter()
        .map(|dpu| (dpu.id, dpu.dpf_id().unwrap()))
        .collect();
    let primary_key = DpfHelmChartIdentity::from_service_id(registrations[0].service_id.parse()?)
        .placement_label_key;
    let all_key = DpfHelmChartIdentity::from_service_id(registrations[1].service_id.parse()?)
        .placement_label_key;
    let all_active_key =
        DpfHelmChartIdentity::from_service_id(registrations[2].service_id.parse()?)
            .placement_label_key;
    for (_, name) in &devices {
        labels.lock().unwrap().insert(
            name.clone(),
            BTreeMap::from([
                ("unrelated-owner".into(), "preserved".into()),
                (
                    all_active_key.clone(),
                    DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.into(),
                ),
            ]),
        );
    }
    for _ in 0..3 {
        env.run_machine_state_controller_iteration().await;
    }
    {
        let labels = labels.lock().unwrap();
        for (id, name) in &devices {
            let labels = &labels[name];
            assert_eq!(
                labels.get(&primary_key).map(String::as_str),
                (*id == primary).then_some(DPF_HELM_CHART_PLACEMENT_LABEL_VALUE)
            );
            assert_eq!(
                labels.get(&all_key).map(String::as_str),
                Some(DPF_HELM_CHART_PLACEMENT_LABEL_VALUE)
            );
            assert_eq!(
                labels.get(&all_active_key).map(String::as_str),
                used_dpus
                    .contains(id)
                    .then_some(DPF_HELM_CHART_PLACEMENT_LABEL_VALUE)
            );
            assert_eq!(labels["unrelated-owner"], "preserved");
        }
    }
    let public = instance
        .rpc_instance()
        .await
        .into_inner()
        .status
        .unwrap()
        .dpu_extension_services
        .unwrap();
    for (service, count) in registrations.iter().zip([1, 3, 2]) {
        let status = public
            .dpu_extension_services
            .iter()
            .find(|status| status.service_id == service.service_id)
            .unwrap();
        assert_eq!(status.dpu_statuses.len(), count);
        assert_eq!(
            status.deployment_status,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32
        );
    }
    // Simulate a former primary label that must be removed even though it is no longer a target.
    let former_primary = devices
        .iter()
        .find(|(id, _)| !used_dpus.contains(id))
        .unwrap()
        .1
        .clone();
    labels
        .lock()
        .unwrap()
        .get_mut(&former_primary)
        .unwrap()
        .insert(
            primary_key.clone(),
            DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.into(),
        );
    labels
        .lock()
        .unwrap()
        .get_mut(&former_primary)
        .unwrap()
        .insert(
            all_active_key.clone(),
            DPF_HELM_CHART_PLACEMENT_LABEL_VALUE.into(),
        );
    *failed_device.lock().unwrap() = Some(former_primary.clone());
    config.dpu_extension_services = Some(rpc::InstanceDpuExtensionServicesConfig::default());
    env.api
        .update_instance_config(Request::new(rpc::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            metadata: current.metadata.clone(),
            config: Some(config),
            ..Default::default()
        }))
        .await?;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    assert!(
        !instance
            .db_instance(&mut txn)
            .await
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    txn.commit().await?;
    assert!(labels.lock().unwrap()[&former_primary].contains_key(&primary_key));
    assert!(labels.lock().unwrap()[&former_primary].contains_key(&all_active_key));
    *failed_device.lock().unwrap() = None;
    for _ in 0..3 {
        env.run_machine_state_controller_iteration().await;
    }
    let mut txn = env.db_txn().await;
    assert!(
        instance
            .db_instance(&mut txn)
            .await
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    txn.commit().await?;
    for labels in labels.lock().unwrap().values() {
        assert!(!labels.contains_key(&primary_key));
        assert!(!labels.contains_key(&all_key));
        assert!(!labels.contains_key(&all_active_key));
        assert_eq!(labels["unrelated-owner"], "preserved");
    }
    Ok(())
}
