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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::DpuMachineId;
use carbide_uuid::vpc::VpcId;
use model::instance::config::network::InstanceServiceInterfaceConfig;
use model::machine::network::ServiceVpcNetworkStatusObservation;
use model::metadata::Metadata;
use rpc::forge;
use rpc::forge::forge_server::Forge;

use crate::tests::common::api_fixtures::instance::{
    default_os_config, persist_service_vpc_attachment, seed_service_vpc_endpoint,
    single_interface_network_config,
};
use crate::tests::common::api_fixtures::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, create_tenant_network_segment,
};
use crate::tests::common::api_fixtures::{
    TestEnv, create_managed_host_multi_dpu, create_test_env, populate_network_security_groups,
};
use crate::tests::common::rpc_builder::VpcCreationRequest;

/// Keeps three committed service interfaces together so tests can distinguish logical interface
/// counts from shared slots and repeated instance membership.
struct ServiceVpcPropagationFixture {
    env: TestEnv,
    instance_id: InstanceId,
    vpc_id: VpcId,
    host_vpc_id: VpcId,
    interfaces: [InstanceServiceInterfaceConfig; 3],
    group: forge::NetworkSecurityGroup,
}

impl ServiceVpcPropagationFixture {
    /// Creates two shared-slot interfaces and one secondary-DPU interface to exercise VPC
    /// accounting with separate or shared host networking and an optional instance NSG.
    async fn new(pool: sqlx::PgPool, overlap: bool, configure_instance_nsg: bool) -> Self {
        // Reuse the committed NSG and attachment fixtures because public service activation is gated.
        let env = create_test_env(pool).await;
        populate_network_security_groups(env.api.clone()).await;
        let group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";
        let group = env
            .api
            .find_network_security_groups_by_ids(tonic::Request::new(
                forge::FindNetworkSecurityGroupsByIdsRequest {
                    network_security_group_ids: vec![group_id.to_string()],
                    tenant_organization_id: None,
                },
            ))
            .await
            .expect("find fixture NSG")
            .into_inner()
            .network_security_groups
            .remove(0);
        let vpc_id = VpcId::new();
        let segment = env
            .create_vpc_and_tenant_segment_with_vpc_details(
                VpcCreationRequest::builder("Tenant1")
                    .id(vpc_id)
                    .metadata(Metadata::new_with_default_name())
                    .network_security_group_id(group_id)
                    .rpc(),
            )
            .await;
        // Share the VPC to check combined interface counts and instance NSG precedence.
        let (host_vpc_id, host_segment) = if overlap {
            (vpc_id, segment)
        } else {
            let host_vpc = env
                .api
                .create_vpc(tonic::Request::new(
                    VpcCreationRequest::builder("Tenant1")
                        .metadata(Metadata::new_with_default_name())
                        .network_security_group_id(group_id) // Keep both VPCs eligible for the same NSG.
                        .rpc(),
                ))
                .await
                .expect("create host VPC")
                .into_inner();
            let host_segment = create_tenant_network_segment(
                &env.api,
                host_vpc.id,
                FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1], // Avoid the service segment's prefix.
                "HOST",
                true,
            )
            .await;
            env.run_network_segment_controller_iteration().await;
            env.run_network_segment_controller_iteration().await;
            (host_vpc.id.expect("host VPC ID"), host_segment)
        };
        let host = create_managed_host_multi_dpu(&env, 2).await;
        let instance_id = host
            .instance_builer(&env)
            .config(forge::InstanceConfig {
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(host_segment)),
                network_security_group_id: configure_instance_nsg
                    .then(|| "b65b13d6-d81c-11ef-9252-b346dc360bd4".to_string()), // Keep host policy distinct from VPC policy.
                ..Default::default()
            })
            .build()
            .await
            .id;

        // Distinct attachments share slot zero on the primary; the first also serves the secondary.
        let first = seed_service_vpc_endpoint(&env, instance_id, host.dpu_ids[0], segment).await;
        let mut second =
            seed_service_vpc_endpoint(&env, instance_id, host.dpu_ids[0], segment).await;
        // Keep the synthetic link addresses distinct while retaining the shared slot.
        second.link_prefix = "192.0.2.2/31".parse().expect("second service prefix");
        let secondary = InstanceServiceInterfaceConfig {
            dpu_id: host.dpu_ids[1], // Require evidence from the secondary DPU.
            internal_uuid: uuid::Uuid::new_v4(), // Give this logical interface independent evidence.
            link_prefix: "192.0.2.4/31".parse().expect("secondary service prefix"),
            ..first.clone()
        };
        let fixture = Self {
            env,
            instance_id,
            vpc_id,
            host_vpc_id,
            interfaces: [first, second, secondary],
            group,
        };

        // Publish all three service interfaces after attachment seeding finishes updating the
        // instance configuration.
        fixture.replace_interfaces(&fixture.interfaces).await;
        fixture
    }

    /// Commits exact service-interface membership and reloads it so removal tests exercise
    /// persisted configuration rather than an in-memory fixture.
    async fn replace_interfaces(&self, interfaces: &[InstanceServiceInterfaceConfig]) {
        // Preserve host networking while changing only the internal service-interface records.
        let mut txn = self.env.db_txn().await;
        let stored = db::instance::find_by_id(txn.as_mut(), self.instance_id)
            .await
            .expect("find instance")
            .expect("instance exists");
        let mut network = stored.config.network;
        network.service_interfaces = interfaces.to_vec();
        db::instance::update_network_config(
            txn.as_mut(),
            self.instance_id,
            stored.network_config_version,
            &network,
            false,
        )
        .await
        .expect("replace service interfaces");
        txn.commit().await.expect("commit service interfaces");

        // Read committed membership because propagation queries consume stored service interfaces.
        let persisted = db::instance::find_by_id(&self.env.pool, self.instance_id)
            .await
            .expect("reload instance")
            .expect("instance exists");
        assert_eq!(persisted.config.network.service_interfaces, interfaces);
    }

    /// Records and reloads service policy without a host network version so service-only
    /// acknowledgements prove the real report-to-database path.
    async fn report(
        &self,
        dpu_id: DpuMachineId,
        identities: &[uuid::Uuid],
        group: &forge::NetworkSecurityGroup,
    ) {
        // Supply only service evidence and the DPU health required by status validation.
        let report = forge::ServiceVpcNetworkStatusObservation {
            interfaces: identities
                .iter()
                .map(|identity| forge::ServiceVpcInterfaceStatusObservation {
                    internal_uuid: Some((*identity).into()), // Correlate each logical service interface.
                    network_security_group: Some(forge::NetworkSecurityGroupStatus {
                        id: group.id.clone(), // Select the reported policy independently of its configured VPC.
                        version: group.version.clone(), // Detect stale policy even without a host version.
                        source: forge::NetworkSecurityGroupSource::NsgSourceVpc.into(),
                    }),
                })
                .collect(),
        };
        self.env
            .api
            .record_dpu_network_status(tonic::Request::new(forge::DpuNetworkStatus {
                dpu_machine_id: Some(dpu_id),
                service_vpc_network_observation: Some(report.clone()),
                dpu_health: Some(rpc::health::HealthReport {
                    source: "dpu-agent".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await
            .expect("record service policy");

        // Reload the persisted report because RPC success alone does not prove applied evidence.
        let persisted = db::machine::find_dpu_network_status_inputs(&self.env.pool, &dpu_id)
            .await
            .expect("find DPU status")
            .expect("DPU exists")
            .network_status_observation
            .expect("network status exists");
        assert_eq!(
            persisted.service_vpc_network_observation,
            Some(ServiceVpcNetworkStatusObservation::try_from(report).expect("valid report"))
        );
        assert!(persisted.instance_network_observation.is_none());
    }

    /// Verifies exact query counts and public instance membership so service accounting cannot
    /// silently collapse shared-slot interfaces or lose the affected instance.
    async fn assert_status(&self, expected: u32, applied: u32) {
        // Check the narrow count contract with a selected VPC and NSG.
        let group_id = self.group.id.parse().expect("fixture NSG ID");
        let (vpcs, _) = db::network_security_group::get_propagation_status(
            &mut self.env.db_reader(),
            Some(&[group_id]),
            None,
            Some(&[self.vpc_id]),
            Some(&[]),
        )
        .await
        .expect("query propagation counts");
        if expected == 0 {
            assert!(
                vpcs.is_empty(),
                "zero targets retain the existing empty-result contract"
            );
        } else {
            assert_eq!(vpcs.len(), 1);
            assert_eq!(
                (vpcs[0].interfaces_expected, vpcs[0].interfaces_applied),
                (expected, applied)
            );
            assert_eq!(vpcs[0].related_instance_ids, vec![self.instance_id]);
            assert_eq!(
                vpcs[0].unpropagated_instance_ids,
                if expected == applied {
                    vec![]
                } else {
                    vec![self.instance_id]
                }
            );
        }

        // Verify the public status and deduplicated instance IDs independently of the count fields.
        let public = self
            .env
            .api
            .get_network_security_group_propagation_status(tonic::Request::new(
                forge::GetNetworkSecurityGroupPropagationStatusRequest {
                    vpc_ids: vec![self.vpc_id.to_string()],
                    instance_ids: vec![],
                    network_security_group_ids: Some(forge::NetworkSecurityGroupIdList {
                        ids: vec![self.group.id.clone()],
                    }),
                },
            ))
            .await
            .expect("get propagation status")
            .into_inner();
        let expected_public = if expected == 0 {
            vec![]
        } else {
            vec![forge::NetworkSecurityGroupPropagationObjectStatus {
                id: self.vpc_id.to_string(),
                status: if applied == expected {
                    forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull
                } else if applied == 0 {
                    forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusNone
                } else {
                    forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusPartial
                }
                .into(),
                details: None,
                related_instance_ids: vec![self.instance_id.to_string()],
                unpropagated_instance_ids: if applied == expected {
                    vec![]
                } else {
                    vec![self.instance_id.to_string()]
                },
            }]
        };
        assert_eq!(public.vpcs, expected_public);
        assert!(public.instances.is_empty());
    }
}

/// Counts each active service interface once on its reporting DPU and requires fresh VPC policy
/// after an NSG update; removed, terminating, and deleted-instance records cannot retain targets.
/// Detaching the NSG omits the VPC despite retained endpoints and applied evidence, while an
/// attached empty NSG remains a propagation target.
/// Selecting separate host and service VPCs together keeps service counts in their configured
/// VPC, preventing applied service policy from inflating another selected VPC's result.
/// Current UUIDs reported without NSG metadata receive no applied credit after NSG attachment,
/// so a report without policy cannot acknowledge the newly attached NSG.
#[crate::sqlx_test]
async fn service_only_vpc_propagation_tracks_policy_and_membership(pool: sqlx::PgPool) {
    Box::pin(async move {
        // Use a VPC with no host interfaces so the query must discover service-only targets.
        let mut fixture = ServiceVpcPropagationFixture::new(pool, false, false).await;
        let [first, second, secondary] = fixture.interfaces.clone();
        fixture.assert_status(3, 0).await;

        // Exercise distinct identity and NSG failures before acknowledging each DPU independently.
        let mut unrelated_group = fixture.group.clone();
        // Change only the NSG ID to isolate ID matching from version matching.
        unrelated_group.id = "b65b13d6-d81c-11ef-9252-b346dc360bd4".to_string();
        for (scenario, dpu_id, identities, group, applied) in [
            // An observation on another DPU cannot satisfy either primary interface.
            (
                "wrong DPU",
                secondary.dpu_id,
                vec![first.internal_uuid, second.internal_uuid],
                &fixture.group,
                0,
            ),
            // Matching identities cannot acknowledge a different NSG, even at the same version.
            (
                "wrong NSG",
                first.dpu_id,
                vec![first.internal_uuid, second.internal_uuid],
                &unrelated_group,
                0,
            ),
            // One current UUID cannot acknowledge the other logical interface sharing its slot.
            (
                "first shared-slot interface applied",
                first.dpu_id,
                vec![first.internal_uuid],
                &fixture.group,
                1,
            ),
            // Two logical interfaces sharing one slot must contribute two applied interfaces.
            (
                "primary applied",
                first.dpu_id,
                vec![first.internal_uuid, second.internal_uuid],
                &fixture.group,
                2,
            ),
            // Secondary service evidence must work without a host instance network version.
            (
                "secondary applied",
                secondary.dpu_id,
                vec![secondary.internal_uuid],
                &fixture.group,
                3,
            ),
        ] {
            eprintln!("verifying service propagation: {scenario}");
            fixture.report(dpu_id, &identities, group).await;
            fixture.assert_status(3, applied).await;
        }

        // Reject excluded service targets through each query filter without repeating the host matrix.
        for (scenario, groups, tenant, vpcs) in [
            // An explicit empty NSG selection must not behave like an omitted selection.
            ("empty NSG filter", Some(vec![]), None, vec![fixture.vpc_id]),
            // Tenant filtering must also constrain VPCs reached only through service interfaces.
            (
                "other tenant",
                None,
                Some("Tenant2".parse().expect("tenant ID")),
                vec![fixture.vpc_id],
            ),
            // A service interface must not leak into another requested VPC.
            ("other VPC", None, None, vec![VpcId::new()]),
        ] {
            let (vpcs, _) = db::network_security_group::get_propagation_status(
                &mut fixture.env.db_reader(),
                groups.as_deref(),
                tenant.as_ref(),
                Some(&vpcs),
                Some(&[]),
            )
            .await
            .expect("query filtered service propagation");
            assert!(vpcs.is_empty(), "{scenario}");
        }

        // Select both real VPCs while the reports contain applied service policy and no host evidence.
        let mut txn = fixture.env.db_txn().await;
        let (vpcs, instances) = db::network_security_group::get_propagation_status(
            txn.as_mut(),
            None,
            None,
            Some(&[fixture.vpc_id, fixture.host_vpc_id]),
            Some(&[]),
        )
        .await
        .expect("query propagation for separate host and service VPCs");
        txn.commit().await.expect("finish separate VPC read");

        // Verify each selected VPC retains its exact counts and affected-instance membership.
        assert_eq!(vpcs.len(), 2);
        assert!(instances.is_empty());
        for (vpc_id, expected, applied, unpropagated_instance_ids) in [
            // Acknowledged service interfaces contribute only to their configured VPC.
            (fixture.vpc_id, 3, 3, vec![]),
            // The host VPC must not count the service VPC's acknowledged interfaces.
            (fixture.host_vpc_id, 1, 0, vec![fixture.instance_id]),
        ] {
            let status = vpcs
                .iter()
                .find(|status| status.id == vpc_id.to_string())
                .expect("selected VPC result");
            assert_eq!(
                (status.interfaces_expected, status.interfaces_applied),
                (expected, applied)
            );
            assert_eq!(status.related_instance_ids, vec![fixture.instance_id]);
            assert_eq!(status.unpropagated_instance_ids, unpropagated_instance_ids);
        }

        // Seed duplicate persisted evidence directly because report conversion discards service
        // observations containing duplicate UUIDs.
        sqlx::query(
            "UPDATE machines SET network_status_observation = jsonb_set(
                network_status_observation,
                '{service_vpc_network_observation,interfaces}',
                (network_status_observation #> '{service_vpc_network_observation,interfaces}') ||
                (network_status_observation #> '{service_vpc_network_observation,interfaces}')
            ) WHERE id = $1",
        )
        .bind(first.dpu_id)
        .execute(&fixture.env.pool)
        .await
        .expect("duplicate stored evidence");

        // Verify duplicated service evidence persisted without increasing the applied-interface count.
        let mut txn = fixture.env.db_txn().await;
        let duplicated = db::machine::find_dpu_network_status_inputs(txn.as_mut(), &first.dpu_id)
            .await
            .expect("reload duplicate evidence")
            .expect("DPU exists")
            .network_status_observation
            .expect("network observation exists")
            .service_vpc_network_observation
            .expect("service report exists");
        assert_eq!(duplicated.interfaces.len(), 4);
        txn.commit().await.expect("finish duplicate read");
        fixture.assert_status(3, 3).await;

        // Change only NSG policy, using empty rules to retain the attached-empty-NSG contract.
        let network_version = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await
            .expect("find instance")
            .expect("instance exists")
            .network_config_version;
        let updated = fixture
            .env
            .api
            .update_network_security_group(tonic::Request::new(
                forge::UpdateNetworkSecurityGroupRequest {
                    id: fixture.group.id.clone(),
                    tenant_organization_id: fixture.group.tenant_organization_id.clone(),
                    metadata: fixture.group.metadata.clone(),
                    network_security_group_attributes: Some(
                        forge::NetworkSecurityGroupAttributes {
                            rules: vec![], // An attached empty group still requires matching applied evidence.
                            ..fixture
                                .group
                                .attributes
                                .clone()
                                .expect("fixture NSG attributes")
                        },
                    ),
                    if_version_match: Some(fixture.group.version.clone()),
                },
            ))
            .await
            .expect("update VPC NSG")
            .into_inner()
            .network_security_group
            .expect("updated NSG");

        // Reload the NSG and instance to verify stale reports stop counting without changing
        // network_config_version.
        fixture.group = fixture
            .env
            .api
            .find_network_security_groups_by_ids(tonic::Request::new(
                forge::FindNetworkSecurityGroupsByIdsRequest {
                    network_security_group_ids: vec![updated.id.clone()],
                    tenant_organization_id: None,
                },
            ))
            .await
            .expect("reload updated NSG")
            .into_inner()
            .network_security_groups
            .remove(0);
        assert_eq!(fixture.group, updated);
        assert!(
            fixture
                .group
                .attributes
                .as_ref()
                .expect("updated NSG attributes")
                .rules
                .is_empty()
        );
        let persisted = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await
            .expect("reload instance")
            .expect("instance exists");
        assert_eq!(persisted.network_config_version, network_version);

        // Verify the primary's old report survives the update so deletion cannot explain
        // zero applied interfaces.
        let mut txn = fixture.env.db_txn().await;
        let retained = db::machine::find_dpu_network_status_inputs(txn.as_mut(), &first.dpu_id)
            .await
            .expect("reload stale primary evidence")
            .expect("DPU exists")
            .network_status_observation
            .expect("network observation exists")
            .service_vpc_network_observation
            .expect("service report exists");
        assert_eq!(retained, duplicated);
        txn.commit().await.expect("finish stale evidence read");
        fixture.assert_status(3, 0).await;

        // Fresh service reports restore propagation without any instance network-version change.
        fixture
            .report(
                first.dpu_id,
                &[first.internal_uuid, second.internal_uuid],
                &fixture.group,
            )
            .await;
        fixture.assert_status(3, 2).await;
        fixture
            .report(secondary.dpu_id, &[secondary.internal_uuid], &fixture.group)
            .await;
        fixture.assert_status(3, 3).await;

        // Change the NSG attachment while retaining service interfaces and the secondary's report.
        for group_id in [
            // No NSG must omit retained targets even without a request-level NSG filter.
            None,
            // Restore the empty NSG so its evidence and subsequent removal checks remain effective.
            Some(fixture.group.id.clone()),
        ] {
            let updated = fixture
                .env
                .api
                .update_vpc(tonic::Request::new(forge::VpcUpdateRequest {
                    id: Some(fixture.vpc_id),
                    network_security_group_id: group_id.clone(), // None removes the NSG.
                    ..Default::default()
                }))
                .await
                .expect("change service VPC NSG attachment")
                .into_inner()
                .vpc
                .expect("updated VPC");
            assert_eq!(
                updated
                    .config
                    .as_ref()
                    .expect("VPC config")
                    .network_security_group_id,
                group_id
            );

            // Find the committed VPC and retained service interfaces before observing propagation.
            let persisted = fixture
                .env
                .api
                .find_vpcs_by_ids(tonic::Request::new(forge::VpcsByIdsRequest {
                    vpc_ids: vec![fixture.vpc_id],
                }))
                .await
                .expect("reload service VPC NSG attachment")
                .into_inner()
                .vpcs
                .remove(0);
            assert_eq!(persisted.config, updated.config);
            let retained = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
                .await
                .expect("reload retained service interfaces")
                .expect("instance exists");
            assert_eq!(
                retained.config.network.service_interfaces,
                fixture.interfaces
            );

            // Restoring the attachment must reuse the secondary's applied evidence without a new
            // secondary report; primary UUIDs without NSG metadata contribute zero applied interfaces.
            if group_id.is_some() {
                fixture.assert_status(3, 1).await;

                // Matching primary policy must complete propagation after the report without policy.
                fixture
                    .report(
                        first.dpu_id,
                        &[first.internal_uuid, second.internal_uuid],
                        &fixture.group,
                    )
                    .await;
                fixture.assert_status(3, 3).await;
            } else {
                let public = fixture
                    .env
                    .api
                    .get_network_security_group_propagation_status(tonic::Request::new(
                        forge::GetNetworkSecurityGroupPropagationStatusRequest {
                            vpc_ids: vec![fixture.vpc_id.to_string()],
                            instance_ids: vec![],
                            network_security_group_ids: None, // Prevent an NSG filter from hiding targets.
                        },
                    ))
                    .await
                    .expect("get service propagation without an attached NSG")
                    .into_inner();
                assert!(public.vpcs.is_empty());
                assert!(public.instances.is_empty());

                // Record current primary UUIDs without policy while the selected VPC has no NSG.
                let report_without_policy = forge::ServiceVpcNetworkStatusObservation {
                    interfaces: [first.internal_uuid, second.internal_uuid]
                        .into_iter()
                        .map(|identity| forge::ServiceVpcInterfaceStatusObservation {
                            internal_uuid: Some(identity.into()), // Match a current primary interface.
                            network_security_group: None, // Report reconciliation without NSG policy.
                        })
                        .collect(),
                };
                fixture
                    .env
                    .api
                    .record_dpu_network_status(tonic::Request::new(forge::DpuNetworkStatus {
                        dpu_machine_id: Some(first.dpu_id),
                        service_vpc_network_observation: Some(report_without_policy.clone()),
                        dpu_health: Some(rpc::health::HealthReport {
                            source: "dpu-agent".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }))
                    .await
                    .expect("record service identities without VPC policy");

                // Read committed UUIDs and absent NSG metadata before the next iteration attaches the NSG.
                let persisted_without_policy =
                    db::machine::find_dpu_network_status_inputs(&fixture.env.pool, &first.dpu_id)
                        .await
                        .expect("reload service identities without VPC policy")
                        .expect("primary DPU exists")
                        .network_status_observation
                        .expect("primary observation exists")
                        .service_vpc_network_observation;
                assert_eq!(
                    persisted_without_policy,
                    Some(
                        ServiceVpcNetworkStatusObservation::try_from(report_without_policy)
                            .expect("valid service identities without VPC policy")
                    )
                );
            }
        }

        // Remove one shared-slot record while retaining its stale observation and the other member.
        fixture
            .replace_interfaces(&[second.clone(), secondary.clone()])
            .await;
        fixture.assert_status(2, 2).await;

        // Reuse the remaining slot with a new UUID; evidence for the retired UUID must not count.
        let mut replacement = seed_service_vpc_endpoint(
            &fixture.env,
            fixture.instance_id,
            second.dpu_id,
            second.network_segment_id,
        )
        .await;
        // Keep the synthetic link addresses distinct without changing the reused slot.
        replacement.link_prefix = "192.0.2.6/31".parse().expect("replacement service prefix");
        fixture
            .replace_interfaces(&[replacement.clone(), secondary.clone()])
            .await;
        fixture.assert_status(2, 1).await;

        // Report the replacement UUID to restore full propagation while retaining the secondary report.
        fixture
            .report(
                replacement.dpu_id,
                &[replacement.internal_uuid],
                &fixture.group,
            )
            .await;
        fixture.assert_status(2, 2).await;

        // Mark the final primary member terminating while retaining its endpoint and applied report.
        let mut txn = fixture.env.db_txn().await;
        let stored = db::instance::find_by_id(txn.as_mut(), fixture.instance_id)
            .await
            .expect("find attachment")
            .expect("instance exists");
        let mut services = stored.config.extension_services.clone();
        services
            .service_configs
            .iter_mut()
            .find(|attachment| attachment.id == Some(replacement.attachment_id))
            .expect("replacement attachment")
            .removed = Some(chrono::Utc::now());
        assert!(matches!(
            db::instance::update_extension_services_config(
                txn.as_mut(),
                fixture.instance_id,
                stored.extension_services_config_version,
                &stored.config.extension_services,
                &services,
                true,
            )
            .await
            .expect("terminate attachment"),
            db::ConditionalWrite::Applied(())
        ));
        txn.commit().await.expect("commit attachment termination");

        // Reload the committed attachment change to verify termination removes its propagation target.
        let persisted = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await
            .expect("reload termination")
            .expect("instance exists");
        assert_eq!(persisted.config.extension_services, services);
        fixture.assert_status(1, 1).await;

        // Remove all desired service interfaces while retaining their attachments and old reports.
        fixture.replace_interfaces(&[]).await;

        // Verify target removal leaves the instance live, the VPC NSG attached, and the
        // secondary DPU's service report stored.
        let retained = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await
            .expect("reload instance without service interfaces")
            .expect("instance exists");
        assert!(retained.deleted.is_none());
        let retained_vpc = fixture
            .env
            .api
            .find_vpcs_by_ids(tonic::Request::new(forge::VpcsByIdsRequest {
                vpc_ids: vec![fixture.vpc_id],
            }))
            .await
            .expect("reload service VPC")
            .into_inner()
            .vpcs
            .remove(0);
        assert_eq!(
            retained_vpc
                .config
                .expect("service VPC config")
                .network_security_group_id,
            Some(fixture.group.id.clone())
        );
        let retained_report =
            db::machine::find_dpu_network_status_inputs(&fixture.env.pool, &secondary.dpu_id)
                .await
                .expect("reload retained secondary report")
                .expect("secondary DPU exists")
                .network_status_observation
                .expect("secondary network observation exists")
                .service_vpc_network_observation
                .expect("secondary service report exists");
        assert_eq!(retained_report.interfaces.len(), 1);
        assert_eq!(
            retained_report.interfaces[0].internal_uuid,
            secondary.internal_uuid
        );
        fixture.assert_status(0, 0).await;

        // Restore the active secondary target so deletion remains an independent exclusion check.
        fixture
            .replace_interfaces(&[replacement.clone(), secondary.clone()])
            .await;
        fixture.assert_status(1, 1).await;

        // Mark only the instance deleted because public release also terminates attachments,
        // which would not independently prove deleted-instance filtering.
        let mut txn = fixture.env.db_txn().await;
        db::instance::mark_as_deleted(fixture.instance_id, txn.as_mut())
            .await
            .expect("mark instance deleted");
        txn.commit().await.expect("commit deletion");

        // Verify the deleted instance retains its secondary interface and active attachment
        // before checking that no propagation targets remain.
        let persisted = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await
            .expect("reload deleted instance")
            .expect("instance remains during cleanup");
        assert!(persisted.deleted.is_some());
        assert!(
            persisted
                .config
                .network
                .service_interfaces
                .contains(&secondary)
        );
        assert!(
            persisted
                .config
                .extension_services
                .service_configs
                .iter()
                .any(|attachment| {
                    attachment.id == Some(secondary.attachment_id) && attachment.removed.is_none()
                })
        );
        fixture.assert_status(0, 0).await;
    })
    .await;
}

/// Attributes service policy to the correct instance when two instances share a VPC, so an instance
/// with complete NSG evidence cannot hide another instance's outstanding service interfaces.
/// An explicit empty service report with a machine ACK and no instance ACK (as used by Admin
/// reports) clears only the reporting DPU's applied service evidence while active service interfaces
/// remain propagation targets.
#[crate::sqlx_test]
async fn service_vpc_propagation_attributes_unpropagated_instances(pool: sqlx::PgPool) {
    Box::pin(async move {
        // Give both instances an instance NSG so only service interfaces contribute to VPC counts.
        let fixture = ServiceVpcPropagationFixture::new(pool, true, true).await;
        let [first, second, secondary] = &fixture.interfaces;
        let other_host = create_managed_host_multi_dpu(&fixture.env, 1).await;
        let other_instance_id = other_host
            .instance_builer(&fixture.env)
            .config(forge::InstanceConfig {
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(first.network_segment_id)),
                network_security_group_id: Some(fixture.group.id.clone()), // Omit host counts.
                ..Default::default()
            })
            .build()
            .await
            .id;

        // Give the second instance one interface with its own identity and the next link prefix.
        // Reuse the selected VPC and slot zero on the second host's DPU.
        let other_interface = InstanceServiceInterfaceConfig {
            attachment_id: uuid::Uuid::new_v4(),
            dpu_id: other_host.dpu_ids[0], // Attribute this target to the second instance's DPU.
            internal_uuid: uuid::Uuid::new_v4(), // Require independent applied evidence.
            link_prefix: "192.0.2.6/31".parse().expect("other service prefix"),
            ..first.clone()
        };
        persist_service_vpc_attachment(
            &fixture.env,
            other_instance_id,
            fixture.group.tenant_organization_id.clone(),
            vec![other_interface.clone()],
        )
        .await;

        // Compare exact IDs after each report to distinguish instances with outstanding policy.
        let mut related_instance_ids = vec![fixture.instance_id, other_instance_id];
        related_instance_ids.sort();
        let assert_membership = async |applied, unpropagated_instance_id: Option<InstanceId>| {
            // Check per-instance SQL aggregation before observing its public conversion.
            let (mut vpcs, instances) = db::network_security_group::get_propagation_status(
                &mut fixture.env.db_reader(),
                None,
                None,
                Some(&[fixture.vpc_id]),
                Some(&[]),
            )
            .await
            .expect("query two-instance service propagation");
            assert_eq!(vpcs.len(), 1);
            assert!(instances.is_empty());
            vpcs[0].related_instance_ids.sort();
            assert_eq!(
                (vpcs[0].interfaces_expected, vpcs[0].interfaces_applied),
                (4, applied)
            );
            assert_eq!(vpcs[0].related_instance_ids, related_instance_ids);
            assert_eq!(
                vpcs[0].unpropagated_instance_ids,
                unpropagated_instance_id.into_iter().collect::<Vec<_>>()
            );

            // Verify public status and exact membership independently of the count fields.
            let mut public = fixture
                .env
                .api
                .get_network_security_group_propagation_status(tonic::Request::new(
                    forge::GetNetworkSecurityGroupPropagationStatusRequest {
                        vpc_ids: vec![fixture.vpc_id.to_string()],
                        instance_ids: vec![],
                        network_security_group_ids: None,
                    },
                ))
                .await
                .expect("get two-instance service propagation")
                .into_inner();
            assert_eq!(public.vpcs.len(), 1);
            public.vpcs[0].related_instance_ids.sort();
            assert_eq!(
                public.vpcs,
                vec![forge::NetworkSecurityGroupPropagationObjectStatus {
                    id: fixture.vpc_id.to_string(),
                    status: if applied == 4 {
                        forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull
                    } else {
                        forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusPartial
                    }
                    .into(),
                    details: None,
                    related_instance_ids: related_instance_ids
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    unpropagated_instance_ids: unpropagated_instance_id
                        .into_iter()
                        .map(|id| id.to_string())
                        .collect(),
                }]
            );
            assert!(public.instances.is_empty());
        };

        // Report NSG evidence for the first instance's three interfaces while the second has no report.
        fixture
            .report(
                first.dpu_id,
                &[first.internal_uuid, second.internal_uuid],
                &fixture.group,
            )
            .await;
        fixture
            .report(secondary.dpu_id, &[secondary.internal_uuid], &fixture.group)
            .await;

        // Only the second instance must remain unpropagated after the first completes.
        assert_membership(3, Some(other_instance_id)).await;

        // Report NSG evidence for the second instance's interface so neither remains unpropagated.
        fixture
            .report(
                other_interface.dpu_id,
                &[other_interface.internal_uuid],
                &fixture.group,
            )
            .await;

        // Both instances must be absent from unpropagated membership after the second completes.
        assert_membership(4, None).await;

        // Read the current machine version for the machine ACK without an instance ACK
        // used by Admin reports.
        let primary_network_version =
            db::machine::find_dpu_network_status_inputs(&fixture.env.pool, &first.dpu_id)
                .await
                .expect("find primary machine version")
                .expect("primary DPU exists")
                .network_config
                .version;

        // Clear the primary's two service observations with explicit empty evidence and a machine ACK.
        fixture
            .env
            .api
            .record_dpu_network_status(tonic::Request::new(forge::DpuNetworkStatus {
                dpu_machine_id: Some(first.dpu_id),
                observed_at: Some(std::time::SystemTime::now().into()), // Replace the previous primary report.
                network_config_version: Some(primary_network_version.to_string()), // Machine ACK.
                instance_network_config_version: None,                  // No instance ACK.
                service_vpc_network_observation: Some(
                    forge::ServiceVpcNetworkStatusObservation::default(),
                ), // Keep the empty service report present.
                dpu_health: Some(rpc::health::HealthReport {
                    source: "dpu-agent".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await
            .expect("record empty service evidence with machine ACK");

        // Reload the replacement so empty evidence and the machine ACK are proved persisted.
        let persisted =
            db::machine::find_dpu_network_status_inputs(&fixture.env.pool, &first.dpu_id)
                .await
                .expect("reload empty service evidence")
                .expect("primary DPU exists")
                .network_status_observation
                .expect("primary observation exists");
        assert_eq!(
            persisted.network_config_version,
            Some(primary_network_version)
        );
        assert!(persisted.instance_network_observation.is_none());
        assert_eq!(
            persisted.service_vpc_network_observation,
            Some(ServiceVpcNetworkStatusObservation { interfaces: vec![] })
        );

        // Retain all four targets while only the first instance loses applied service credit.
        assert_membership(2, Some(fixture.instance_id)).await;
    })
    .await;
}

/// Keeps overlapping service interfaces in VPC propagation while host interfaces use their
/// instance NSG, preventing an instance override from hiding unpropagated service policy.
#[crate::sqlx_test]
async fn service_vpc_propagation_preserves_instance_nsg_precedence(pool: sqlx::PgPool) {
    Box::pin(async move {
        // Use the same VPC for host and service interfaces, with a different NSG on the instance.
        let fixture = ServiceVpcPropagationFixture::new(pool, true, true).await;
        let [first, second, secondary] = &fixture.interfaces;

        // Verify the instance NSG leaves all three service interfaces awaiting VPC policy.
        fixture.assert_status(3, 0).await;

        // Report selected-VPC policy independently from each DPU.
        fixture
            .report(
                first.dpu_id,
                &[first.internal_uuid, second.internal_uuid],
                &fixture.group,
            )
            .await;
        fixture
            .report(secondary.dpu_id, &[secondary.internal_uuid], &fixture.group)
            .await;

        // Verify both DPU reports complete service propagation despite the instance NSG override.
        fixture.assert_status(3, 3).await;

        // Service reports must not satisfy the instance NSG's host-interface count.
        let (_, instances) = db::network_security_group::get_propagation_status(
            &mut fixture.env.db_reader(),
            None,
            None,
            Some(&[fixture.vpc_id]),
            Some(&[fixture.instance_id]),
        )
        .await
        .expect("query instance propagation");
        assert_eq!(instances.len(), 1);
        assert_eq!(
            (
                instances[0].interfaces_expected,
                instances[0].interfaces_applied
            ),
            (1, 0)
        );

        // Read the configured host UUID and instance NSG before acknowledging host policy independently.
        let stored = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await
            .expect("find host interface")
            .expect("instance exists");
        let instance_group_id = stored
            .config
            .network_security_group_id
            .expect("instance NSG");
        let host_group = fixture
            .env
            .api
            .find_network_security_groups_by_ids(tonic::Request::new(
                forge::FindNetworkSecurityGroupsByIdsRequest {
                    network_security_group_ids: vec![instance_group_id.to_string()],
                    tenant_organization_id: None,
                },
            ))
            .await
            .expect("find host NSG")
            .into_inner()
            .network_security_groups
            .remove(0);

        // A legacy host-only report acknowledges the instance NSG and clears primary service evidence.
        super::update_network_status_observation(
            &fixture.env,
            &fixture.instance_id,
            &host_group.id,
            &host_group.version,
            &first.dpu_id,
            forge::NetworkSecurityGroupSource::NsgSourceInstance,
            &stored.config.network.interfaces[0].internal_uuid.into(),
        )
        .await;
        fixture.assert_status(3, 1).await;

        // The public response must retain the same instance under both independently reported NSGs.
        let public = fixture
            .env
            .api
            .get_network_security_group_propagation_status(tonic::Request::new(
                forge::GetNetworkSecurityGroupPropagationStatusRequest {
                    vpc_ids: vec![fixture.vpc_id.to_string()],
                    instance_ids: vec![fixture.instance_id.to_string()],
                    network_security_group_ids: None,
                },
            ))
            .await
            .expect("get host and service propagation")
            .into_inner();
        assert_eq!(
            public.instances,
            vec![forge::NetworkSecurityGroupPropagationObjectStatus {
                id: fixture.instance_id.to_string(),
                status: forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusFull.into(),
                details: None,
                related_instance_ids: vec![fixture.instance_id.to_string()],
                unpropagated_instance_ids: vec![],
            }]
        );
        assert_eq!(public.vpcs.len(), 1);
        assert_eq!(
            public.vpcs[0].related_instance_ids,
            vec![fixture.instance_id.to_string()]
        );
        assert_eq!(
            public.vpcs[0].unpropagated_instance_ids,
            vec![fixture.instance_id.to_string()]
        );
    })
    .await;
}

/// Verifies a VPC counts host and service interfaces without an instance NSG and lists the
/// instance once, so combined host and service accounting cannot omit an interface type.
/// Complete service NSG evidence leaves the VPC PARTIAL and the instance unpropagated while
/// host policy evidence is missing.
/// Matching totals remain UNKNOWN when repeated host evidence hides missing service evidence,
/// keeping the affected instance visible after SQL combines both interface counts.
/// Equal host and service contributions remain additive, and complete evidence clears the
/// instance from unpropagated membership.
#[crate::sqlx_test]
async fn service_vpc_propagation_combines_host_and_service_counts(pool: sqlx::PgPool) {
    Box::pin(async move {
        // Keep host and service interfaces in one VPC without an instance NSG.
        let fixture = ServiceVpcPropagationFixture::new(pool, true, false).await;
        let [first, second, secondary] = &fixture.interfaces;
        // Count host and service targets before either DPU acknowledges policy.
        fixture.assert_status(4, 0).await;

        // Report NSG evidence for all service interfaces before either DPU reports host policy.
        fixture
            .report(
                first.dpu_id,
                &[first.internal_uuid, second.internal_uuid],
                &fixture.group,
            )
            .await;
        fixture
            .report(secondary.dpu_id, &[secondary.internal_uuid], &fixture.group)
            .await;

        // Verify the instance remains unpropagated when only host policy evidence is missing.
        fixture.assert_status(4, 3).await;

        // Read the persisted host identity needed for an independent VPC-policy acknowledgement.
        let stored = db::instance::find_by_id(&fixture.env.pool, fixture.instance_id)
            .await
            .expect("find host interface")
            .expect("instance exists");

        // Record host policy alone on the primary, retaining only the secondary's service evidence.
        super::update_network_status_observation(
            &fixture.env,
            &fixture.instance_id,
            &fixture.group.id,
            &fixture.group.version,
            &first.dpu_id,
            forge::NetworkSecurityGroupSource::NsgSourceVpc,
            &stored.config.network.interfaces[0].internal_uuid.into(),
        )
        .await;

        // Count both applied contributions while the primary service interfaces remain outstanding.
        fixture.assert_status(4, 2).await;

        // Repeat the accepted host identity while acknowledging only one primary service interface.
        // The secondary's retained service report leaves host (1, 2) and service (3, 2) counts.
        let applied_group = forge::NetworkSecurityGroupStatus {
            id: fixture.group.id.clone(), // Match the selected VPC's policy.
            version: fixture.group.version.clone(), // Keep the reported policy current.
            source: forge::NetworkSecurityGroupSource::NsgSourceVpc.into(), // Count both under VPC policy.
        };
        let combined_report = forge::DpuNetworkStatus {
            dpu_machine_id: Some(first.dpu_id), // Attribute both host copies to the primary DPU.
            observed_at: Some(std::time::SystemTime::now().into()),
            instance_network_config_version: Some(stored.network_config_version.to_string()), // Enable host observations alongside service evidence.
            interfaces: vec![
                forge::InstanceInterfaceStatusObservation {
                    internal_uuid: Some(stored.config.network.interfaces[0].internal_uuid.into()), // Repeat the configured host identity.
                    function_type: forge::InterfaceFunctionType::Physical.into(),
                    network_security_group: Some(applied_group.clone()),
                    ..Default::default()
                };
                2 // Repeated host UUIDs are accepted and can overcount the host contribution.
            ],
            service_vpc_network_observation: Some(forge::ServiceVpcNetworkStatusObservation {
                interfaces: vec![forge::ServiceVpcInterfaceStatusObservation {
                    internal_uuid: Some(first.internal_uuid.into()), // Omit evidence for the shared-slot sibling.
                    network_security_group: Some(applied_group),
                }],
            }),
            dpu_health: Some(rpc::health::HealthReport {
                source: "dpu-agent".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let expected_observation =
            model::machine::network::MachineNetworkStatusObservation::try_from(
                combined_report.clone(),
            )
            .expect("valid combined report");
        fixture
            .env
            .api
            .record_dpu_network_status(tonic::Request::new(combined_report.clone()))
            .await
            .expect("record repeated host evidence with partial service evidence");

        // Reload both observations to prove the accepted report was stored before querying SQL.
        let persisted =
            db::machine::find_dpu_network_status_inputs(&fixture.env.pool, &first.dpu_id)
                .await
                .expect("reload combined report")
                .expect("DPU exists")
                .network_status_observation
                .expect("network observation exists");
        assert_eq!(
            (
                persisted.instance_network_observation,
                persisted.service_vpc_network_observation
            ),
            (
                expected_observation.instance_network_observation,
                expected_observation.service_vpc_network_observation
            )
        );
        let (vpcs, _) = db::network_security_group::get_propagation_status(
            &mut fixture.env.db_reader(),
            None,
            None,
            Some(&[fixture.vpc_id]),
            Some(&[]),
        )
        .await
        .expect("query matching totals with unpropagated service interfaces");

        // Keep the instance unpropagated when host overcounting offsets missing service evidence.
        assert_eq!(vpcs.len(), 1);
        assert_eq!(
            (vpcs[0].interfaces_expected, vpcs[0].interfaces_applied),
            (4, 4)
        );
        assert_eq!(vpcs[0].related_instance_ids, vec![fixture.instance_id]);
        assert_eq!(vpcs[0].unpropagated_instance_ids, vec![fixture.instance_id]);

        // The public conversion must use SQL's retained membership rather than claim FULL.
        let public = fixture
            .env
            .api
            .get_network_security_group_propagation_status(tonic::Request::new(
                forge::GetNetworkSecurityGroupPropagationStatusRequest {
                    vpc_ids: vec![fixture.vpc_id.to_string()],
                    instance_ids: vec![],
                    network_security_group_ids: None,
                },
            ))
            .await
            .expect("get matching-total propagation")
            .into_inner();
        assert_eq!(
            public.vpcs,
            vec![forge::NetworkSecurityGroupPropagationObjectStatus {
                id: fixture.vpc_id.to_string(),
                status: forge::NetworkSecurityGroupPropagationStatus::NsgPropStatusUnknown.into(),
                details: Some(
                    "matching interface totals include unpropagated instances".to_string()
                ),
                related_instance_ids: vec![fixture.instance_id.to_string()],
                unpropagated_instance_ids: vec![fixture.instance_id.to_string()],
            }]
        );
        assert!(public.instances.is_empty());

        // Retain one service interface so host and service contributions have identical counts.
        fixture
            .replace_interfaces(std::slice::from_ref(first))
            .await;
        let mut complete_report = combined_report;
        complete_report.observed_at = Some(std::time::SystemTime::now().into()); // Replace the prior report.

        // Remove repeated host observations so each contribution has one applied interface.
        complete_report.interfaces.truncate(1);

        // Acknowledge the host once alongside its service interface through the public report API.
        fixture
            .env
            .api
            .record_dpu_network_status(tonic::Request::new(complete_report))
            .await
            .expect("record complete host and service evidence");

        // Equal contributions must add to two interfaces and clear unpropagated instance membership.
        fixture.assert_status(2, 2).await;
    })
    .await;
}
