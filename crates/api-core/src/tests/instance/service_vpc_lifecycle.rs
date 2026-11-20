// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Public service-VPC lifecycle wiring with persisted allocations and revision-fenced DPF effects.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use ::rpc::forge as rpc;
use carbide_dpf::{
    DPU_MACHINE_ID_LABEL, DpfResourcePreconditions, DpuClusterIdentity,
    DpuDevicePlacementObservation, ServiceVpcChainIntent, ServiceVpcChainObservation,
    ServiceVpcChainState, ServiceVpcNodeIdentity, ServiceVpcResourceObservation,
};
use db::TransactionVending as _;
use model::extension_service::service_vpc::ServiceVpcEndpointStatusObservation;
use model::instance::config::network::InstanceServiceInterfaceConfig;
use model::instance::status::extension_service::ExtensionServiceDeploymentStatus;
use sqlx::Connection as _;

use super::*;

/// Retains the remote device revisions and Node UIDs needed to reject stale effects across retries
/// and Core restarts while exposing specific transient failures and incomplete chain cleanup.
/// Captures chain requests and a concurrent writer to verify bypass persistence after stale observations.
#[derive(Default)]
struct LifecycleDpfState {
    devices: BTreeMap<String, (DpuMachineId, DpuDevicePlacementObservation)>,
    nodes: BTreeMap<DpuMachineId, ServiceVpcNodeIdentity>,
    fail_next_identity_preparation: Option<DpuMachineId>,
    fail_next_placement: Option<DpuMachineId>,
    omit_node_once: Option<DpuMachineId>,
    omit_placement_stamp_once: Option<DpuMachineId>,
    delay_next_placement: Option<(
        tokio::sync::oneshot::Receiver<()>,
        tokio::sync::oneshot::Sender<()>,
    )>,
    delayed_placement_result: Option<Result<(), DpfError>>,
    stale_chain_contexts: BTreeMap<
        ExtensionServiceId,
        model::extension_service::service_vpc::ServiceVpcChainRequestContext,
    >,
    remaining_chains_on_dpus: BTreeSet<(ExtensionServiceId, DpuMachineId)>,
    unacknowledged_exclusions: BTreeSet<ExtensionServiceId>,
    remaining_chains: BTreeSet<ExtensionServiceId>,
    placement_reads: usize,
    successful_placements: BTreeMap<DpuMachineId, usize>,
    newly_detected_cleanup_bypasses: BTreeSet<ExtensionServiceId>,
    chain_requests: Vec<(
        model::extension_service::service_vpc::ServiceVpcChainRequestContext,
        bool,
    )>,
    before_next_active_placement:
        Option<Box<dyn FnOnce(DpuMachineId, ServiceVpcNodeIdentity) + Send>>,
    before_next_chain_result: Option<Box<dyn FnOnce() + Send>>,
    reboot_completions: usize,
}

/// Applies immediate and delayed simulated server writes with identical UID and revision checks,
/// rejecting requests whose captured DPUDevice UID or resource version no longer matches.
fn apply_lifecycle_placement(
    state: &mut LifecycleDpfState,
    device: &str,
    changes: BTreeMap<String, Option<String>>,
    version: &str,
    preconditions: &DpfResourcePreconditions,
) -> Result<(), DpfError> {
    // Check the captured remote incarnation and revision before changing labels or the configuration stamp.
    let dpu_id = state
        .devices
        .get(device)
        .expect("read precedes placement")
        .0;
    let omit_stamp = state.omit_placement_stamp_once == Some(dpu_id);
    let observed = &mut state.devices.get_mut(device).expect("known device").1;
    if observed.resource.uid != preconditions.uid
        || observed.resource.resource_version != preconditions.resource_version
    {
        return Err(DpfError::InvalidState(
            "stale placement revision".to_owned(),
        ));
    }
    // Apply the complete label delta and stamp together unless testing unstamped withdrawal, preserving unrelated labels.
    for (key, value) in changes {
        if let Some(value) = value {
            observed.node_labels.insert(key, value);
        } else {
            observed.node_labels.remove(&key);
        }
    }
    observed.config_version = (!omit_stamp).then(|| version.to_owned());
    observed.resource.generation += 1;
    observed.resource.resource_version = (observed
        .resource
        .resource_version
        .parse::<u64>()
        .expect("revision")
        + 1)
    .to_string();
    if omit_stamp {
        state.omit_placement_stamp_once = None;
    }
    *state.successful_placements.entry(dpu_id).or_default() += 1;
    Ok(())
}

/// Supplies per-DPU revision fencing and typed chain observations so controller tests exercise
/// persisted lifecycle decisions while the SDK's retained tests own Kubernetes protocol validation.
fn lifecycle_dpf(state: Arc<Mutex<LifecycleDpfState>>) -> Arc<dyn DpfOperations> {
    let mut sdk = MockDpfOperations::new();
    sdk.expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    let identity_preparation = state.clone();
    sdk.expect_ensure_dpu_device_node_identity()
        .returning(move |_, dpu_id| {
            // Consume a preparation failure before placement reads can synthesize declared identity.
            let mut state = identity_preparation.lock().expect("DPF state");
            if state.fail_next_identity_preparation == Some(dpu_id) {
                state.fail_next_identity_preparation = None;
                return Err(DpfError::timeout(
                    "prepare service placement identity",
                    "injected transient identity preparation failure",
                ));
            }
            Ok(())
        });
    sdk.expect_force_delete_host().returning(|_, _| Ok(()));
    sdk.expect_verify_node_labels().returning(|_, _| Ok(true));
    let reboots = state.clone();
    sdk.expect_reboot_complete().returning(move |_| {
        reboots.lock().expect("DPF state").reboot_completions += 1;
        Ok(())
    });
    let reads = state.clone();
    sdk.expect_get_dpu_device_placement()
        .returning(move |device, expected_dpu_id| {
            // These fixtures place service endpoints on every DPU, so reads must retain identity validation.
            let dpu_id = expected_dpu_id.expect("service endpoint DPU identity");
            // Lazily instantiate the remote device when the controller first observes its real fixture ID.
            let mut state = reads.lock().expect("DPF state");
            state.placement_reads += 1;
            Ok(state
                .devices
                .entry(device.to_owned())
                .or_insert_with(|| {
                    (
                        dpu_id,
                        DpuDevicePlacementObservation {
                            resource: ServiceVpcResourceObservation {
                                namespace: "service-vpc".to_owned(),
                                name: format!("device-{device}"),
                                uid: format!("device-{device}-uid"),
                                generation: 1,
                                resource_version: "1".to_owned(),
                                is_deleting: false,
                            },
                            node_labels: BTreeMap::from([
                                (DPU_MACHINE_ID_LABEL.to_owned(), dpu_id.to_string()),
                                ("other-controller".to_owned(), "preserved".to_owned()),
                            ]),
                            config_version: None,
                        },
                    )
                })
                .1
                .clone())
        });
    let writes = state.clone();
    sdk.expect_patch_dpu_device_placement().returning(
        move |device, changes, version, preconditions| {
            let mut state = writes.lock().expect("DPF state");
            let dpu_id = state
                .devices
                .get(device)
                .expect("read precedes placement")
                .0;
            // Keep the actual controller request pending on the simulated server after an uncertain timeout.
            if let Some((release, completed)) = state.delay_next_placement.take() {
                let writes = writes.clone();
                let device = device.to_owned();
                let version = version.to_owned();
                let preconditions = preconditions.clone();
                tokio::spawn(async move {
                    release.await.expect("release delayed placement");
                    let mut state = writes.lock().expect("DPF state");
                    let result = apply_lifecycle_placement(
                        &mut state,
                        &device,
                        changes,
                        &version,
                        &preconditions,
                    );
                    state.delayed_placement_result = Some(result);
                    completed.send(()).expect("delayed placement observer");
                });
                return Err(DpfError::timeout(
                    "service placement",
                    "server completion is delayed",
                ));
            }
            // Inject one actual transient operation error while later DPUs remain independently usable.
            if state.fail_next_placement == Some(dpu_id) {
                state.fail_next_placement = None;
                return Err(DpfError::timeout(
                    "service placement",
                    "injected transient DPF failure",
                ));
            }
            // Inspect committed evidence before the first label activation can reach the device.
            if changes
                .values()
                .any(|value| value.as_deref() == Some("enabled"))
                && let Some(observe) = state.before_next_active_placement.take()
            {
                observe(
                    dpu_id,
                    state.nodes.get(&dpu_id).expect("resolved Node").clone(),
                );
            }
            apply_lifecycle_placement(&mut state, device, changes, version, preconditions)
        },
    );
    let node_reads = state.clone();
    sdk.expect_resolve_service_vpc_nodes()
        .returning(move |devices, _deadline| {
            let mut state = node_reads.lock().expect("DPF state");
            let missing_node = state.omit_node_once.take();
            Ok(devices
                .iter()
                .filter_map(|(dpu_id, device)| {
                    // A missing initial Node must leave the other resolved Nodes usable.
                    if missing_node == Some(*dpu_id) {
                        return None;
                    }
                    let node =
                        state
                            .nodes
                            .entry(*dpu_id)
                            .or_insert_with(|| ServiceVpcNodeIdentity {
                                cluster: DpuClusterIdentity {
                                    namespace: "service-vpc".to_owned(),
                                    name: "cluster".to_owned(),
                                    uid: "cluster-uid".to_owned(),
                                },
                                name: format!("node-{device}"),
                                uid: format!("node-{device}-uid"),
                            });
                    Some((*dpu_id, node.clone()))
                })
                .collect())
        });
    sdk.expect_observe_service_vpc_chains()
        .returning(move |requests, _deadline| {
            let mut state = state.lock().expect("DPF state");
            // Run the writer after reconciliation commits Node and selector evidence, before publishing its result.
            if let Some(update) = state.before_next_chain_result.take() {
                update();
            }
            state.chain_requests.extend(requests.iter().map(|request| {
                (
                    request.request_context.clone(),
                    request.cleanup_bypass_observed,
                )
            }));
            requests
                .iter()
                .map(|request| {
                    let context = &request.request_context;
                    let node = state.nodes.get(&context.dpu_id).cloned();
                    // Check the request definition's selector version and DPU exclusion against its context.
                    assert_eq!(
                        request.definition.selector_version,
                        Some(context.selector_version)
                    );
                    assert_eq!(
                        request
                            .definition
                            .excluded_dpu_ids
                            .contains(&context.dpu_id),
                        context.intent == ServiceVpcChainIntent::Retiring,
                    );
                    let cleanup_bypass_observed = request.cleanup_bypass_observed
                        || state
                            .newly_detected_cleanup_bypasses
                            .contains(&context.service_id);
                    let chain_state =
                        if cleanup_bypass_observed || node.as_ref() != Some(&request.node) {
                            ServiceVpcChainState::StaleIdentity
                        } else if context.intent == ServiceVpcChainIntent::Active {
                            ServiceVpcChainState::Converged
                        } else if state
                            .unacknowledged_exclusions
                            .contains(&context.service_id)
                        {
                            ServiceVpcChainState::ExclusionPending
                        } else if state.remaining_chains.contains(&context.service_id)
                            || state
                                .remaining_chains_on_dpus
                                .contains(&(context.service_id, context.dpu_id))
                        {
                            ServiceVpcChainState::Deleting
                        } else {
                            ServiceVpcChainState::Isolated
                        };
                    let resource = |name: &str| ServiceVpcResourceObservation {
                        namespace: request.definition.namespace.clone(),
                        name: name.to_owned(),
                        uid: format!("{name}-uid"),
                        generation: 7,
                        resource_version: "19".to_owned(),
                        is_deleting: false,
                    };
                    let list_is_current = !matches!(
                        chain_state,
                        ServiceVpcChainState::ExclusionPending
                            | ServiceVpcChainState::StaleIdentity
                    );
                    let chains = if matches!(
                        chain_state,
                        ServiceVpcChainState::Converged | ServiceVpcChainState::Deleting
                    ) {
                        vec![resource("chain")]
                    } else {
                        Vec::new()
                    };
                    ServiceVpcChainObservation {
                        request_context: state
                            .stale_chain_contexts
                            .get(&context.service_id)
                            .cloned()
                            .unwrap_or_else(|| context.clone()),
                        node,
                        cleanup_bypass_observed,
                        registration: Some(resource(&request.definition.name)),
                        set: Some(resource("set")),
                        chains,
                        chain_list_resource_version: list_is_current
                            .then(|| format!("after-ack-{}", context.selector_version)),
                        reason: match chain_state {
                            ServiceVpcChainState::StaleIdentity if cleanup_bypass_observed => {
                                Some("cleanup bypass requires administrative recovery")
                            }
                            ServiceVpcChainState::StaleIdentity => {
                                Some("retained Node UID does not match")
                            }
                            ServiceVpcChainState::ExclusionPending => {
                                Some("current exclusion is not acknowledged")
                            }
                            ServiceVpcChainState::Deleting => Some("target chain remains"),
                            _ => None,
                        },
                        state: chain_state,
                    }
                })
                .collect()
        });
    Arc::new(sdk)
}

/// Starts Core with one provisioned slot and explicit overlap and IPv4 admission so a restart can
/// change admission policy while retaining accepted endpoint inventory and external DPF state.
/// Callers must create network segments only for the initial Core fixture on a pool.
async fn lifecycle_env(
    pool: PgPool,
    state: Arc<Mutex<LifecycleDpfState>>,
    allow_overlap: bool,
    enable_ipv4: bool,
    create_network_segments: bool,
) -> TestEnv {
    let mut config = get_config();
    config.dpf.enabled = true;
    config.site_global_vpc_vni = None;
    config.dpu_config.service_vpc_slot_count = 1; // Keep shared-VPC members in slot zero.
    config.dpu_config.max_active_service_vpc_interfaces_per_dpu = 2; // Admit both members.
    config.dpu_config.allow_service_vpc_tenant_overlap = allow_overlap; // Change admission across restart.
    // Vary IPv4 admission without changing already accepted endpoint inventory.
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        [model::extension_service::ServiceVpcAddressFamily::Ipv4]
            .into_iter()
            .filter(|_| enable_ipv4)
            .collect();
    let mut overrides = TestEnvOverrides::with_config(config)
        .with_dpf_sdk(lifecycle_dpf(state))
        .with_fnn_config(None);
    // Replacement Core fixtures retain the Admin and underlay prefixes already stored in PostgreSQL.
    overrides.create_network_segments = Some(create_network_segments);
    // Instantiate shared host/service loopbacks so lifecycle cleanup must retain their references.
    overrides
        .fnn_config
        .as_mut()
        .expect("FNN configuration")
        .use_vpc_vrf_loopback = true;
    create_test_env_with_overrides(pool, overrides).await
}

/// Keeps owned fixture identifiers across Core restarts without borrowing a TestEnv through
/// TestInstance, allowing tests to observe the same persisted attachment after replacing Core.
struct LifecycleFixture {
    env: TestEnv,
    host: TestManagedHost,
    instance_id: InstanceId,
    services: Vec<DpuExtensionService>,
    service_vpc_id: VpcId,
    state: Arc<Mutex<LifecycleDpfState>>,
}

impl LifecycleFixture {
    /// Allocates through the public IPv4 path with an FNN host interface on every target DPU so
    /// tests reach lifecycle reconciliation without bypassing admission or normalized allocation.
    async fn new(
        pool: PgPool,
        dpu_count: usize,
        service_count: usize,
        allow_overlap: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let state = Arc::new(Mutex::new(LifecycleDpfState::default()));
        let env = lifecycle_env(pool, state.clone(), allow_overlap, true, true).await;
        // Create tenant FNN segments and IPv4 capacity before AllocateInstance validates networking.
        create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
        create_fixture_tenant(&env, "best_org").await?;
        let mut segments = Vec::new();
        let mut host_vpcs = Vec::new();
        for index in 0..dpu_count {
            let (vpc_id, segment_id) = create_fnn_host_segment(
                &env,
                &format!("lifecycle host VPC {index}"),
                FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[index],
            )
            .await;
            host_vpcs.push(vpc_id);
            segments.push(segment_id);
        }
        let service_vpc_id = if allow_overlap {
            host_vpcs[0]
        } else {
            create_fnn_vpc(&env, "lifecycle service VPC").await
        };
        create_tenant_overlay_prefix_with_prefix(
            &env,
            service_vpc_id,
            "lifecycle link capacity",
            "10.239.0.0/27".parse()?,
        )
        .await;
        // Capture the DPF-ingested host's DPU locators for the allocation's host interfaces.
        let host = create_service_vpc_test_host_with_dpu_count(&env, dpu_count).await?;
        let mut txn = env.db_txn().await;
        let host_machine = host.host().db_machine(&mut txn).await;
        let locators = host
            .dpu_ids
            .iter()
            .map(|dpu_id| {
                host_machine
                    .get_device_locator_for_dpu_id(dpu_id)
                    .expect("fixture DPU locator")
            })
            .collect::<Vec<_>>();
        txn.rollback().await?;
        // Mark registrations Ready before public allocation checks DPF service admission.
        let mut services = Vec::new();
        for index in 0..service_count {
            let service =
                create_networked_dpf_service(&env, &format!("lifecycle-service-{index}")).await?;
            mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
            services.push(service);
        }
        // Allocate publicly so lifecycle tests use admitted service interfaces and normalized addresses.
        let instance_id = InstanceId::new();
        let response = env
            .api
            .allocate_instance(Request::new(rpc::InstanceAllocationRequest {
                machine_id: Some(host.id),
                instance_id: Some(instance_id),
                // Select the endpoint DPUs and Service VPCs while retaining the tenant and OS
                // prerequisites for Instance admission.
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    // Select every fixture DPU so allocation creates endpoints on each one.
                    network: Some(interface_network_config_with_devices(&segments, &locators)),
                    dpu_extension_services: Some(rpc::InstanceDpuExtensionServicesConfig {
                        service_configs: services
                            .iter()
                            .map(|service| rpc::InstanceDpuExtensionServiceConfig {
                                service_id: service.service_id.clone(),
                                version: service
                                    .latest_version_info
                                    .as_ref()
                                    .expect("service version")
                                    .version
                                    .clone(),
                                // Bind each service to the VPC whose endpoint allocations are checked.
                                service_vpc_ids: vec![service_vpc_id],
                            })
                            .collect(),
                    }),
                    ..Default::default()
                }),
                metadata: Some(rpc::Metadata {
                    name: "service-vpc-lifecycle".to_owned(),
                    ..Default::default()
                }),
                instance_type_id: None,
                allow_unhealthy_machine: false,
                external_validation_request_id: None,
            }))
            .await?
            .into_inner();
        assert_eq!(response.id, Some(instance_id));
        // Find after AllocateInstance so subsequent assertions describe committed data.
        assert_eq!(
            env.one_instance(instance_id).await.into_inner().id,
            Some(instance_id)
        );
        // Drive generated service segments toward Ready before tests reconcile machine state.
        env.run_network_segment_controller_iteration().await;
        env.run_network_segment_controller_iteration().await;
        Ok(Self {
            env,
            host,
            instance_id,
            services,
            service_vpc_id,
            state,
        })
    }

    /// Reloads service_interfaces and checks expected endpoints' attachments and normalized addresses
    /// so retained capacity is verified independently of public service status.
    ///
    /// Callers must query retired endpoints' addresses separately to verify reclamation; an empty
    /// expected set checks only service_interfaces.
    async fn allocations(
        &self,
        expected: &[InstanceServiceInterfaceConfig],
    ) -> Result<InstanceSnapshot, Box<dyn std::error::Error>> {
        let stored = db::instance::find_by_id(&self.env.pool, self.instance_id)
            .await?
            .expect("retained Instance");
        assert_eq!(stored.config.network.service_interfaces, expected);
        for endpoint in expected {
            let persisted_address_allocations =
                db::instance_address::find_all_by_instance_id_and_segment_id(
                    &mut *self.env.pool.acquire().await?,
                    &self.instance_id,
                    &endpoint.network_segment_id,
                )
                .await?;
            assert_eq!(persisted_address_allocations.len(), 1);
            assert_eq!(
                persisted_address_allocations[0].prefix,
                endpoint.link_prefix
            );
            assert!(
                stored
                    .config
                    .extension_services
                    .service_configs
                    .iter()
                    .any(|config| config.id == Some(endpoint.attachment_id))
            );
        }
        Ok(stored)
    }

    /// Reads the DPU rows that hold service evidence so tests can verify the branch's persisted
    /// Node identity, versions, acknowledgement, and exclusion observations after each iteration.
    async fn observations(
        &self,
    ) -> Result<Vec<ServiceVpcEndpointStatusObservation>, Box<dyn std::error::Error>> {
        let mut observations = Vec::new();
        for dpu_id in &self.host.dpu_ids {
            let dpu = db::machine::find_one(
                &self.env.pool,
                dpu_id,
                model::machine::machine_search_config::MachineSearchConfig::default(),
            )
            .await?
            .expect("fixture DPU");
            if let Some(observed) = dpu
                .status
                .extension_service_status_observations
                .for_service_type(ExtensionServiceType::DpfHelmChart)
            {
                observations.extend(observed.service_vpc_endpoints.clone());
            }
        }
        Ok(observations)
    }

    /// Acknowledges the host and delivered Instance generations while optionally withholding the nested
    /// service report so tests isolate its requirement from the existing outer acknowledgements.
    async fn acknowledge(
        &self,
        dpu_id: DpuMachineId,
        service_applied: bool,
    ) -> Result<rpc::ManagedHostNetworkConfigResponse, Box<dyn std::error::Error>> {
        let response = self
            .env
            .api
            .get_managed_host_network_config(Request::new(rpc::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(dpu_id),
            }))
            .await?
            .into_inner();
        let service_report = service_applied.then(|| rpc::ServiceVpcNetworkStatusObservation {
            network_config_version: if response.instance_network_config_version.is_empty() {
                response
                    .instance
                    .as_ref()
                    .map(|instance| instance.network_config_version.clone())
            } else {
                Some(response.instance_network_config_version.clone())
            },
            interfaces: response
                .service_interfaces
                .iter()
                .map(|interface| rpc::ServiceVpcInterfaceStatusObservation {
                    internal_uuid: interface.internal_uuid.clone(),
                    network_security_group: None,
                })
                .collect(),
        });
        self.env
            .api
            .record_dpu_network_status(Request::new(rpc::DpuNetworkStatus {
                dpu_machine_id: Some(dpu_id),
                network_config_version: Some(response.managed_host_config_version.clone()),
                instance_network_config_version: (!response
                    .instance_network_config_version
                    .is_empty())
                .then(|| response.instance_network_config_version.clone()),
                service_vpc_network_observation: service_report,
                dpu_health: Some(::rpc::health::HealthReport {
                    source: health_report::HealthReport::DPU_AGENT_SOURCE.to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await?;
        Ok(response)
    }

    /// Advances the separately tested host setup to the extension-service wait in a fresh Tenant
    /// generation so service tests can isolate the primary Admin-to-Tenant delivery requirement.
    async fn request_tenant_generation(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = self.env.db_txn().await;
        let host = self.host.host().db_machine(&mut txn).await;
        let mut network = host.network_config.value.clone();
        network.use_admin_network = Some(false);
        assert!(matches!(
            db::machine::try_update_network_config(
                txn.as_mut(),
                &host.id,
                host.network_config.version,
                &network,
            )
            .await?,
            db::ConditionalWrite::Applied(_)
        ));
        db::machine::advance(
            &host,
            &mut txn,
            &ManagedHostState::Assigned {
                instance_state: InstanceState::WaitingForExtensionServicesConfig,
            },
            None,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// Completes the separately tested host boot prerequisite so public attachment updates can run.
    /// Callers must first verify service convergence; this helper does not apply or acknowledge networking.
    async fn finish_host_boot(&self) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = self.env.db_txn().await;
        let host = self.host.host().db_machine(&mut txn).await;
        db::machine::advance(
            &host,
            &mut txn,
            &ManagedHostState::Assigned {
                instance_state: InstanceState::Ready,
            },
            None,
        )
        .await?;
        txn.commit().await?;
        Ok(())
    }

    /// Updates the public attachment list while retaining the original host configuration so
    /// detach and rebind tests exercise the same immutable attachment admission as clients.
    async fn select(
        &self,
        selections: &[(usize, VpcId)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let current = self.env.one_instance(self.instance_id).await.into_inner();
        let mut config = current.config.expect("public Instance configuration");
        config.dpu_extension_services = Some(rpc::InstanceDpuExtensionServicesConfig {
            service_configs: selections
                .iter()
                .map(|(index, vpc_id)| {
                    let service = &self.services[*index];
                    rpc::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id.clone(),
                        version: service
                            .latest_version_info
                            .as_ref()
                            .expect("service version")
                            .version
                            .clone(),
                        // Select the Service VPC used by this detach or rebind operation.
                        service_vpc_ids: vec![*vpc_id],
                    }
                })
                .collect(),
        });
        self.env
            .api
            .update_instance_config(Request::new(rpc::InstanceConfigUpdateRequest {
                instance_id: Some(self.instance_id),
                if_version_match: None,
                config: Some(config),
                metadata: current.metadata,
            }))
            .await?;
        // Find after update and verify the active selection before the next controller pass.
        let persisted = self.env.one_instance(self.instance_id).await.into_inner();
        let active = persisted
            .config
            .expect("persisted configuration")
            .dpu_extension_services
            .unwrap_or_default()
            .service_configs;
        assert_eq!(active.len(), selections.len());
        for (actual, (index, vpc_id)) in active.iter().zip(selections) {
            assert_eq!(actual.service_id, self.services[*index].service_id);
            assert_eq!(actual.service_vpc_ids, vec![*vpc_id]);
        }
        Ok(())
    }

    /// Reads public service status after reconciliation so tests include model aggregation and RPC
    /// conversion rather than relying only on the controller's internal evidence representation.
    async fn public_states(&self) -> Vec<i32> {
        self.env
            .one_instance(self.instance_id)
            .await
            .into_inner()
            .status
            .expect("Instance status")
            .dpu_extension_services
            .expect("service statuses")
            .dpu_extension_services
            .into_iter()
            .map(|service| service.deployment_status)
            .collect()
    }
}

/// Retains both DPU allocations through failed identity preparation, missing or mismatched service receipts,
/// a retried placement timeout, and primary Admin delivery, then permits force-delete only after current
/// network and chain isolation. Verifies committed Node and selector evidence before the first activation
/// so recovery can identify the activated Node.
/// Saved reprovision steps and started requests must withhold Running on their DPU without invalidating its sibling.
/// Both reporting DPUs remain counted while force deletion waits and after cleanup advances the configuration.
#[crate::sqlx_test]
async fn two_dpu_convergence_retries_and_default_force_delete_waits_for_observed_cleanup(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Allocate on both DPUs while the primary still receives its initial Admin configuration.
    // Heap-allocate fixture creation to leave stack space for nested host-discovery Machine reads.
    let fixture = Box::pin(LifecycleFixture::new(pool, 2, 1, false)).await?;
    // Poll the long lifecycle checks after host discovery so their stack frames do not nest.
    Box::pin(two_dpu_convergence_and_cleanup(fixture)).await
}

/// Runs two-DPU convergence and cleanup after fixture creation so host discovery can finish before
/// the large test future is polled.
/// Checks DPU-up counts during pending cleanup and its configuration reload so retained DPUs stay observable.
async fn two_dpu_convergence_and_cleanup(
    fixture: LifecycleFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read the allocated endpoints and DPU roles before checking initial Admin delivery.
    let initial = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("allocated Instance");
    let endpoints = initial.config.network.service_interfaces.clone();
    assert_eq!(endpoints.len(), 2);
    let mut txn = fixture.env.db_txn().await;
    let primary = fixture
        .host
        .host()
        .db_machine(&mut txn)
        .await
        .primary_attached_dpu_machine_id()
        .expect("primary DPU");
    txn.rollback().await?;
    let secondary = fixture
        .host
        .dpu_ids
        .iter()
        .copied()
        .find(|dpu| *dpu != primary)
        .expect("secondary DPU");
    let primary_admin = fixture.acknowledge(primary, true).await?;
    let secondary_admin = fixture.acknowledge(secondary, true).await?;
    assert!(primary_admin.use_admin_network && primary_admin.service_interfaces.is_empty());
    assert!(secondary_admin.use_admin_network && !secondary_admin.service_interfaces.is_empty());

    // Start without retained evidence so the first activation cannot reuse a previous commit.
    assert!(fixture.observations().await?.is_empty());
    // Observe activation from another connection so uncommitted Node and selector writes remain invisible.
    let connect_options = fixture
        .env
        .pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("statement_timeout", "5s")]);
    let secondary_endpoint = endpoints
        .iter()
        .find(|endpoint| endpoint.dpu_id == secondary)
        .expect("secondary endpoint")
        .clone();
    {
        let mut state = fixture.state.lock().expect("DPF state");
        state.fail_next_identity_preparation = Some(primary);
        state.before_next_active_placement = Some(Box::new(move |dpu_id, node| {
            assert_eq!(dpu_id, secondary);
            // Read independently while the synchronous placement mock holds the controller before publication.
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("activation observer runtime")
                    .block_on(async move {
                        let mut connection = sqlx::PgConnection::connect_with(&connect_options)
                            .await
                            .expect("independent activation observer");
                        let dpu =
                            db::machine::find_one(&mut connection, &dpu_id, Default::default())
                                .await
                                .expect("read committed activation")
                                .expect("secondary DPU");
                        let observation = dpu
                            .status
                            .extension_service_status_observations
                            .for_service_type(ExtensionServiceType::DpfHelmChart)
                            .expect("committed activation evidence");
                        // Require the captured endpoint and Node to be committed before activating labels.
                        assert_eq!(observation.service_vpc_endpoints.len(), 1);
                        let activation = &observation.service_vpc_endpoints[0];
                        assert_eq!(activation.endpoint, secondary_endpoint);
                        assert_eq!(activation.node.as_ref(), Some(&node));
                        assert_eq!(activation.context.intent, ServiceVpcChainIntent::Active);

                        // Require the same committed selector version to allow activation and exclude the failed sibling.
                        let selector = db::extension_service::find_chain_selectors(
                            &mut connection,
                            activation.context.service_id,
                        )
                        .await
                        .expect("read committed selector")
                        .into_iter()
                        .find(|selector| {
                            selector.interface_ordinal == activation.context.interface_ordinal
                                && selector.slot == activation.context.slot_index
                        })
                        .expect("committed activation selector");
                        assert_eq!(selector.version, activation.context.selector_version);
                        assert!(!selector.excluded_dpu_ids.contains(&dpu_id));
                        assert!(selector.excluded_dpu_ids.contains(&primary));
                    });
            })
            .join()
            .expect("joined activation observer");
        }));
    }

    // Fail primary identity preparation independently of Node resolution and later placement failures.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Reload the failure to prove preparation ran without authorizing placement or releasing capacity.
    fixture.allocations(&endpoints).await?;
    let initial_evidence = fixture.observations().await?;
    let preparation_failure = initial_evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("persisted preparation failure");
    assert!(preparation_failure.node.is_none());
    assert!(!preparation_failure.placement_observed);
    assert!(
        preparation_failure
            .message
            .contains("injected transient identity preparation failure")
    );
    {
        let state = fixture.state.lock().expect("DPF state");
        // Prove both the injected error and the independent first-activation observation actually ran.
        assert!(state.fail_next_identity_preparation.is_none());
        assert!(state.before_next_active_placement.is_none());
        assert!(!state.successful_placements.contains_key(&primary));
        assert!(state.successful_placements.contains_key(&secondary));
    }

    // Refresh the sibling receipt after ordinary host handling can advance its network generation.
    fixture.acknowledge(secondary, true).await?;

    // Withhold one initial Node while the later DPU still captures current evidence independently.
    {
        let mut state = fixture.state.lock().expect("DPF state");
        state.omit_node_once = Some(primary);
    }
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), 2);
    let primary_evidence = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("primary evidence");
    let secondary_evidence = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == secondary)
        .expect("secondary evidence");
    assert!(primary_evidence.node.is_none());
    // Consuming this separate Node fault proves identity preparation retried before resolution.
    assert!(
        fixture
            .state
            .lock()
            .expect("DPF state")
            .omit_node_once
            .is_none()
    );
    assert!(secondary_evidence.node.is_some());
    assert!(!primary_evidence.placement_observed && !primary_evidence.network_acknowledged);
    assert!(secondary_evidence.placement_observed && secondary_evidence.network_acknowledged);
    assert!(
        fixture
            .state
            .lock()
            .expect("DPF state")
            .successful_placements
            .contains_key(&secondary)
    );
    assert_ne!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // Let Assigned publish Tenant networking instead of constructing the transition in the fixture.
    fixture
        .env
        .run_machine_state_controller_iteration_until_state_matches(
            &fixture.host.id,
            20,
            ManagedHostState::Assigned {
                instance_state: InstanceState::WaitingForNetworkConfig,
            },
        )
        .await;
    let mut txn = fixture.env.db_txn().await;
    assert!(!fixture.host.snapshot(&mut txn).await.use_admin_network());
    txn.rollback().await?;

    // Withhold only the primary's nested report while acknowledging current host and Instance generations.
    let missing_service_report = fixture.acknowledge(primary, false).await?;
    fixture.acknowledge(secondary, true).await?;
    // Reload the receipt to prove nested absence is the only withheld network acknowledgement.
    let primary_receipt = db::machine::find_one(
        &fixture.env.pool,
        &primary,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("primary DPU")
    .network_status_observation
    .expect("persisted current outer acknowledgements");
    assert_eq!(
        primary_receipt
            .network_config_version
            .expect("host acknowledgement")
            .version_string(),
        missing_service_report.managed_host_config_version
    );
    assert_eq!(
        primary_receipt
            .instance_network_observation
            .expect("Instance acknowledgement")
            .config_version
            .version_string(),
        missing_service_report.instance_network_config_version
    );
    assert!(primary_receipt.service_vpc_network_observation.is_none());
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    let primary_evidence = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("primary retry evidence");
    assert!(primary_evidence.node.is_some());
    assert!(primary_evidence.placement_observed && primary_evidence.chain_converged);
    assert!(!primary_evidence.network_acknowledged);
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );

    // A fresh Tenant response must deliver the primary endpoint before its acknowledgement can run it.
    let primary_tenant = fixture.acknowledge(primary, true).await?;
    assert!(!primary_tenant.use_admin_network);
    assert_eq!(primary_tenant.service_interfaces.len(), 1);
    assert_ne!(
        primary_tenant.managed_host_config_version,
        primary_admin.managed_host_config_version
    );
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // A newer missing nested service report must invalidate Running despite current outer acknowledgements.
    fixture.acknowledge(primary, false).await?;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );
    // Inspect the initial refresh commit before its next chain result can replace retained DPF facts.
    let connect_options = fixture
        .env
        .pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("statement_timeout", "5s")]);
    fixture
        .state
        .lock()
        .expect("DPF state")
        .before_next_chain_result = Some(Box::new(move || {
        // Use a separate runtime because the ready mock cannot await this independent persisted-state read.
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("refresh observer runtime")
                .block_on(async move {
                    let mut connection = sqlx::PgConnection::connect_with(&connect_options)
                        .await
                        .expect("independent refresh observer");
                    let dpu = db::machine::find_one(&mut connection, &primary, Default::default())
                        .await
                        .expect("read committed refresh")
                        .expect("primary DPU");
                    let refresh = dpu
                        .status
                        .extension_service_status_observations
                        .for_service_type(ExtensionServiceType::DpfHelmChart)
                        .expect("persisted refresh");
                    assert_eq!(refresh.service_vpc_endpoints.len(), 1);
                    let pending_network = &refresh.service_vpc_endpoints[0];
                    assert!(pending_network.placement_observed && pending_network.chain_converged);
                    assert!(!pending_network.network_acknowledged);
                });
        })
        .join()
        .expect("joined refresh observer");
    }));
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert!(
        fixture
            .state
            .lock()
            .expect("DPF state")
            .before_next_chain_result
            .is_none()
    );
    fixture.allocations(&endpoints).await?;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );
    fixture.acknowledge(primary, true).await?;

    // Inject a placement timeout after Node retention and valid network evidence to isolate the retry contract.
    let retained_node = fixture
        .observations()
        .await?
        .into_iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("converged primary evidence")
        .node
        .expect("retained primary Node");
    let successful_before_failure = {
        let mut state = fixture.state.lock().expect("DPF state");
        state.fail_next_placement = Some(primary);
        state.successful_placements.clone()
    };
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // Reload the failed endpoint to prove the injected operation ran without losing its retained Node or capacity.
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    let primary_failure = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("persisted placement failure");
    assert_eq!(primary_failure.node.as_ref(), Some(&retained_node));
    assert!(!primary_failure.placement_observed && primary_failure.network_acknowledged);
    assert!(
        primary_failure
            .message
            .contains("injected transient DPF failure")
    );
    {
        let state = fixture.state.lock().expect("DPF state");
        assert!(state.fail_next_placement.is_none());
        assert_eq!(
            state.successful_placements[&primary],
            successful_before_failure[&primary]
        );
        assert!(state.successful_placements[&secondary] > successful_before_failure[&secondary]);
    }
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );

    // Retry the same accepted endpoint after the one-shot error has been consumed.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let recovered = fixture.observations().await?;
    let primary_recovery = recovered
        .iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("persisted placement recovery");
    assert_eq!(primary_recovery.node.as_ref(), Some(&retained_node));
    assert!(primary_recovery.placement_observed && primary_recovery.network_acknowledged);
    assert!(
        fixture
            .state
            .lock()
            .expect("DPF state")
            .successful_placements[&primary]
            > successful_before_failure[&primary]
    );
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // Report the current generation with the other DPU's UUID so a valid token cannot acknowledge local membership.
    let wrong_local_uuid = endpoints
        .iter()
        .find(|endpoint| endpoint.dpu_id == secondary)
        .expect("secondary endpoint")
        .internal_uuid;
    fixture
        .env
        .api
        .record_dpu_network_status(Request::new(rpc::DpuNetworkStatus {
            dpu_machine_id: Some(primary),
            network_config_version: Some(primary_tenant.managed_host_config_version.clone()),
            instance_network_config_version: Some(
                primary_tenant.instance_network_config_version.clone(),
            ),
            service_vpc_network_observation: Some(rpc::ServiceVpcNetworkStatusObservation {
                network_config_version: Some(
                    primary_tenant.instance_network_config_version.clone(),
                ),
                interfaces: vec![rpc::ServiceVpcInterfaceStatusObservation {
                    internal_uuid: Some(wrong_local_uuid.into()), // Keep a real endpoint UUID on the wrong DPU.
                    network_security_group: None,
                }],
            }),
            dpu_health: Some(::rpc::health::HealthReport {
                source: health_report::HealthReport::DPU_AGENT_SOURCE.to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // Persisted chain and placement success must retain capacity and Pending until the local UUID set matches.
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    let wrong_membership = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("wrong-membership evidence");
    assert!(wrong_membership.placement_observed && wrong_membership.chain_converged);
    assert!(!wrong_membership.network_acknowledged);
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );
    fixture.acknowledge(primary, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // Enter primary-only DPF reprovisioning without changing the host generation or stored DPF evidence.
    let mut txn = fixture.env.db_txn().await;
    let host = fixture.host.host().db_machine(&mut txn).await;
    let state_before_reprovision = host.state.value.clone();
    let host_generation = host.network_config.version;
    let reprovision_state = |primary_state| ManagedHostState::Assigned {
        instance_state: InstanceState::DPUReprovision {
            dpu_states: model::machine::DpuReprovisionStates {
                states: HashMap::from([
                    (primary, primary_state),
                    (
                        secondary,
                        model::machine::ReprovisionState::NotUnderReprovision,
                    ),
                ]),
            },
        },
    };
    db::machine::advance(
        &host,
        &mut txn,
        &reprovision_state(model::machine::ReprovisionState::DpfStates {
            substate: model::machine::DpfState::WaitingForReady { phase_detail: None },
        }),
        None,
    )
    .await?;
    txn.commit().await?;

    // Public aggregation must invalidate the primary before another controller iteration can refresh it.
    fixture.allocations(&endpoints).await?;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );
    let mut txn = fixture.env.db_txn().await;
    let reprovisioning = fixture.host.snapshot(&mut txn).await;
    assert_eq!(
        reprovisioning.host_snapshot.network_config.version,
        host_generation
    );
    assert_eq!(
        reprovisioning.instance.expect("retained Instance").id,
        fixture.instance_id
    );
    txn.rollback().await?;
    let dpu_statuses = fixture
        .env
        .one_instance(fixture.instance_id)
        .await
        .into_inner()
        .status
        .expect("public status")
        .dpu_extension_services
        .expect("extension-service status")
        .dpu_extension_services
        .pop()
        .expect("service status")
        .dpu_statuses;
    for (dpu_id, expected) in [
        // A saved DPF reprovision step invalidates only this DPU's active receipt.
        (
            primary,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending,
        ),
        // The sibling retains its current placement, chain and network evidence.
        (
            secondary,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning,
        ),
    ] {
        assert_eq!(
            dpu_statuses
                .iter()
                .find(|status| status.dpu_machine_id == Some(dpu_id))
                .expect("per-DPU status")
                .status,
            expected as i32
        );
    }

    // Park per-DPU state as DPF migration does before deletion while retaining the primary's
    // started request.
    let mut txn = fixture.env.db_txn().await;
    let host = fixture.host.host().db_machine(&mut txn).await;
    db::machine::trigger_dpu_reprovisioning_request(&primary, txn.as_mut(), "test", true).await?;
    db::machine::update_dpu_reprovision_start_time(&primary, txn.as_mut()).await?;
    db::machine::advance(
        &host,
        &mut txn,
        &reprovision_state(model::machine::ReprovisionState::NotUnderReprovision),
        None,
    )
    .await?;
    txn.commit().await?;
    // A started reprovision request must keep the primary Pending while its per-DPU state is parked.
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );
    fixture.allocations(&endpoints).await?;

    // Reload the parked state to verify the host generation and started request remain persisted.
    let mut txn = fixture.env.db_txn().await;
    let parked = fixture.host.snapshot(&mut txn).await;
    assert_eq!(parked.host_snapshot.network_config.version, host_generation);
    assert!(
        parked
            .dpu_snapshots
            .iter()
            .find(|dpu| dpu.id == primary)
            .expect("parked primary")
            .reprovision_requested
            .as_ref()
            .expect("started request")
            .started_at
            .is_some()
    );
    txn.rollback().await?;

    // Complete reprovisioning by clearing the started request and restoring Assigned before reusing evidence.
    let mut txn = fixture.env.db_txn().await;
    let host = fixture.host.host().db_machine(&mut txn).await;
    assert_eq!(host.network_config.version, host_generation);
    db::machine::clear_dpu_reprovisioning_request(txn.as_mut(), &primary, false).await?;
    db::machine::advance(&host, &mut txn, &state_before_reprovision, None).await?;
    txn.commit().await?;
    fixture.allocations(&endpoints).await?;
    let retained = fixture.observations().await?;
    assert_eq!(
        retained
            .iter()
            .find(|observed| observed.endpoint.dpu_id == primary)
            .expect("primary evidence")
            .node
            .as_ref(),
        Some(&retained_node)
    );
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // A shared selector edit and restoration preserve its spec but invalidate earlier selector evidence.
    let selector_context = fixture
        .observations()
        .await?
        .into_iter()
        .next()
        .expect("current endpoint context")
        .context;
    let mut txn = fixture.env.db_txn().await;
    db::extension_service::find_by_ids(txn.as_mut(), &[selector_context.service_id], false, true)
        .await?
        .pop()
        .expect("lock registered service");
    // Reuse each returned selector token because its timestamp is assigned when the write runs.
    let mut selector_version = selector_context.selector_version;
    for change in [
        // Change this DPU's selector membership to advance the desired revision.
        db::extension_service::ServiceVpcChainExclusion::Exclude,
        // Restore the same membership at a later revision so the prior typed observation stays stale.
        db::extension_service::ServiceVpcChainExclusion::Allow,
    ] {
        let db::ConditionalWrite::Applied(selector) =
            db::extension_service::try_update_chain_exclusion(
                txn.as_mut(),
                selector_context.service_id,
                selector_context.interface_ordinal,
                selector_context.slot_index,
                selector_context.dpu_id,
                Some(selector_version),
                change,
            )
            .await?
        else {
            panic!("selector membership update must apply");
        };
        selector_version = selector.version;
    }
    txn.commit().await?;
    // Restored membership must not make the earlier selector observation current.
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending as i32]
    );
    // Refresh the selector observation before checking Running again.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Current selector evidence restores Running without another network acknowledgement.
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // Initiate whole-instance removal through force-delete without any tenant configuration update.
    let service_id = fixture.services[0].service_id.parse()?;
    {
        let mut state = fixture.state.lock().expect("DPF state");
        state.unacknowledged_exclusions.insert(service_id);
        state.remaining_chains.insert(service_id);
    }
    let pending = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            fixture.host.id,
            false,
        )))
        .await?
        .into_inner();
    assert!(!pending.all_done);
    let terminating = fixture.allocations(&endpoints).await?;
    assert!(terminating.deleted.is_some());
    assert!(
        terminating
            .config
            .extension_services
            .service_configs
            .iter()
            .all(|config| config.removed.is_some())
    );
    assert_eq!(
        terminating.extension_services_config_version.version_nr(),
        initial.extension_services_config_version.version_nr() + 1
    );
    assert_eq!(
        terminating.network_config_version.version_nr(),
        initial.network_config_version.version_nr() + 1
    );

    // Neither an old network report nor an unacknowledged exclusion permits the cleanup transaction.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(
        evidence
            .iter()
            .all(|observed| !observed.network_acknowledged && !observed.chain_isolated)
    );
    for dpu_id in &fixture.host.dpu_ids {
        fixture.acknowledge(*dpu_id, true).await?;
    }
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Keep both reporting DPUs counted while force-delete polling retains the Instance.
    assert_eq!(
        fixture
            .env
            .test_meter
            .formatted_metric(r#"carbide_dpus_up_count{fresh="true"}"#)
            .as_deref(),
        Some("2")
    );
    let waiting = fixture.allocations(&endpoints).await?;
    assert_eq!(
        waiting.extension_services_config_version,
        terminating.extension_services_config_version
    );
    assert_eq!(
        waiting.network_config_version,
        terminating.network_config_version
    );
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(
        evidence
            .iter()
            .all(|observed| observed.network_acknowledged && !observed.chain_isolated)
    );

    // Administrative polling must preserve removal generations and the first removal timestamp.
    let polled = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            fixture.host.id,
            false,
        )))
        .await?
        .into_inner();
    assert!(!polled.all_done);
    let repolled = fixture.allocations(&endpoints).await?;
    assert_eq!(
        repolled.extension_services_config_version,
        terminating.extension_services_config_version
    );
    assert_eq!(
        repolled.network_config_version,
        terminating.network_config_version
    );
    assert_eq!(
        repolled.config.extension_services,
        terminating.config.extension_services
    );
    assert_eq!(repolled.deleted, terminating.deleted);

    // An acknowledged exclusion still cannot reclaim while the target chain remains on either DPU.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .unacknowledged_exclusions
        .remove(&service_id);
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let exclusions =
        db::extension_service::find_chain_selectors(&fixture.env.pool, service_id).await?;
    assert!(exclusions.iter().any(|selector| {
        fixture
            .host
            .dpu_ids
            .iter()
            .all(|dpu| selector.excluded_dpu_ids.contains(dpu))
    }));

    // Isolating one DPU must not release the endpoint still connected on the other DPU.
    {
        let mut state = fixture.state.lock().expect("DPF state");
        state.remaining_chains.remove(&service_id);
        state
            .remaining_chains_on_dpus
            .insert((service_id, secondary));
    }
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(
        evidence
            .iter()
            .find(|observed| observed.endpoint.dpu_id == primary)
            .expect("isolated primary")
            .chain_isolated
    );
    assert!(
        !evidence
            .iter()
            .find(|observed| observed.endpoint.dpu_id == secondary)
            .expect("connected secondary")
            .chain_isolated
    );

    // Fresh isolation on both retained Nodes permits one atomic allocation and tombstone cleanup.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains_on_dpus
        .remove(&(service_id, secondary));
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Keep both reporting DPUs counted when reclamation requires a fresh configuration snapshot.
    assert_eq!(
        fixture
            .env
            .test_meter
            .formatted_metric(r#"carbide_dpus_up_count{fresh="true"}"#)
            .as_deref(),
        Some("2")
    );
    let reclaimed = fixture.allocations(&[]).await?;
    assert!(
        reclaimed
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    assert_eq!(
        reclaimed.extension_services_config_version.version_nr(),
        terminating.extension_services_config_version.version_nr() + 1
    );
    assert_eq!(
        reclaimed.network_config_version.version_nr(),
        terminating.network_config_version.version_nr() + 1
    );
    for endpoint in &endpoints {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *fixture.env.pool.acquire().await?,
                &fixture.instance_id,
                &endpoint.network_segment_id,
            )
            .await?
            .is_empty()
        );
    }
    assert_eq!(
        db::extension_service::find_chain_selectors(&fixture.env.pool, service_id).await?,
        exclusions
    );

    // Background iterations must retain the original Instance until the administrative poll.
    for _ in 0..2 {
        fixture
            .env
            .run_machine_state_controller_iteration_no_requeue()
            .await;
        assert_eq!(fixture.allocations(&[]).await?.id, fixture.instance_id);
    }

    // The next administrative poll may enter terminal ForceDeletion and remove the retained Instance.
    let completed = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            fixture.host.id,
            false,
        )))
        .await?
        .into_inner();
    assert!(completed.all_done);
    assert!(
        db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await?
            .is_none()
    );
    Ok(())
}

/// Reclaims simultaneously retired attachments in separate configuration generations so one
/// removal receipt cannot release its sibling's endpoint address after the first version advance.
#[crate::sqlx_test]
async fn simultaneous_service_retirement_requires_each_reclamation_generation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Admit two services in one DPU/VPC slot; the remaining fixture fields provide valid host networking.
    let fixture = LifecycleFixture::new(pool, 1, 2, true).await?;
    let dpu_id = fixture.host.dpu_ids[0];
    // Converge both services before checking public attachment-removal behavior.
    fixture.request_tenant_generation().await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32; 2]
    );
    // Complete the separately tested boot prerequisite so public detachment is admitted.
    fixture.finish_host_boot().await?;
    let initial = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("Instance");
    let endpoints = initial.config.network.service_interfaces;
    assert_eq!(endpoints.len(), 2);

    // Retire both attachments together before delivering their common removal generation.
    fixture.select(&[]).await?;
    let removing = fixture.allocations(&endpoints).await?;
    let generations = [
        // The first acknowledged generation must leave the other eligible attachment allocated.
        (1, 1),
        // A new receipt after the first version advance permits the final attachment's reclamation.
        (0, 2),
    ];
    for (remaining_count, reclaimed_count) in generations {
        // Deliver the currently requested generation before allowing one cleanup pass.
        fixture.acknowledge(dpu_id, true).await?;
        fixture
            .env
            .run_machine_state_controller_iteration_no_requeue()
            .await;
        let reclaimed = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await?
            .expect("Instance remains after attachment cleanup");
        let retained = &reclaimed.config.network.service_interfaces;

        // Reload retained normalized addresses and require exactly one increment for each reclaimed attachment.
        assert_eq!(retained.len(), remaining_count);
        fixture.allocations(retained).await?;
        assert_eq!(
            reclaimed.config.extension_services.service_configs.len(),
            remaining_count
        );
        assert_eq!(
            reclaimed.extension_services_config_version.version_nr(),
            removing.extension_services_config_version.version_nr() + reclaimed_count
        );
        assert_eq!(
            reclaimed.network_config_version.version_nr(),
            removing.network_config_version.version_nr() + reclaimed_count
        );
        // Query removed endpoint addresses directly so configuration cleanup cannot hide a leaked allocation.
        for endpoint in endpoints
            .iter()
            .filter(|endpoint| !retained.contains(endpoint))
        {
            assert!(
                db::instance_address::find_all_by_instance_id_and_segment_id(
                    &mut *fixture.env.pool.acquire().await?,
                    &fixture.instance_id,
                    &endpoint.network_segment_id,
                )
                .await?
                .is_empty()
            );
        }
        // Both attachments were eligible under the first receipt, so the first reclamation must leave stale sibling evidence.
        if remaining_count == 1 {
            let first_evidence = fixture.observations().await?;
            assert_eq!(first_evidence.len(), 2);
            assert!(first_evidence.iter().all(|evidence| {
                evidence.deployment_status() == ExtensionServiceDeploymentStatus::Terminated
                    && evidence.context.extension_services_config_version
                        == removing.extension_services_config_version
                    && evidence.context.network_config_version == removing.network_config_version
            }));

            // Refresh chain isolation without acknowledging the version advanced by the first reclamation.
            fixture
                .env
                .run_machine_state_controller_iteration_no_requeue()
                .await;
            let awaiting_receipt = fixture.allocations(retained).await?;

            // The sibling and its actual address remain stored while fresh chain evidence awaits the new receipt.
            assert_eq!(
                awaiting_receipt.config.extension_services,
                reclaimed.config.extension_services
            );
            assert_eq!(
                awaiting_receipt.extension_services_config_version,
                reclaimed.extension_services_config_version
            );
            assert_eq!(
                awaiting_receipt.network_config_version,
                reclaimed.network_config_version
            );
            let fresh_evidence = fixture.observations().await?;
            assert_eq!(fresh_evidence.len(), 1);
            assert!(fresh_evidence[0].chain_isolated && fresh_evidence[0].placement_observed);
            assert!(!fresh_evidence[0].network_acknowledged);
            assert_eq!(
                fresh_evidence[0].context.network_config_version,
                reclaimed.network_config_version
            );
        }
    }
    Ok(())
}

/// Preserves a shared-slot survivor across disabled overlap and IPv4 admission and a changed Node UID,
/// then reuses the slot after isolation while rejecting stale writes, chain context and unstamped withdrawal.
/// Missing network acknowledgement must hold Assigned at its extension-service wait until both services run.
/// A prior withdrawal context must not reclaim the replacement's allocations during removal.
#[crate::sqlx_test]
async fn shared_slot_cleanup_survives_restart_and_rebinds_only_after_isolation(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Accept two independent services in the host VPC while overlap admission is enabled.
    // Heap-allocate fixture creation to leave stack space for nested host-discovery Machine reads.
    let fixture = Box::pin(LifecycleFixture::new(pool, 1, 2, true)).await?;
    // Heap-allocate the checks after host discovery so their large stack frames do not nest.
    Box::pin(shared_slot_cleanup(fixture)).await
}

/// Runs shared-slot removal and rebind in a separate future so the test can finish host discovery
/// first and allocate these checks on the heap.
async fn shared_slot_cleanup(
    mut fixture: LifecycleFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    // Request Tenant networking before checking the persisted extension-service wait.
    fixture.request_tenant_generation().await?;
    let dpu_id = fixture.host.dpu_ids[0];

    // Reconcile before the DPU acknowledges Tenant networking to exercise the extension-service wait.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let mut txn = fixture.env.db_txn().await;
    let waiting = fixture.host.host().db_machine(&mut txn).await;
    assert_eq!(
        waiting.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForExtensionServicesConfig,
        }
    );
    assert!(matches!(
        &waiting.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason == "Waiting for all active extension services to be running on all DPUs."
    ));
    txn.rollback().await?;

    // Acknowledge the requested generation so the next pass can advance past the service wait.
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let initial = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("shared Instance");
    let endpoints = initial.config.network.service_interfaces.clone();
    assert_eq!(endpoints.len(), 2);
    assert!(
        endpoints
            .iter()
            .all(|endpoint| endpoint.slot_index == 0 && endpoint.vpc_id == fixture.service_vpc_id)
    );
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32; 2]
    );
    // Reload the host to prove service convergence advances the persisted Assigned state before boot.
    let mut txn = fixture.env.db_txn().await;
    assert_eq!(
        fixture
            .host
            .host()
            .db_machine(&mut txn)
            .await
            .current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForRebootToReady,
        }
    );
    txn.rollback().await?;

    // Complete the separately tested host boot prerequisite before public attachment edits.
    fixture.finish_host_boot().await?;
    let first_id = fixture.services[0].service_id.parse()?;
    let second_id = fixture.services[1].service_id.parse()?;
    let first_attachment = initial
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|config| config.service_id == first_id)
        .expect("first attachment")
        .id
        .expect("attachment ID");
    let survivor = endpoints
        .iter()
        .find(|endpoint| endpoint.attachment_id != first_attachment)
        .expect("survivor")
        .clone();
    let initial_evidence = fixture.observations().await?;
    let initial_active_context = initial_evidence
        .iter()
        .find(|observed| observed.context.service_id == first_id)
        .expect("first active context")
        .context
        .clone();
    let shared_loopback = db::vpc_dpu_loopback::find(
        &mut *fixture.env.pool.acquire().await?,
        &dpu_id,
        &fixture.service_vpc_id,
    )
    .await?
    .expect("host and services share a loopback")
    .loopback_ip;
    let original_node = initial_evidence[0].node.clone().expect("retained Node");

    // Capture an active refresh from the controller and defer the simulated server's completion.
    let (release_old_write, delayed_write) = tokio::sync::oneshot::channel();
    let (write_completed, delayed_completion) = tokio::sync::oneshot::channel();
    fixture
        .state
        .lock()
        .expect("DPF state")
        .delay_next_placement = Some((delayed_write, write_completed));
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    assert!(
        fixture
            .state
            .lock()
            .expect("DPF state")
            .delay_next_placement
            .is_none()
    );

    // Withdraw only the first service and keep the pre-acknowledgement chain result incomplete.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .unacknowledged_exclusions
        .insert(first_id);
    fixture.select(&[(1, fixture.service_vpc_id)]).await?;
    let removing_first = fixture.allocations(&endpoints).await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(
        evidence
            .iter()
            .any(|observed| observed.context.service_id == first_id
                && observed.context.intent == ServiceVpcChainIntent::Retiring
                && !observed.chain_isolated)
    );
    // Save the first withdrawal's context before the same service later reuses this slot.
    let old_withdrawal_context = evidence
        .iter()
        .find(|observed| observed.context.service_id == first_id)
        .expect("first withdrawal context")
        .context
        .clone();
    let first_exclusions =
        db::extension_service::find_chain_selectors(&fixture.env.pool, first_id).await?;
    assert!(
        first_exclusions
            .iter()
            .any(|selector| selector.excluded_dpu_ids.contains(&dpu_id))
    );

    // Restart Core with overlap and IPv4 admission disabled and a replacement Node mapping; neither
    // may discard accepted allocations or replace the original Node baseline retained in PostgreSQL.
    let pool = fixture.env.pool.clone();
    fixture.env = lifecycle_env(pool, fixture.state.clone(), false, false, false).await;
    fixture.host.api = fixture.env.api.clone();
    // Clear the exclusion fault before replacing the Node to isolate the retained Node UID requirement.
    {
        let mut state = fixture.state.lock().expect("DPF state");
        state.unacknowledged_exclusions.remove(&first_id);
        state.nodes.get_mut(&dpu_id).expect("known Node").uid = "replacement-node-uid".to_owned();
    }
    // Keep networking current so the replacement Node identity independently withholds reclamation.
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(
        evidence
            .iter()
            .all(|observed| observed.node.as_ref() == Some(&original_node))
    );
    assert!(evidence.iter().all(|observed| !observed.chain_isolated));
    let restarted_statuses = fixture
        .env
        .one_instance(fixture.instance_id)
        .await
        .into_inner()
        .status
        .expect("public status")
        .dpu_extension_services
        .expect("service statuses")
        .dpu_extension_services;
    for (service_id, expected) in [
        // The removed attachment still needs isolation on its retained Node.
        (
            first_id,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating,
        ),
        // The surviving attachment cannot use the replacement Node as current active evidence.
        (
            second_id,
            rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServicePending,
        ),
    ] {
        assert_eq!(
            restarted_statuses
                .iter()
                .find(|status| status.service_id == service_id.to_string())
                .expect("retained service status")
                .deployment_status,
            expected as i32
        );
    }

    // Restore reachability to the original Node and isolate the first chain while its slot member survives.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .nodes
        .insert(dpu_id, original_node.clone());
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let first_reclaimed = fixture.allocations(std::slice::from_ref(&survivor)).await?;
    for retired in endpoints
        .iter()
        .filter(|endpoint| endpoint.attachment_id == first_attachment)
    {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *fixture.env.pool.acquire().await?,
                &fixture.instance_id,
                &retired.network_segment_id,
            )
            .await?
            .is_empty()
        );
    }
    assert_eq!(
        first_reclaimed
            .extension_services_config_version
            .version_nr(),
        removing_first
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        first_reclaimed.network_config_version.version_nr(),
        removing_first.network_config_version.version_nr() + 1
    );
    assert!(
        first_reclaimed
            .config
            .extension_services
            .service_configs
            .iter()
            .all(|config| config.service_id == second_id)
    );
    assert_eq!(
        db::extension_service::find_chain_selectors(&fixture.env.pool, first_id).await?,
        first_exclusions
    );
    let labels = fixture
        .state
        .lock()
        .expect("DPF state")
        .devices
        .values()
        .next()
        .expect("device")
        .1
        .node_labels
        .clone();
    assert_eq!(
        labels
            .get(
                &model::extension_service::DpfHelmChartIdentity::from_service_id(second_id)
                    .slot_placement_label_key(0, 0)
            )
            .map(String::as_str),
        Some("enabled")
    );
    assert_eq!(
        labels
            .get(
                &model::extension_service::DpfHelmChartIdentity::from_service_id(second_id)
                    .placement_label_key
            )
            .map(String::as_str),
        Some("enabled"),
    );
    assert_eq!(
        labels.get("other-controller").map(String::as_str),
        Some("preserved")
    );
    let survivor_selectors =
        db::extension_service::find_chain_selectors(&fixture.env.pool, second_id).await?;
    assert!(!survivor_selectors.is_empty());
    assert!(
        survivor_selectors
            .iter()
            .all(|selector| !selector.excluded_dpu_ids.contains(&dpu_id))
    );
    assert_eq!(
        db::vpc_dpu_loopback::find(
            &mut *fixture.env.pool.acquire().await?,
            &dpu_id,
            &fixture.service_vpc_id,
        )
        .await?
        .expect("host and surviving service retain their loopback")
        .loopback_ip,
        shared_loopback
    );

    // Deliver the surviving endpoint after reclamation advances the generation under disabled admission.
    let surviving_delivery = fixture.acknowledge(dpu_id, true).await?;
    assert!(!surviving_delivery.use_admin_network);
    assert_eq!(surviving_delivery.service_interfaces.len(), 1);
    assert_eq!(
        surviving_delivery.service_interfaces[0].internal_uuid,
        Some(survivor.internal_uuid.into())
    );
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(std::slice::from_ref(&survivor)).await?;
    let surviving_evidence = fixture.observations().await?;
    assert_eq!(surviving_evidence.len(), 1);
    assert_eq!(surviving_evidence[0].node.as_ref(), Some(&original_node));
    assert!(surviving_evidence[0].network_acknowledged && surviving_evidence[0].chain_converged);
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // Final-member removal retains slot occupancy until its chain and network removal both converge.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .insert(second_id);
    fixture.select(&[]).await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(std::slice::from_ref(&survivor)).await?;
    assert_eq!(
        db::instance::highest_retained_service_vpc_slot(&fixture.env.pool, second_id).await?,
        Some(0)
    );
    {
        let mut state = fixture.state.lock().expect("DPF state");
        // Supply chain isolation while withholding only the placement stamp from this withdrawal.
        state.remaining_chains.remove(&second_id);
        // Absent labels without this withdrawal's stamp must not prove placement withdrawal.
        state.omit_placement_stamp_once = Some(dpu_id);
    }
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(std::slice::from_ref(&survivor)).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), 1);
    assert!(evidence[0].chain_isolated && evidence[0].network_acknowledged);
    assert!(!evidence[0].placement_observed);
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let empty = fixture.allocations(&[]).await?;
    assert!(empty.config.extension_services.service_configs.is_empty());
    for endpoint in &endpoints {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *fixture.env.pool.acquire().await?,
                &fixture.instance_id,
                &endpoint.network_segment_id,
            )
            .await?
            .is_empty()
        );
    }
    assert_eq!(
        db::vpc_dpu_loopback::find(
            &mut *fixture.env.pool.acquire().await?,
            &dpu_id,
            &fixture.service_vpc_id,
        )
        .await?
        .expect("host retains its loopback after final service removal")
        .loopback_ip,
        shared_loopback
    );
    assert_eq!(
        db::instance::highest_retained_service_vpc_slot(&fixture.env.pool, second_id).await?,
        None
    );
    let retired_second =
        db::extension_service::find_chain_selectors(&fixture.env.pool, second_id).await?;
    assert!(
        retired_second
            .iter()
            .any(|selector| selector.excluded_dpu_ids.contains(&dpu_id))
    );

    // Re-enable admission only after the accepted endpoints finish cleanup under disabled IPv4 policy.
    let pool = fixture.env.pool.clone();
    fixture.env = lifecycle_env(pool, fixture.state.clone(), false, true, false).await;
    fixture.host.api = fixture.env.api.clone();
    assert_eq!(
        db::extension_service::find_chain_selectors(&fixture.env.pool, second_id).await?,
        retired_second
    );

    // A new VPC may reuse slot zero only after both prior members have completed observed cleanup.
    let rebound_vpc = create_fnn_vpc(&fixture.env, "rebound service VPC").await;
    create_tenant_overlay_prefix_with_prefix(
        &fixture.env,
        rebound_vpc,
        "rebound link capacity",
        "10.240.0.0/29".parse()?,
    )
    .await;
    fixture.select(&[(0, rebound_vpc)]).await?;
    fixture.env.run_network_segment_controller_iteration().await;
    fixture.env.run_network_segment_controller_iteration().await;
    let rebound = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("rebound Instance");
    let rebound_endpoint = &rebound.config.network.service_interfaces[0];
    assert_eq!(rebound_endpoint.slot_index, 0);
    assert_eq!(rebound_endpoint.vpc_id, rebound_vpc);
    assert_ne!(rebound_endpoint.attachment_id, first_attachment);
    assert!(
        !endpoints
            .iter()
            .any(|endpoint| endpoint.internal_uuid == rebound_endpoint.internal_uuid)
    );
    // Current network acknowledgement cannot make an earlier attachment's chain evidence current.
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .state
        .lock()
        .expect("DPF state")
        .stale_chain_contexts
        .insert(first_id, initial_active_context);
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), 1);
    assert!(evidence[0].network_acknowledged && !evidence[0].chain_converged);
    assert_ne!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    // Restore the current chain context before retrying this activation.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .stale_chain_contexts
        .remove(&first_id);
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture
        .allocations(&rebound.config.network.service_interfaces)
        .await?;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    assert!(
        db::extension_service::find_chain_selectors(&fixture.env.pool, first_id)
            .await?
            .iter()
            .all(|selector| !selector.excluded_dpu_ids.contains(&dpu_id))
    );
    assert_eq!(
        db::extension_service::find_chain_selectors(&fixture.env.pool, second_id).await?,
        retired_second
    );

    // Complete the old controller request only after cleanup and rebind advanced the remote revision.
    let placement_snapshot = |state: &LifecycleDpfState| {
        state
            .devices
            .iter()
            .map(|(device, (_, observed))| {
                (
                    device.clone(),
                    observed.resource.clone(),
                    observed.node_labels.clone(),
                    observed.config_version.clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    let rebound_placements = placement_snapshot(&fixture.state.lock().expect("DPF state"));
    release_old_write
        .send(())
        .expect("delayed placement worker");
    tokio::time::timeout(Duration::from_secs(5), delayed_completion)
        .await
        .expect("delayed placement must finish")
        .expect("delayed placement completion");
    {
        let state = fixture.state.lock().expect("DPF state");
        assert!(matches!(
            state.delayed_placement_result.as_ref(),
            Some(Err(DpfError::InvalidState(_)))
        ));
        assert_eq!(placement_snapshot(&state), rebound_placements);
    }

    // Ordinary release also initiates removal without a tenant service update and retains ownership.
    fixture
        .env
        .api
        .release_instance(Request::new(rpc::InstanceReleaseRequest {
            id: Some(fixture.instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await?;
    let releasing = fixture
        .allocations(&rebound.config.network.service_interfaces)
        .await?;
    assert!(releasing.deleted.is_some());
    assert!(
        releasing
            .config
            .extension_services
            .service_configs
            .iter()
            .all(|config| config.removed.is_some())
    );
    assert_eq!(
        releasing.network_config_version.version_nr(),
        rebound.network_config_version.version_nr() + 1
    );

    // Ordinary release uses the same observed cleanup before any broad Instance-address deletion.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .insert(first_id);
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture
        .allocations(&rebound.config.network.service_interfaces)
        .await?;
    // Remove the release-time chain while replaying the first attachment's withdrawal context.
    {
        let mut state = fixture.state.lock().expect("DPF state");
        state.remaining_chains.remove(&first_id);
        // Keep only this pass's request so skipped observation cannot satisfy the replay check.
        state.chain_requests.clear();
        state
            .stale_chain_contexts
            .insert(first_id, old_withdrawal_context);
    }
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // Current placement and networking cannot let an old chain context reclaim the replacement.
    fixture
        .allocations(&rebound.config.network.service_interfaces)
        .await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].endpoint, *rebound_endpoint);
    assert_eq!(evidence[0].context.intent, ServiceVpcChainIntent::Retiring);
    let replacement_requests = fixture
        .state
        .lock()
        .expect("DPF state")
        .chain_requests
        .clone();
    assert_eq!(replacement_requests.len(), 1);
    assert_eq!(replacement_requests[0].0, evidence[0].context);
    assert!(
        evidence[0].placement_observed
            && evidence[0].network_acknowledged
            && !evidence[0].chain_isolated
    );

    // Restore the replacement's chain context so its own isolation can permit reclamation.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .stale_chain_contexts
        .remove(&first_id);
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&[]).await?;
    for endpoint in &rebound.config.network.service_interfaces {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *fixture.env.pool.acquire().await?,
                &fixture.instance_id,
                &endpoint.network_segment_id,
            )
            .await?
            .is_empty()
        );
    }
    assert!(
        db::extension_service::find_chain_selectors(&fixture.env.pool, first_id)
            .await?
            .iter()
            .any(|selector| selector.excluded_dpu_ids.contains(&dpu_id))
    );
    Ok(())
}

/// Retains a newly detected cleanup bypass when an Instance generation changes during chain I/O,
/// then resubmits it after restart so later chain absence cannot silently reclaim the retained endpoint.
#[crate::sqlx_test]
async fn cleanup_bypass_survives_stale_completion_and_restart(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Converge one accepted endpoint before beginning removal with a chain still present.
    let mut fixture = LifecycleFixture::new(pool, 1, 1, false).await?;
    let dpu_id = fixture.host.dpu_ids[0];
    fixture.request_tenant_generation().await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    fixture.finish_host_boot().await?;
    let endpoints = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("allocated Instance")
        .config
        .network
        .service_interfaces;
    let retained_node = fixture.observations().await?[0]
        .node
        .clone()
        .expect("retained Node");
    let service_id = fixture.services[0].service_id.parse()?;
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .insert(service_id);
    fixture.select(&[]).await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let terminating = fixture.allocations(&endpoints).await?;

    // Commit a newer Instance network generation after the controller's initial evidence transaction.
    let mut newer_network = terminating.config.network.clone();
    // Change the Instance host-interface routing profile without changing the retained service endpoints.
    newer_network.interfaces[0].routing_profile = Some(Default::default());
    let expected_network = newer_network.clone();
    let connect_options = fixture
        .env
        .pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("lock_timeout", "5s"), ("statement_timeout", "5s")]);
    let instance_id = fixture.instance_id;
    let network_version = terminating.network_config_version;
    {
        let mut state = fixture.state.lock().expect("DPF state");
        // Remove the chain while newly reporting its missing cleanup protection to separate absence from safe isolation.
        state.remaining_chains.remove(&service_id);
        state.newly_detected_cleanup_bypasses.insert(service_id);
        // Discard earlier calls so the next assertion identifies the request that first learns of the bypass.
        state.chain_requests.clear();
        state.before_next_chain_result = Some(Box::new(move || {
            // Use a fresh connection on its own runtime because the ready mock cannot await a concurrent writer.
            std::thread::spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("concurrent writer runtime")
                    .block_on(async move {
                        let mut connection = sqlx::PgConnection::connect_with(&connect_options)
                            .await
                            .expect("independent writer connection");
                        let mut txn = db::Transaction::begin_inner(&mut connection)
                            .await
                            .expect("writer transaction");
                        db::instance::update_network_config(
                            txn.as_mut(),
                            instance_id,
                            network_version,
                            &newer_network,
                            true,
                        )
                        .await
                        .expect("concurrent network generation update");
                        txn.commit()
                            .await
                            .expect("commit before returning bypass evidence");
                    });
            })
            .join()
            .expect("joined concurrent writer");
        }));
    }
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // The stale result must OR the new bypass into the stored endpoint without replacing its older context.
    let raced = fixture.allocations(&endpoints).await?;
    assert_eq!(raced.config.network, expected_network);
    assert_eq!(
        raced.network_config_version.version_nr(),
        network_version.version_nr() + 1
    );
    assert_eq!(
        raced.extension_services_config_version,
        terminating.extension_services_config_version
    );
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), 1);
    assert!(evidence[0].cleanup_bypass_observed);
    assert_eq!(evidence[0].context.network_config_version, network_version);
    assert_eq!(evidence[0].node.as_ref(), Some(&retained_node));
    assert!(!evidence[0].chain_isolated);
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating as i32]
    );
    {
        let state = fixture.state.lock().expect("DPF state");
        assert!(state.before_next_chain_result.is_none());
        assert_eq!(state.chain_requests.len(), 1);
        assert!(!state.chain_requests[0].1);
    }

    // Restart with the simulated bypass source and chain both absent, leaving PostgreSQL as the only bypass record.
    {
        let mut state = fixture.state.lock().expect("DPF state");
        state.newly_detected_cleanup_bypasses.clear();
        state.chain_requests.clear();
    }
    let pool = fixture.env.pool.clone();
    fixture.env = lifecycle_env(pool, fixture.state.clone(), false, true, false).await;
    fixture.host.api = fixture.env.api.clone();
    let removal_delivery = fixture.acknowledge(dpu_id, true).await?;
    assert!(removal_delivery.service_interfaces.is_empty());
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // Resubmitted bypass evidence must withhold Terminated and normalized address reclamation despite current ACKs.
    let retained = fixture.allocations(&endpoints).await?;
    assert_eq!(
        retained.network_config_version,
        raced.network_config_version
    );
    assert_eq!(
        retained.extension_services_config_version,
        raced.extension_services_config_version
    );
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), 1);
    assert!(evidence[0].cleanup_bypass_observed && evidence[0].network_acknowledged);
    assert_eq!(
        evidence[0].context.network_config_version,
        raced.network_config_version
    );
    assert_eq!(evidence[0].node.as_ref(), Some(&retained_node));
    assert!(evidence[0].chains.is_empty() && !evidence[0].chain_isolated);
    {
        let state = fixture.state.lock().expect("DPF state");
        assert!(
            state.newly_detected_cleanup_bypasses.is_empty() && state.remaining_chains.is_empty()
        );
        assert_eq!(state.chain_requests.len(), 1);
        assert!(state.chain_requests[0].1);
        assert_eq!(
            state.chain_requests[0].0.internal_uuid,
            endpoints[0].internal_uuid
        );
    }
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating as i32]
    );
    Ok(())
}

/// Discards a cleanup attempt captured before a concurrent host-network generation change and
/// requires its replacement acknowledgement, preserving both host fields and retained allocations.
/// Retains the final service loopback when only a staged host update still references its VPC.
#[crate::sqlx_test]
async fn host_network_change_invalidates_inflight_service_cleanup(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Converge one public attachment before requesting removal with its chain still present.
    let mut fixture = LifecycleFixture::new(pool, 1, 1, false).await?;
    let dpu_id = fixture.host.dpu_ids[0];
    fixture.request_tenant_generation().await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    fixture.finish_host_boot().await?;
    let initial = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("Instance");
    let endpoints = initial.config.network.service_interfaces.clone();
    let service_id = fixture.services[0].service_id.parse()?;
    // Enable overlap for a public host move from its original VPC into the service-only VPC.
    let pool = fixture.env.pool.clone();
    fixture.env = lifecycle_env(pool, fixture.state.clone(), true, true, false).await;
    fixture.host.api = fixture.env.api.clone();
    let pending_host_segment = create_tenant_network_segment(
        &fixture.env.api,
        Some(fixture.service_vpc_id),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
        "pending service VPC host interface",
        true,
    )
    .await;
    fixture.env.run_network_segment_controller_iteration().await;
    fixture.env.run_network_segment_controller_iteration().await;
    let service_loopback = db::vpc_dpu_loopback::find(
        &mut *fixture.env.pool.acquire().await?,
        &dpu_id,
        &fixture.service_vpc_id,
    )
    .await?
    .expect("initial service-only loopback")
    .loopback_ip;
    // Keep the chain present while public detachment and host-update staging commit.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .insert(service_id);
    fixture.select(&[]).await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let terminating = fixture.allocations(&endpoints).await?;
    // Stage the host move publicly while the retained chain still prevents service reclamation.
    let current = fixture
        .env
        .one_instance(fixture.instance_id)
        .await
        .into_inner();
    let mut config = current.config.expect("public Instance config");
    config.network = Some(single_interface_network_config(pending_host_segment));
    let staged = fixture
        .env
        .api
        .update_instance_config(Request::new(rpc::InstanceConfigUpdateRequest {
            instance_id: Some(fixture.instance_id),
            if_version_match: None,
            config: Some(config),
            metadata: current.metadata,
        }))
        .await?
        .into_inner();
    assert_eq!(staged.id, Some(fixture.instance_id));
    let persisted = fixture
        .env
        .one_instance(fixture.instance_id)
        .await
        .into_inner();
    assert_eq!(persisted.id, staged.id);
    let staged = fixture.allocations(&endpoints).await?;

    // Staging preserves the live host configuration and version; the concurrent write advances that version.
    assert_eq!(staged.config.network, terminating.config.network);
    assert_eq!(
        staged.network_config_version,
        terminating.network_config_version
    );
    let pending_host_update = staged
        .update_network_config_request
        .expect("staged host update");
    assert_eq!(
        pending_host_update.new_config.interfaces[0].vpc_id,
        Some(fixture.service_vpc_id)
    );
    assert!(
        pending_host_update
            .old_config
            .interfaces
            .iter()
            .all(|interface| interface.vpc_id != Some(fixture.service_vpc_id))
    );

    // Save persisted removal evidence before chain absence can make the next result conclusive.
    let evidence_before_race = fixture.observations().await?;
    // Remove the simulated chain before racing a newer network generation against reconciliation.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .remove(&service_id);

    // Hold the Instance lock until the next controller pass captures its older device revision.
    let mut writer = fixture.env.pool.txn_begin().await?;
    db::instance::find_by_id_for_update(writer.as_mut(), fixture.instance_id)
        .await?
        .expect("locked Instance");
    let writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let reads_before = fixture.state.lock().expect("DPF state").placement_reads;
    let writes_before = fixture
        .state
        .lock()
        .expect("DPF state")
        .successful_placements
        .clone();
    let mut updated_network = terminating.config.network.clone();
    // An explicit empty routing profile is a valid host configuration change independent of service endpoints.
    updated_network.interfaces[0].routing_profile = Some(Default::default());
    let expected_network = updated_network.clone();

    // Commit the newer host configuration only after the controller is waiting to revalidate its snapshot.
    let update = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let observed = fixture.state.lock().expect("DPF state").placement_reads;
                if observed > reads_before {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("controller must capture placement before the writer commits");
        wait_for_blocked_query(&fixture.env.pool, writer_pid, "FROM instances i").await;
        db::instance::update_network_config(
            writer.as_mut(),
            fixture.instance_id,
            terminating.network_config_version,
            &updated_network,
            true,
        )
        .await?;
        writer.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let ((), result) = tokio::join!(
        fixture
            .env
            .run_machine_state_controller_iteration_no_requeue(),
        update,
    );
    result?;

    // The old pass must preserve the new host fields without publishing or reclaiming stale work.
    let raced = fixture.allocations(&endpoints).await?;
    assert_eq!(raced.config.network, expected_network);
    assert_eq!(
        raced.extension_services_config_version,
        terminating.extension_services_config_version
    );
    assert_eq!(
        raced.network_config_version.version_nr(),
        terminating.network_config_version.version_nr() + 1
    );
    assert_eq!(
        fixture
            .state
            .lock()
            .expect("DPF state")
            .successful_placements,
        writes_before
    );

    // The raced pass must retain the prior evidence instead of publishing stale cleanup results.
    assert_eq!(fixture.observations().await?, evidence_before_race);

    // Fresh isolation cannot borrow the old network acknowledgement after the host update.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(
        evidence
            .iter()
            .all(|observed| observed.chain_isolated && !observed.network_acknowledged)
    );
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating as i32]
    );
    // Acknowledge the raced generation so service cleanup can finish while the host move remains staged.
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let reclaimed = fixture.allocations(&[]).await?;
    assert_eq!(
        reclaimed.config.network.interfaces,
        expected_network.interfaces
    );
    assert_eq!(
        reclaimed.extension_services_config_version.version_nr(),
        terminating.extension_services_config_version.version_nr() + 1
    );
    assert_eq!(
        reclaimed.network_config_version.version_nr(),
        raced.network_config_version.version_nr() + 1
    );
    // Only the still-staged new host configuration references this DPU/VPC after the final service is reclaimed.
    assert!(
        reclaimed
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    assert!(
        reclaimed
            .config
            .network
            .interfaces
            .iter()
            .all(|interface| interface.vpc_id != Some(fixture.service_vpc_id))
    );
    assert_eq!(
        reclaimed.update_network_config_request.as_ref(),
        Some(&pending_host_update)
    );
    assert_eq!(
        db::vpc_dpu_loopback::find(
            &mut *fixture.env.pool.acquire().await?,
            &dpu_id,
            &fixture.service_vpc_id,
        )
        .await?
        .expect("pending host membership retains loopback")
        .loopback_ip,
        service_loopback
    );
    let retained_allocation = db::resource_pool::find_value(
        &mut *fixture.env.pool.acquire().await?,
        &service_loopback.to_string(),
    )
    .await?
    .into_iter()
    .find(|entry| {
        entry.pool_name
            == fixture
                .env
                .common_pools
                .ethernet
                .pool_vpc_dpu_loopback_ip
                .name()
    })
    .expect("retained loopback allocation");
    assert_eq!(
        retained_allocation.state.0,
        ResourcePoolEntryState::Allocated {
            owner: dpu_id.to_string(),
            owner_type: OwnerType::Machine.to_string(),
        }
    );
    Ok(())
}

/// Keeps a secondary DPU's accepted service generation required during force-delete from Admin,
/// preventing an old Admin receipt from releasing a service port that was delivered independently.
/// A current removal generation must also reject retained secondary UUIDs until the report confirms
/// their absence, preventing a generation-only acknowledgement from reclaiming their allocations.
#[crate::sqlx_test]
async fn force_delete_from_admin_waits_for_secondary_service_removal(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Apply initial Admin responses before any Tenant transition, including the secondary service port.
    let fixture = LifecycleFixture::new(pool, 2, 1, false).await?;
    let initial = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("allocated Instance");
    let endpoints = initial.config.network.service_interfaces.clone();
    let mut txn = fixture.env.db_txn().await;
    let primary = fixture
        .host
        .host()
        .db_machine(&mut txn)
        .await
        .primary_attached_dpu_machine_id()
        .expect("primary DPU");
    txn.rollback().await?;
    let secondary = fixture
        .host
        .dpu_ids
        .iter()
        .copied()
        .find(|dpu| *dpu != primary)
        .expect("secondary DPU");
    let primary_admin = fixture.acknowledge(primary, true).await?;
    let secondary_admin = fixture.acknowledge(secondary, true).await?;
    assert!(primary_admin.use_admin_network && primary_admin.service_interfaces.is_empty());
    assert!(secondary_admin.use_admin_network && !secondary_admin.service_interfaces.is_empty());

    // Request removal while the same Admin host generation is still acknowledged on both DPUs.
    let pending = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            fixture.host.id,
            false,
        )))
        .await?
        .into_inner();
    assert!(!pending.all_done);
    let terminating = fixture.allocations(&endpoints).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // Primary Admin omitted all service ports, while the secondary's old populated report remains incomplete.
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    let primary_evidence = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == primary)
        .expect("primary withdrawal");
    let secondary_evidence = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == secondary)
        .expect("secondary withdrawal");
    assert!(primary_evidence.chain_isolated && primary_evidence.network_acknowledged);
    assert!(secondary_evidence.chain_isolated && !secondary_evidence.network_acknowledged);
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating as i32]
    );
    assert_eq!(
        primary_evidence
            .managed_host_config_version
            .version_string(),
        primary_admin.managed_host_config_version
    );
    assert_eq!(
        secondary_evidence
            .managed_host_config_version
            .version_string(),
        secondary_admin.managed_host_config_version
    );

    // Replace only the current removal report's empty UUID set with the secondary's previously delivered UUID.
    let removed_network = fixture.acknowledge(secondary, true).await?;
    assert!(removed_network.service_interfaces.is_empty());
    fixture
        .env
        .api
        .record_dpu_network_status(Request::new(rpc::DpuNetworkStatus {
            dpu_machine_id: Some(secondary),
            network_config_version: Some(removed_network.managed_host_config_version.clone()),
            instance_network_config_version: (!removed_network
                .instance_network_config_version
                .is_empty())
            .then(|| removed_network.instance_network_config_version.clone()),
            service_vpc_network_observation: Some(rpc::ServiceVpcNetworkStatusObservation {
                network_config_version: Some(terminating.network_config_version.version_string()), // Match the removal generation to isolate UUID membership.
                interfaces: vec![rpc::ServiceVpcInterfaceStatusObservation {
                    internal_uuid: secondary_admin.service_interfaces[0].internal_uuid.clone(), // Retain a delivered local UUID after its requested removal.
                    network_security_group: None,
                }],
            }),
            dpu_health: Some(::rpc::health::HealthReport {
                source: health_report::HealthReport::DPU_AGENT_SOURCE.to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await?;

    // Reconcile the retained UUID at the removal generation before accepting an empty report.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // A retained secondary UUID must withhold acknowledgement and capacity despite current tokens and isolation.
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    let secondary_evidence = evidence
        .iter()
        .find(|observed| observed.endpoint.dpu_id == secondary)
        .expect("secondary withdrawal with a retained UUID");
    assert_eq!(
        secondary_evidence.context.intent,
        ServiceVpcChainIntent::Retiring
    );
    assert_eq!(
        secondary_evidence.context.network_config_version,
        terminating.network_config_version
    );
    assert_eq!(
        secondary_evidence
            .managed_host_config_version
            .version_string(),
        removed_network.managed_host_config_version
    );
    assert!(secondary_evidence.chain_isolated && !secondary_evidence.network_acknowledged);
    assert!(
        evidence
            .iter()
            .any(|observed| observed.endpoint.dpu_id == primary
                && observed.chain_isolated
                && observed.network_acknowledged)
    );
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating as i32]
    );

    // Only an exact empty secondary report at the same removal generation permits reclamation.
    let empty_removal = fixture.acknowledge(secondary, true).await?;
    assert!(empty_removal.service_interfaces.is_empty());
    assert_eq!(
        empty_removal.managed_host_config_version,
        removed_network.managed_host_config_version
    );
    assert_eq!(
        empty_removal
            .instance
            .expect("retained Instance")
            .network_config_version,
        terminating.network_config_version.version_string()
    );
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let reclaimed = fixture.allocations(&[]).await?;
    assert!(
        reclaimed
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    assert_eq!(
        reclaimed.extension_services_config_version.version_nr(),
        terminating.extension_services_config_version.version_nr() + 1
    );
    assert_eq!(
        reclaimed.network_config_version.version_nr(),
        terminating.network_config_version.version_nr() + 1
    );
    for endpoint in &endpoints {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *fixture.env.pool.acquire().await?,
                &fixture.instance_id,
                &endpoint.network_segment_id,
            )
            .await?
            .is_empty()
        );
    }
    Ok(())
}

/// Rejects a diagnostic GET captured before detachment when it resumes after endpoint reclamation,
/// preventing a removed service interface from recreating a durable DPU/VPC loopback allocation.
#[crate::sqlx_test]
async fn stale_network_config_get_cannot_reallocate_a_reclaimed_service_loopback(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // The service VPC has the final loopback membership; host networking uses another VPC.
    let fixture = LifecycleFixture::new(pool, 1, 1, false).await?;
    let dpu_id = fixture.host.dpu_ids[0];
    // Apply the initial service network before testing concurrent endpoint removal.
    fixture.request_tenant_generation().await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Require service convergence before advancing host boot and admitting public detachment.
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    fixture.finish_host_boot().await?;

    // Only the selected service VPC has an NSG, placing its read after segment capture.
    let network_security_group = fixture
        .env
        .api
        .create_network_security_group(Request::new(rpc::CreateNetworkSecurityGroupRequest {
            id: None,
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            metadata: Some(rpc::Metadata {
                name: "stale service GET policy".to_string(),
                ..Default::default()
            }),
            network_security_group_attributes: Some(Default::default()),
        }))
        .await?
        .into_inner()
        .network_security_group
        .expect("created service VPC NSG");
    fixture
        .env
        .api
        .update_vpc(Request::new(rpc::VpcUpdateRequest {
            id: Some(fixture.service_vpc_id),
            metadata: Some(rpc::Metadata {
                name: "stale service GET VPC".to_string(),
                ..Default::default()
            }),
            network_security_group_id: Some(network_security_group.id),
            ..Default::default()
        }))
        .await?;
    // Block only the service policy read so the GET retains its active endpoint and segment.
    let mut gate = fixture.env.db_txn().await;
    let gate_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(gate.as_mut())
        .await?;
    sqlx::query("LOCK TABLE network_security_groups IN ACCESS EXCLUSIVE MODE")
        .execute(gate.as_mut())
        .await?;
    let api = fixture.env.api.clone();
    let stale_get = TestTaskHandle::new(tokio::spawn(async move {
        api.get_managed_host_network_config(Request::new(rpc::ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(dpu_id),
        }))
        .await
    }));
    wait_for_blocked_query(&fixture.env.pool, gate_pid, "network_security_groups").await;

    // Removal omits the service NSG read, letting the Agent acknowledge while the old GET waits.
    fixture.select(&[]).await?;
    let removal = fixture.acknowledge(dpu_id, true).await?;
    assert!(removal.service_interfaces.is_empty());
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Verify reclamation committed before allowing the cached endpoint to reach allocation.
    let reclaimed = fixture.allocations(&[]).await?;
    assert!(
        reclaimed
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    let mut persisted = fixture.env.pool.acquire().await?;
    let service_loopback =
        db::vpc_dpu_loopback::find(&mut *persisted, &dpu_id, &fixture.service_vpc_id).await?;
    assert!(service_loopback.is_none());
    let reclaimed_pool = db::resource_pool::stats(
        &mut *persisted,
        fixture
            .env
            .common_pools
            .ethernet
            .pool_vpc_dpu_loopback_ip
            .name(),
    )
    .await?;
    drop(persisted);

    // Resume only after cleanup commits, requiring a retry instead of a new allocation.
    gate.rollback().await?;
    let error = tokio::time::timeout(Duration::from_secs(5), stale_get.join())
        .await
        .expect("stale diagnostic GET completes")
        .expect("diagnostic GET task")
        .expect_err("a removed endpoint must not recreate its loopback");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    // Reload the loopback and pool after the stale RPC, then verify a fresh GET can succeed.
    let mut persisted = fixture.env.pool.acquire().await?;
    let service_loopback =
        db::vpc_dpu_loopback::find(&mut *persisted, &dpu_id, &fixture.service_vpc_id).await?;
    assert!(service_loopback.is_none());
    assert_eq!(
        db::resource_pool::stats(
            &mut *persisted,
            fixture
                .env
                .common_pools
                .ethernet
                .pool_vpc_dpu_loopback_ip
                .name(),
        )
        .await?,
        reclaimed_pool,
    );
    drop(persisted);
    let fresh = fixture.acknowledge(dpu_id, true).await?;
    assert!(fresh.service_interfaces.is_empty());
    Ok(())
}

/// Keeps the host in Reset through current Admin acknowledgement until chain isolation completes,
/// then reclaims the attachment before deleting the Instance so Admin cannot bypass DPF cleanup.
/// Both DPUs must acknowledge Admin, and fresh chain isolation still retains the live Instance and
/// allocations until the secondary reports the current service-removal generation with no service UUIDs.
#[crate::sqlx_test]
async fn reset_waits_for_chain_isolation_after_admin_acknowledgement(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use model::machine::ResetState;

    // Converge a DPF attachment on both DPUs before resetting a host with Tenant networking applied.
    let fixture = LifecycleFixture::new(pool, 2, 1, false).await?;
    fixture.request_tenant_generation().await?;
    for &dpu_id in &fixture.host.dpu_ids {
        fixture.acknowledge(dpu_id, true).await?;
    }
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    fixture.finish_host_boot().await?;
    let initial = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("allocated Instance");
    let endpoints = initial.config.network.service_interfaces.clone();
    let service_id = fixture.services[0].service_id.parse()?;
    // Retain each endpoint's Node identity so cleanup cannot substitute another DPU's evidence.
    let original_nodes = fixture
        .observations()
        .await?
        .into_iter()
        .map(|observed| {
            (
                observed.endpoint.dpu_id,
                observed.node.expect("retained Node"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let host_segment = initial.config.network.interfaces[0]
        .network_segment_id
        .expect("host segment");
    let host_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *fixture.env.pool.acquire().await?,
        &fixture.instance_id,
        &host_segment,
    )
    .await?
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect::<Vec<_>>();
    assert!(!host_addresses.is_empty());
    let mut txn = fixture.env.db_txn().await;
    let snapshot = fixture.host.snapshot(&mut txn).await;
    let tenant_version = snapshot.host_snapshot.network_config.version;
    // Select the secondary from persisted topology so its receipt cannot use the primary Admin exemption.
    let secondary = fixture
        .host
        .dpu_ids
        .iter()
        .copied()
        .find(|dpu_id| Some(*dpu_id) != snapshot.host_snapshot.primary_attached_dpu_machine_id())
        .expect("secondary DPU");
    txn.rollback().await?;

    // Request reset while retaining a target chain to isolate cleanup from Admin networking.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .insert(service_id);
    fixture.host.mark_machine_for_updates().await;
    fixture
        .env
        .api
        .trigger_managed_host_reset(Request::new(rpc::ManagedHostResetRequest {
            machine_id: Some(fixture.host.id.into()),
            mode: rpc::managed_host_reset_request::Mode::Set.into(),
            initiator: rpc::UpdateInitiator::AdminCli.into(),
            allow_reset_with_instance: true,
            ignore_cleanup: true, // Stop at DPF teardown after Instance deletion instead of exercising host wiping.
        }))
        .await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let deleting_instance = ManagedHostState::Reset {
        reset_state: ResetState::DeletingInstance,
    };
    let mut txn = fixture.env.db_txn().await;
    assert_eq!(
        fixture.host.snapshot(&mut txn).await.managed_state,
        deleting_instance
    );
    txn.rollback().await?;

    // Persist removal and both generations before reset may publish Admin networking.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let terminating = fixture.allocations(&endpoints).await?;
    assert!(terminating.deleted.is_none());
    assert!(
        terminating.config.extension_services.service_configs[0]
            .removed
            .is_some()
    );
    assert_eq!(
        terminating.extension_services_config_version.version_nr(),
        initial.extension_services_config_version.version_nr() + 1
    );
    assert_eq!(
        terminating.network_config_version.version_nr(),
        initial.network_config_version.version_nr() + 1
    );
    let mut txn = fixture.env.db_txn().await;
    let snapshot = fixture.host.snapshot(&mut txn).await;
    assert!(!snapshot.use_admin_network());
    assert_eq!(
        snapshot.host_snapshot.network_config.version,
        tenant_version
    );
    txn.rollback().await?;

    // Publish a new Admin generation while preserving the Instance and service addresses.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Replace the previous Admin wait with DoNothing so the retry must persist its own diagnostic.
    let mut txn = fixture.env.db_txn().await;
    db::machine::update_controller_state_outcome(
        txn.as_mut(),
        &fixture.host.id.into(),
        PersistentStateHandlerOutcome::DoNothing { source_ref: None },
    )
    .await?;
    txn.commit().await?;

    // Retry before Admin acknowledgement while the retained chain still prevents service reclamation.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let mut txn = fixture.env.db_txn().await;
    let snapshot = fixture.host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, deleting_instance);
    assert!(snapshot.use_admin_network());
    assert!(!snapshot.managed_host_network_config_version_synced());
    assert_ne!(
        snapshot.host_snapshot.network_config.version,
        tenant_version
    );
    // Check that the persisted Admin wait takes precedence over pending service cleanup.
    assert!(matches!(
        &snapshot.host_snapshot.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason
                == "waiting for DPUs to apply Admin networking before deleting the Instance"
    ));
    txn.rollback().await?;

    // Acknowledge Admin and secondary service removal so chain isolation remains an independent cleanup requirement.
    for dpu_id in &fixture.host.dpu_ids {
        network_configured_with_health(&fixture.env, dpu_id, None).await;
    }
    fixture.acknowledge(secondary, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&endpoints).await?;
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(evidence.iter().all(|observed| observed.context.intent
        == ServiceVpcChainIntent::Retiring
        && observed.node.as_ref() == original_nodes.get(&observed.endpoint.dpu_id)
        && observed.placement_observed
        && observed.network_acknowledged
        && !observed.chain_isolated));
    let mut txn = fixture.env.db_txn().await;
    let snapshot = fixture.host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, deleting_instance);
    assert!(snapshot.managed_host_network_config_version_synced());
    assert!(matches!(&snapshot.host_snapshot.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason == "waiting for service VPC network cleanup before deleting the Instance"));
    let retained_host_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &fixture.instance_id,
        &host_segment,
    )
    .await?
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect::<Vec<_>>();
    assert_eq!(retained_host_addresses, host_addresses);
    txn.rollback().await?;
    let exclusions =
        db::extension_service::find_chain_selectors(&fixture.env.pool, service_id).await?;
    assert!(fixture.host.dpu_ids.iter().all(|dpu_id| {
        exclusions
            .iter()
            .any(|selector| selector.excluded_dpu_ids.contains(dpu_id))
    }));

    // Omit only the secondary service receipt from a new report that still acknowledges current Admin.
    let missing_removal = fixture.acknowledge(secondary, false).await?;
    assert!(missing_removal.use_admin_network && missing_removal.service_interfaces.is_empty());
    assert!(missing_removal.instance_network_config_version.is_empty());

    // Remove both chains so the secondary service receipt is the only incomplete cleanup requirement.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .remove(&service_id);
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    // Reload the live Instance and service evidence to prove outer Admin receipts cannot release capacity.
    let retained = fixture.allocations(&endpoints).await?;
    assert!(retained.deleted.is_none());
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), endpoints.len());
    assert!(evidence.iter().all(|observed| observed.chain_isolated
        && observed.network_acknowledged == (observed.endpoint.dpu_id != secondary)));
    let mut txn = fixture.env.db_txn().await;
    let snapshot = fixture.host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, deleting_instance);
    assert!(snapshot.managed_host_network_config_version_synced());
    assert_eq!(
        snapshot
            .host_snapshot
            .network_config
            .version
            .version_string(),
        missing_removal.managed_host_config_version
    );
    assert!(matches!(&snapshot.host_snapshot.controller_state_outcome,
        Some(PersistentStateHandlerOutcome::Wait { reason, .. })
            if reason == "waiting for service VPC network cleanup before deleting the Instance"));
    txn.rollback().await?;

    // Accept the empty secondary receipt without changing Admin desire before retrying reclamation.
    let removed_network = fixture.acknowledge(secondary, true).await?;
    assert!(removed_network.service_interfaces.is_empty());
    assert_eq!(
        removed_network.managed_host_config_version,
        missing_removal.managed_host_config_version
    );
    assert_eq!(
        removed_network
            .instance
            .expect("live Instance during Reset")
            .network_config_version,
        terminating.network_config_version.version_string()
    );
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let reclaimed = fixture.allocations(&[]).await?;
    assert!(
        reclaimed
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    assert_eq!(
        reclaimed.extension_services_config_version.version_nr(),
        terminating.extension_services_config_version.version_nr() + 1
    );
    assert_eq!(
        reclaimed.network_config_version.version_nr(),
        terminating.network_config_version.version_nr() + 1
    );
    for endpoint in &endpoints {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *fixture.env.pool.acquire().await?,
                &fixture.instance_id,
                &endpoint.network_segment_id,
            )
            .await?
            .is_empty()
        );
    }
    assert_eq!(
        db::extension_service::find_chain_selectors(&fixture.env.pool, service_id).await?,
        exclusions
    );

    // Only the following reset iteration may delete the Instance and enter DPF teardown.
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert!(
        db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await?
            .is_none()
    );
    let mut txn = fixture.env.db_txn().await;
    assert_eq!(
        fixture.host.snapshot(&mut txn).await.managed_state,
        ManagedHostState::Reset {
            reset_state: ResetState::DeletingCrs
        }
    );
    txn.rollback().await?;
    Ok(())
}

/// Rejects final reclamation when the host generation, DPU receipt, or shared selector changes
/// after status publication, preserving allocations until the current cleanup context is observed.
/// A host generation change must still wait for a new acknowledgement before reclamation.
#[crate::sqlx_test]
async fn final_service_cleanup_rechecks_locked_evidence(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Converge and terminate one service while keeping its chain present until removal is acknowledged.
    let fixture = LifecycleFixture::new(pool, 1, 1, false).await?;
    let dpu_id = fixture.host.dpu_ids[0];
    fixture.request_tenant_generation().await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    fixture.finish_host_boot().await?;
    let initial = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("allocated Instance");
    let endpoints = initial.config.network.service_interfaces.clone();
    let service_id = fixture.services[0].service_id.parse()?;
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .insert(service_id);
    fixture.select(&[]).await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let terminating = fixture.allocations(&endpoints).await?;
    // Supply chain absence before the final cleanup attempt captures its otherwise complete evidence.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .remove(&service_id);
    #[derive(Clone, Copy, Debug)]
    enum Change {
        HostGeneration,
        MissingServiceReceipt,
        SelectorRevision,
    }
    for change in [
        // A new host generation must be read after the waiting controller obtains its host lock.
        Change::HostGeneration,
        // A replacement receipt can remove required service evidence without changing desired generations.
        Change::MissingServiceReceipt,
        // Restored selector membership still invalidates isolation from an earlier shared revision.
        Change::SelectorRevision,
    ] {
        // Restore current network acknowledgement before each pass captures complete cleanup evidence.
        fixture.acknowledge(dpu_id, true).await?;
        let mut txn = fixture.env.db_txn().await;
        let host = fixture.host.host().db_machine(&mut txn).await;
        let host_version = host.network_config.version;
        let host_network = host.network_config.value.clone();
        txn.rollback().await?;

        // Block only the final reclamation phase so observation and status publication finish first.
        let mut address_blocker = fixture.env.pool.txn_begin().await?;
        sqlx::query(
            "SELECT id FROM instance_addresses WHERE instance_id = $1 ORDER BY id FOR UPDATE",
        )
        .bind(fixture.instance_id)
        .fetch_all(&mut *address_blocker)
        .await?;
        let address_writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *address_blocker)
            .await?;
        let update = async {
            wait_for_blocked_query(
                &fixture.env.pool,
                address_writer_pid,
                "SELECT id FROM instance_addresses",
            )
            .await;
            // Require committed completion before changing the facts that final reclamation must reload.
            let published = fixture.observations().await?;
            assert_eq!(published.len(), endpoints.len());
            assert!(published.iter().all(|observed| {
                observed.deployment_status() == ExtensionServiceDeploymentStatus::Terminated
            }));
            match change {
                Change::HostGeneration => {
                    let mut host_writer = fixture.env.pool.txn_begin().await?;
                    let host_writer_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
                        .fetch_one(&mut *host_writer)
                        .await?;
                    // Advance host and DPU network_config_version values so unchanged configuration
                    // cannot conceal a stale acknowledgement.
                    assert!(matches!(
                        db::machine::try_update_network_config(
                            host_writer.as_mut(),
                            &fixture.host.id,
                            host_version,
                            &host_network,
                        )
                        .await?,
                        db::ConditionalWrite::Applied(_)
                    ));

                    // Release the address wait only after the newer host and DPU generations are locked.
                    address_blocker.commit().await?;
                    // Wait at the host lock so its following snapshot starts after this writer commits.
                    wait_for_blocked_query(
                        &fixture.env.pool,
                        host_writer_pid,
                        "SELECT id FROM machines WHERE id = $1 FOR UPDATE",
                    )
                    .await;
                    host_writer.commit().await?;
                }
                Change::MissingServiceReceipt => {
                    // Replace the receipt through the public API after completion has been published.
                    fixture.acknowledge(dpu_id, false).await?;
                    // Reload the report to distinguish missing service evidence from a host-version mismatch.
                    let dpu = db::machine::find_one(&fixture.env.pool, &dpu_id, Default::default())
                        .await?
                        .expect("reporting DPU");
                    let receipt = dpu.network_status_observation.expect("replacement receipt");
                    assert_eq!(receipt.network_config_version, Some(host_version));
                    assert!(receipt.service_vpc_network_observation.is_none());
                    address_blocker.commit().await?;
                }
                Change::SelectorRevision => {
                    // Advance the shared selector token while restoring its exact persisted exclusion set.
                    let context = &published[0].context;
                    let mut selector_version = context.selector_version;
                    let mut selector_writer = fixture.env.pool.txn_begin().await?;
                    for change in [
                        // Remove the exclusion so restoring it must advance the shared revision.
                        db::extension_service::ServiceVpcChainExclusion::Allow,
                        // Restore the same membership to isolate selector revision from placement desire.
                        db::extension_service::ServiceVpcChainExclusion::Exclude,
                    ] {
                        let db::ConditionalWrite::Applied(selector) =
                            db::extension_service::try_update_chain_exclusion(
                                selector_writer.as_mut(),
                                context.service_id,
                                context.interface_ordinal,
                                context.slot_index,
                                dpu_id,
                                Some(selector_version),
                                change,
                            )
                            .await?
                        else {
                            panic!("selector membership update must apply");
                        };
                        selector_version = selector.version;
                    }
                    selector_writer.commit().await?;
                    // Find the committed selector before allowing final cleanup to inspect its revision.
                    let selector = db::extension_service::find_chain_selectors(
                        &fixture.env.pool,
                        context.service_id,
                    )
                    .await?
                    .into_iter()
                    .find(|selector| {
                        selector.interface_ordinal == context.interface_ordinal
                            && selector.slot == context.slot_index
                    })
                    .expect("updated selector");
                    assert_ne!(selector.version, context.selector_version);
                    assert_eq!(selector.excluded_dpu_ids, vec![dpu_id]);
                    address_blocker.commit().await?;
                }
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        };
        let ((), result) = tokio::join!(
            fixture
                .env
                .run_machine_state_controller_iteration_no_requeue(),
            update,
        );
        result?;

        // Reload the host separately so receipt and selector cases cannot pass through the host-generation guard.
        let mut txn = fixture.env.db_txn().await;
        let host = fixture.host.host().db_machine(&mut txn).await;
        if matches!(change, Change::HostGeneration) {
            assert_ne!(host.network_config.version, host_version);
        } else {
            assert_eq!(host.network_config.version, host_version, "{change:?}");
        }
        txn.rollback().await?;

        // The stale final pass must retain both Instance versions and all normalized endpoint allocations.
        let retained = fixture.allocations(&endpoints).await?;
        assert_eq!(
            retained.extension_services_config_version,
            terminating.extension_services_config_version,
            "{change:?}",
        );
        assert_eq!(
            retained.network_config_version, terminating.network_config_version,
            "{change:?}",
        );
        assert_eq!(
            fixture.public_states().await,
            vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating as i32],
            "{change:?}",
        );
        let evidence = fixture.observations().await?;
        assert_eq!(evidence.len(), endpoints.len());
        assert!(evidence.iter().all(|observed| observed.chain_isolated));
    }

    // Fresh acknowledgement and selector evidence permit the next pass to reclaim the same terminating attachment.
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    let reclaimed = fixture.allocations(&[]).await?;
    assert!(
        reclaimed
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    for endpoint in &endpoints {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *fixture.env.pool.acquire().await?,
                &fixture.instance_id,
                &endpoint.network_segment_id,
            )
            .await?
            .is_empty()
        );
    }
    Ok(())
}

/// Verifies ordinary release atomically begins attachment termination, retries idempotently, and
/// waits before deleting the Instance while its attachment and service-interface records remain for cleanup.
/// Chain isolation then permits release to delete the Instance and its retained host and service addresses.
/// With an unapproved DPU request, release leaves host and DPU rows unlocked while waiting for host
/// state history, preserving the lock order used by force-delete's Admin switch.
#[crate::sqlx_test]
async fn test_release_waits_for_service_vpc_endpoint_cleanup(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Converge one public DPF attachment before exercising ordinary release's endpoint guard.
    let fixture = LifecycleFixture::new(pool, 1, 1, false).await?;
    let dpu_id = fixture.host.dpu_ids[0];
    fixture.request_tenant_generation().await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    fixture.finish_host_boot().await?;
    let env = &fixture.env;
    let managed_host = &fixture.host;
    let instance_id = fixture.instance_id;
    let endpoint = db::instance::find_by_id(&env.pool, instance_id)
        .await?
        .expect("allocated Instance")
        .config
        .network
        .service_interfaces[0]
        .clone();

    // Retain the target chain so Admin acknowledgement alone cannot reclaim its allocation.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .insert(fixture.services[0].service_id.parse()?);
    let before_release = db::instance::find_by_id(&env.pool, instance_id)
        .await
        .unwrap()
        .expect("instance exists before release");

    // Capture nonempty host allocations so final absence cannot pass without releasing real addresses.
    let host_segment = before_release.config.network.interfaces[0]
        .network_segment_id
        .expect("host segment");
    let host_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *env.pool.acquire().await?,
        &instance_id,
        &host_segment,
    )
    .await?
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect::<Vec<_>>();
    assert!(!host_addresses.is_empty());

    // Queue an unapproved DPU request outside any automatic reboot period.
    assert!(
        env.config
            .machine_updater
            .instance_autoreboot_period
            .is_none()
    );
    let mut txn = env.db_txn().await;
    db::machine::trigger_dpu_reprovisioning_request(&dpu_id, txn.as_mut(), "test", true).await?;
    txn.commit().await?;

    // Enter the real release workflow without running background cleanup yet.
    env.api
        .release_instance(Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .unwrap();

    // Reload after release to verify `removed` and both configuration-version increments.
    let after_release = db::instance::find_by_id(&env.pool, instance_id)
        .await
        .unwrap()
        .expect("terminating instance remains stored");
    let removal_time = after_release
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.id == Some(endpoint.attachment_id))
        .and_then(|attachment| attachment.removed)
        .expect("release marks the endpoint owner terminating");
    assert_eq!(
        after_release.extension_services_config_version.version_nr(),
        before_release
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        after_release.network_config_version.version_nr(),
        before_release.network_config_version.version_nr() + 1
    );
    assert_eq!(
        after_release.config.network.service_interfaces,
        vec![endpoint.clone()]
    );

    // Repeat release to prove the retry preserves `removed`, `service_interfaces`, and both versions.
    env.api
        .release_instance(Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .unwrap();
    let after_retry = db::instance::find_by_id(&env.pool, instance_id)
        .await
        .unwrap()
        .expect("terminating instance remains stored after retry");
    assert_eq!(
        after_retry.extension_services_config_version,
        after_release.extension_services_config_version
    );
    assert_eq!(
        after_retry.network_config_version,
        after_release.network_config_version
    );
    assert_eq!(
        after_retry.config.network.service_interfaces,
        vec![endpoint.clone()]
    );
    assert_eq!(
        after_retry
            .config
            .extension_services
            .service_configs
            .iter()
            .find(|attachment| attachment.id == Some(endpoint.attachment_id))
            .and_then(|attachment| attachment.removed),
        Some(removal_time)
    );

    // Hold host history while ordinary release still has an unapproved DPU request to timestamp.
    let mut history_guard = env.db_txn().await;
    assert!(
        db::machine::try_lock_machine_state_history(
            history_guard.as_mut(),
            &managed_host.id.into(),
        )
        .await?
    );
    let history_guard_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(history_guard.as_mut())
        .await?;

    // Observe the history wait before taking host and DPU locks in the order used by the Admin switch.
    let check_release_lock_order = async {
        wait_for_blocked_query(&env.pool, history_guard_pid, "machine_state_history").await;
        for machine_id in [MachineId::from(managed_host.id), dpu_id.into()] {
            sqlx::query("SELECT id FROM machines WHERE id = $1 FOR UPDATE NOWAIT")
                .bind(machine_id)
                .fetch_one(history_guard.as_mut())
                .await?;
        }
        history_guard.rollback().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let ((), lock_order) = tokio::join!(
        env.run_machine_state_controller_iteration_no_requeue(),
        check_release_lock_order,
    );
    lock_order?;

    // Reload the accepted deletion step and its timestamp to prove the Ready branch ran without approval.
    let mut txn = env.db_txn().await;
    let release_progress = managed_host.snapshot(&mut txn).await;
    assert_eq!(
        release_progress.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostPlatformConfiguration {
                platform_config_state: HostPlatformConfigurationState::FactoryResetBmc {
                    reset_state: model::machine::FactoryResetBmcState::CheckPreconditions,
                },
            },
        }
    );
    let started_request = release_progress.dpu_snapshots[0]
        .reprovision_requested
        .as_ref()
        .expect("deletion retains the queued DPU request");
    assert!(started_request.started_at.is_some());
    assert!(!started_request.user_approval_received);
    txn.rollback().await?;

    // Drive background release to the final Admin-network wait after termination is durable.
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        8,
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostPlatformConfiguration {
                platform_config_state:
                    model::machine::HostPlatformConfigurationState::CheckHostConfig,
            },
        },
    )
    .await;
    fixture.acknowledge(dpu_id, true).await?;
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        2,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForDpusToUp,
        },
    )
    .await;
    fixture.acknowledge(dpu_id, true).await?;
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: model::machine::RetryInfo { count: 0 },
            },
        },
    )
    .await;
    // Simulate the requested reboot so release advances from
    // `BootingWithDiscoveryImage` to `WaitingForNetworkReconfig`.
    let mut txn = env.db_txn().await;
    let host = managed_host.host().db_machine(&mut txn).await;
    db::machine::update_reboot_time(&host, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let waiting = ManagedHostState::Assigned {
        instance_state: InstanceState::WaitingForNetworkReconfig,
    };
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        2,
        waiting.clone(),
    )
    .await;
    fixture.acknowledge(dpu_id, true).await?;

    // Current Admin networking and withdrawn placement cannot bypass the retained target chain.
    env.run_machine_state_controller_iteration().await;

    // Persisted DPF evidence must leave chain isolation as the remaining cleanup condition.
    let evidence = fixture.observations().await?;
    assert_eq!(evidence.len(), 1);
    assert!(
        evidence[0].placement_observed
            && evidence[0].network_acknowledged
            && !evidence[0].chain_isolated
    );
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, waiting);
    assert!(
        matches!(
            &snapshot.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason
                == "Waiting for service VPC network cleanup before deleting the instance."
        ),
        "expected the service-network cleanup wait, got {:?}",
        snapshot.host_snapshot.controller_state_outcome
    );
    let persisted = snapshot
        .instance
        .expect("terminating instance remains assigned");
    assert!(persisted.deleted.is_some());
    assert_eq!(
        persisted
            .config
            .extension_services
            .service_configs
            .iter()
            .find(|attachment| attachment.id == Some(endpoint.attachment_id))
            .and_then(|attachment| attachment.removed),
        Some(removal_time)
    );
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![endpoint.clone()]
    );
    // The cleanup wait must retain host addresses as well as the service allocation checked below.
    let retained_host_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &instance_id,
        &host_segment,
    )
    .await?
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect::<Vec<_>>();
    assert_eq!(retained_host_addresses, host_addresses);
    txn.commit().await.unwrap();
    fixture.allocations(&[endpoint.clone()]).await?;

    // Reclaim the isolated service before the next release iteration may delete its Instance.
    fixture
        .state
        .lock()
        .expect("DPF state")
        .remaining_chains
        .remove(&fixture.services[0].service_id.parse()?);
    env.run_machine_state_controller_iteration_no_requeue()
        .await;
    fixture.allocations(&[]).await?;
    env.run_machine_state_controller_iteration_no_requeue()
        .await;

    // Completed release must persist its next state and remove the Instance plus both address allocations.
    let mut txn = env.db_txn().await;
    assert_eq!(
        managed_host.snapshot(&mut txn).await.managed_state,
        ManagedHostState::PostAssignedMeasuring {
            attestation_mode: AttestationMode::MeasuredBoot {
                measuring_state: MeasuringState::WaitingForMeasurements,
            },
        }
    );
    txn.rollback().await?;
    assert!(
        env.find_instances(vec![instance_id])
            .await
            .instances
            .is_empty()
    );
    for segment_id in [host_segment, endpoint.network_segment_id] {
        assert!(
            db::instance_address::find_all_by_instance_id_and_segment_id(
                &mut *env.pool.acquire().await?,
                &instance_id,
                &segment_id,
            )
            .await?
            .is_empty()
        );
    }

    Ok(())
}

/// Rejects Instance power requests after default graceful force-delete without changing boot flags or
/// approving queued updates, so the API cannot acknowledge work withheld by pending deletion.
/// Rejects direct tenant reboots before they can interrupt the same cleanup.
#[crate::sqlx_test]
async fn pending_force_delete_rejects_instance_power_without_changing_boot_or_update_requests(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_test_support::Outcome::FailsWith;
    use carbide_test_support::{Case, check_cases_async};

    // Create one admitted endpoint and request Tenant networking; the fixture supplies ordinary BMC access.
    let fixture = LifecycleFixture::new(pool, 1, 1, false).await?;
    fixture.request_tenant_generation().await?;

    // Acknowledge the current network generation before checking service convergence.
    fixture.acknowledge(fixture.host.dpu_ids[0], true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;

    // Confirm service convergence before completing the separately tested host boot.
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );

    // Persist Assigned/Ready so reboot requests select normal Ready behavior.
    fixture.finish_host_boot().await?;

    // Add the required host-update health alert before queuing an unstarted DPU request.
    fixture.host.mark_machine_for_updates().await;
    fixture
        .host
        .host()
        .trigger_dpu_reprovisioning(rpc::dpu_reprovisioning_request::Mode::Set, false)
        .await;

    // Queue the host update too, leaving both approvals for InvokeInstancePower without a controller pass.
    fixture
        .env
        .api
        .trigger_host_reprovisioning(Request::new(rpc::HostReprovisioningRequest {
            machine_id: Some(fixture.host.id.into()),
            mode: rpc::host_reprovisioning_request::Mode::Set.into(), // Queue an unapproved host update too.
            initiator: rpc::UpdateInitiator::AdminCli.into(),
        }))
        .await?;

    // Initiate default graceful force-delete publicly, including its Some(false) pending marker.
    let pending = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            fixture.host.id,
            false, // Retain endpoints for observed cleanup rather than authorize orphaning.
        )))
        .await?
        .into_inner();
    assert!(!pending.all_done);

    // Reload admission state so the cases exercise delegated, approval, and direct reboot paths.
    let mut txn = fixture.env.db_txn().await;
    let before = fixture.host.snapshot(&mut txn).await;
    txn.rollback().await?;
    assert_eq!(
        before.host_snapshot.force_delete_requires_admin_ack,
        Some(false)
    );
    assert_eq!(
        before.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    );
    let terminating_instance = before.instance.as_ref().expect("retained Instance");
    assert!(terminating_instance.deleted.is_some());
    assert!(!terminating_instance.custom_pxe_reboot_requested);
    let queued_dpu_update = before.dpu_snapshots[0]
        .reprovision_requested
        .as_ref()
        .expect("queued DPU update");
    assert!(queued_dpu_update.started_at.is_none());
    assert!(!queued_dpu_update.user_approval_received);
    let queued_host_update = before
        .host_snapshot
        .host_reprovision_requested
        .as_ref()
        .expect("queued host update");
    assert!(queued_host_update.started_at.is_none());
    assert!(!queued_host_update.user_approval_received);
    let timepoint = fixture.env.redfish_sim.timepoint();
    let api = &fixture.env.api;

    // Reject each distinct reboot path with the same pending-intent error before it publishes work.
    check_cases_async(
        [
            // Custom PXE would queue a Ready request that pending deletion cannot consume.
            ("custom PXE reboot", true, false),
            // Approval would otherwise acknowledge both unstarted updates without starting either.
            ("approve queued updates", false, true),
            // Ordinary reboot would otherwise issue synchronous Redfish during cleanup.
            ("direct reboot", false, false),
        ]
        .into_iter()
        .map(
            |(scenario, boot_with_custom_ipxe, apply_updates_on_reboot)| Case {
                scenario,
                input: rpc::InstancePowerRequest {
                    instance_id: Some(fixture.instance_id),
                    operation: rpc::instance_power_request::Operation::PowerReset.into(),
                    boot_with_custom_ipxe,
                    apply_updates_on_reboot,
                },
                expect: FailsWith((
                    tonic::Code::FailedPrecondition,
                    format!(
                        "host {} is pending force deletion; instance power control is unavailable",
                        fixture.host.id
                    ),
                )),
            },
        ),
        |request| async move {
            api.invoke_instance_power(Request::new(request))
                .await
                .map(|_| ())
                .map_err(|error| (error.code(), error.message().to_owned()))
        },
    )
    .await;

    // Find the original Instance publicly before inspecting persisted boot fields and update requests.
    assert_eq!(
        fixture
            .env
            .one_instance(fixture.instance_id)
            .await
            .into_inner()
            .id,
        Some(fixture.instance_id)
    );
    let mut txn = fixture.env.db_txn().await;
    let after = fixture.host.snapshot(&mut txn).await;
    txn.rollback().await?;
    let retained_instance = after.instance.as_ref().expect("retained Instance");
    assert_eq!(
        (
            retained_instance.custom_pxe_reboot_requested,
            retained_instance.use_custom_pxe_on_boot,
        ),
        (
            terminating_instance.custom_pxe_reboot_requested,
            terminating_instance.use_custom_pxe_on_boot,
        )
    );
    assert_eq!(
        after.dpu_snapshots[0].reprovision_requested,
        before.dpu_snapshots[0].reprovision_requested
    );
    assert_eq!(
        serde_json::to_value(&after.host_snapshot.host_reprovision_requested)?,
        serde_json::to_value(&before.host_snapshot.host_reprovision_requested)?
    );

    // Observe hardware effects independently of the API error and committed database values.
    assert!(
        fixture
            .env
            .redfish_sim
            .actions_since(&timepoint)
            .all_hosts()
            .is_empty()
    );
    Ok(())
}

/// Completes an active DPF power cycle after graceful force-delete starts, retaining the Instance
/// and service allocations until the recovering agent can acknowledge network removal.
#[crate::sqlx_test]
async fn graceful_force_delete_resumes_dpf_reboot_without_releasing_the_instance(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    check_pending_force_delete_power_cycle(pool, false).await
}

/// Resumes an ordinary release power cycle after graceful force-delete interrupts its saved Off phase,
/// retaining its Instance and service allocations and waiting in UnlockHost so power recovery cannot
/// continue ordinary release before force-delete polling completes.
#[crate::sqlx_test]
async fn graceful_force_delete_resumes_release_power_cycle_without_releasing_the_instance(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    check_pending_force_delete_power_cycle(pool, true).await
}

/// Checks saved power-off recovery during pending force deletion while retaining the Instance and
/// service allocations, preventing the pending marker from stranding the later power-on operation.
async fn check_pending_force_delete_power_cycle(
    pool: PgPool,
    ordinary_release: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_redfish::libredfish::RedfishClientPool;
    use model::machine::{DpfState, DpuReprovisionStates, PerformPowerOperation, ReprovisionState};

    // Start with a current Tenant service so force-delete must request a fresh Admin receipt.
    // Vary only ordinary_release; one DPU and one service expose the same retained capacity.
    let fixture = LifecycleFixture::new(pool, 1, 1, false).await?;
    let dpu_id = fixture.host.dpu_ids[0];
    fixture.request_tenant_generation().await?;
    fixture.acknowledge(dpu_id, true).await?;
    fixture
        .env
        .run_machine_state_controller_iteration_no_requeue()
        .await;
    assert_eq!(
        fixture.public_states().await,
        vec![rpc::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32]
    );
    let endpoints = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
        .await?
        .expect("allocated Instance")
        .config
        .network
        .service_interfaces;
    fixture.allocations(&endpoints).await?;
    let reprovision = |substate| ManagedHostState::Assigned {
        instance_state: InstanceState::DPUReprovision {
            dpu_states: DpuReprovisionStates {
                states: [(dpu_id, ReprovisionState::DpfStates { substate })]
                    .into_iter()
                    .collect(),
            },
        },
    };

    let power_cycle = |power_on, retry_count| {
        // Ordinary release has its own saved phase and must not depend on a DPU reprovision request.
        if ordinary_release {
            ManagedHostState::Assigned {
                instance_state: InstanceState::HostPlatformConfiguration {
                    platform_config_state: HostPlatformConfigurationState::PowerCycle {
                        power_on,
                        power_on_retry_count: retry_count,
                    },
                },
            }
        } else {
            reprovision(DpfState::HandleReboot {
                op: if power_on {
                    PerformPowerOperation::On
                } else {
                    PerformPowerOperation::Off
                },
                retry_count,
            })
        }
    };

    // Start ordinary release publicly so its saved power cycle belongs to a soft-deleted Instance.
    if ordinary_release {
        fixture.finish_host_boot().await?;
        fixture
            .env
            .api
            .release_instance(Request::new(rpc::InstanceReleaseRequest {
                id: Some(fixture.instance_id),
                ..Default::default()
            }))
            .await?;
    }

    // Persist the saved Off phase so force-delete must leave the durable recovery checkpoint intact.
    let mut txn = fixture.env.db_txn().await;
    let host = fixture.host.host().db_machine(&mut txn).await;
    let bmc_access = fixture.host.host().bmc_access(&mut txn).await;
    // Only DPF recovery has a started reprovision request; ordinary release must resume without one.
    if !ordinary_release {
        db::machine::trigger_dpu_reprovisioning_request(&dpu_id, txn.as_mut(), "test", true)
            .await?;
        db::machine::update_dpu_reprovision_start_time(&dpu_id, txn.as_mut()).await?;
    }
    db::machine::advance(&host, &mut txn, &power_cycle(false, 0), None).await?;
    txn.commit().await?;

    // Simulate the completed Off command before force-delete can request the fresh Admin generation.
    fixture
        .env
        .redfish_sim
        .client_by_info(&bmc_access)
        .await?
        .power(libredfish::SystemPowerControl::ForceOff)
        .await?;
    let timepoint = fixture.env.redfish_sim.timepoint();

    // Public force-delete must retain the original Instance while durably requesting removal.
    let pending = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            fixture.host.id,
            false,
        )))
        .await?
        .into_inner();
    assert!(!pending.all_done);
    let terminating = fixture.allocations(&endpoints).await?;
    assert!(terminating.deleted.is_some());
    assert!(
        terminating
            .config
            .extension_services
            .service_configs
            .iter()
            .all(|attachment| attachment.removed.is_some())
    );

    // Reload the saved Off phase to verify force-delete changed only removal intent and Admin desire.
    let mut txn = fixture.env.db_txn().await;
    let pending_recovery = fixture.host.snapshot(&mut txn).await;
    assert_eq!(pending_recovery.managed_state, power_cycle(false, 0));
    assert_eq!(
        pending_recovery
            .host_snapshot
            .force_delete_requires_admin_ack,
        Some(false)
    );
    assert!(pending_recovery.use_admin_network());
    assert_eq!(
        pending_recovery.instance.expect("retained Instance").id,
        fixture.instance_id
    );
    txn.rollback().await?;

    // Expect ordinary release to wait in UnlockHost while DPF completes its existing reboot handoff.
    let powered_on_state = if ordinary_release {
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostPlatformConfiguration {
                platform_config_state: HostPlatformConfigurationState::UnlockHost {
                    unlock_host_state: model::machine::UnlockHostState::DisableLockdown,
                },
            },
        }
    } else {
        reprovision(DpfState::WaitingForReady { phase_detail: None })
    };

    // Each saved recovery step must advance through the pending force-delete marker.
    for (expected, power_on_expected, reboot_completions) in [
        // Save On intent before issuing power so a crash can safely resume the cycle.
        (power_cycle(true, 0), false, 0),
        // Issue On from durable intent so the powered-off host can restore its DPU agent.
        (power_cycle(true, 1), true, 0),
        // Notify DPF only after power-on is observed so provisioning can continue;
        // ordinary release instead waits in UnlockHost while force-delete remains pending.
        (
            powered_on_state.clone(),
            true,
            usize::from(!ordinary_release),
        ),
    ] {
        fixture
            .env
            .run_machine_state_controller_iteration_no_requeue()
            .await;

        // Reload the actual recovery substate and original Instance instead of inferring progress from calls.
        let mut txn = fixture.env.db_txn().await;
        let recovered = fixture.host.snapshot(&mut txn).await;
        assert_eq!(recovered.managed_state, expected);
        assert_eq!(
            recovered.host_snapshot.force_delete_requires_admin_ack,
            Some(false)
        );
        assert!(recovered.use_admin_network());
        assert_eq!(
            recovered.instance.expect("retained Instance").id,
            fixture.instance_id
        );
        txn.rollback().await?;
        fixture.allocations(&endpoints).await?;

        // Observe the power command and DPF completion alongside the persisted transition.
        assert_eq!(
            fixture
                .env
                .redfish_sim
                .actions_since(&timepoint)
                .all_hosts()
                .contains(&RedfishSimAction::Power(libredfish::SystemPowerControl::On)),
            power_on_expected
        );
        assert_eq!(
            fixture.state.lock().expect("DPF state").reboot_completions,
            reboot_completions
        );
    }

    // Keep ordinary release parked after power-on so force-delete polling retains deletion control.
    if ordinary_release {
        fixture
            .env
            .run_machine_state_controller_iteration_no_requeue()
            .await;

        // Reload UnlockHost and its Wait outcome to prove pending force deletion withheld that step.
        let mut txn = fixture.env.db_txn().await;
        let paused_release = fixture.host.snapshot(&mut txn).await;
        assert_eq!(paused_release.managed_state, powered_on_state);
        assert!(matches!(
            paused_release.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason
                == "Waiting for force-delete polling before releasing the Instance."
        ));
        txn.rollback().await?;
        fixture.allocations(&endpoints).await?;
    }
    Ok(())
}
