/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

#[path = "support/nic_firmware.rs"]
mod support;

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType, StableHostMachineId};
use prost::Message as _;
use rpc::{DiscoveryInfo, NetworkInterface, PciDeviceProperties, common, forge};
use support::{CommandOptions, Exchange, assert_success, run_command, run_command_with_options};
use tonic::Status;

const SERVICE: &str = "12345678-1234-5678-90ab-cdef01234567";
const OTHER_SERVICE: &str = "abcdef01-2345-6789-abcd-ef0123456789";
const INSTANCE: &str = "23072a44-0eb6-4867-9c2d-c65130b7134c";
const VPC: &str = "d7858ee8-5d42-4fcb-8774-a3fac4131911";
const ATTACHMENT: &str = "02044099-0387-4dee-b092-23b3750bb7b7";
const VERSION: &str = "V1-T1720000000000000";
const TENANT: &str = "fds34511233a";
const DATA: &str = r#"{"repoURL":"oci://registry.example.com/charts","chartName":"my-service","chartVersion":"2.0.0","serviceID":"my-service","deployInCluster":false,"security":{"privileged":false}}"#;
const DEFERRED: &str =
    "service VPC Ipv6 registration is not implemented; retrying after configuration rollout";

/// Reads a public ASCII table into trimmed cells so assertions include empty columns.
fn table_rows(output: &str) -> Vec<Vec<&str>> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect())
        .collect()
}

/// Verifies create sends either family or an empty list and show renders a scripted Creating
/// reason, so the CLI preserves the wire requirements and exposes deferred registration to operators.
#[tokio::test]
async fn registration_declares_family_and_shows_deferred_reason() {
    for (flag, family) in [
        // IPv4 registers the initially supported service address family.
        (Some("ipv4"), Some(forge::ServiceVpcAddressFamily::Ipv4)),
        // IPv6 is a valid declaration even though allocation may be unavailable.
        (Some("ipv6"), Some(forge::ServiceVpcAddressFamily::Ipv6)),
        // Omitting the option keeps established endpoint-free registration behavior.
        (None, None),
    ] {
        // Keep the Helm definition fixed so only the declared family changes on the wire.
        let mut args = vec![
            "extension-service",
            "create",
            "--id",
            SERVICE,
            "--name",
            "my-service",
            "--tenant-organization-id",
            TENANT,
            "--type",
            "dpf-helm-chart",
            "--dpu-target",
            "primary",
            "--data",
            DATA,
        ];
        // Include the optional family flag only for a registration that declares an interface.
        if let Some(flag) = flag {
            args.extend(["--service-vpc-family", flag]);
        }
        let interfaces = family
            .map(|family| forge::ServiceVpcInterfaceRequirement {
                address_family: family.into(),
            })
            .into_iter()
            .collect();

        // Expect a demand-driven Helm request with its required DPU target and the case's family.
        let create = Exchange {
            method: rpc::service_path!("CreateDpuExtensionService"),
            request: forge::CreateDpuExtensionServiceRequest {
                service_id: Some(SERVICE.into()),
                service_name: "my-service".into(),
                service_type: forge::DpuExtensionServiceType::DpfHelmChart.into(),
                tenant_organization_id: TENANT.into(),
                data: DATA.into(),
                dpu_target: Some(forge::DpuExtensionServiceDpuTarget::Primary.into()),
                service_vpc_interfaces: interfaces,
                ..Default::default()
            }
            .encode_to_vec(),
            response: Ok(forge::DpuExtensionService::default().encode_to_vec()),
        };

        // Run create against the scripted registration exchange.
        let output = run_command(&args, None, vec![create]).await;

        // The harness matches the request and the CLI must exit successfully.
        assert_success(&output);
    }

    // Script a separate show response with a deferred IPv6 registration and a plain registration.
    let exchanges = vec![
        Exchange {
            method: rpc::service_path!("FindDpuExtensionServiceIds"),
            request: forge::DpuExtensionServiceSearchFilter::default().encode_to_vec(),
            response: Ok(forge::DpuExtensionServiceIdList {
                service_ids: vec![SERVICE.into(), OTHER_SERVICE.into()],
            }
            .encode_to_vec()),
        },
        Exchange {
            method: rpc::service_path!("FindDpuExtensionServicesByIds"),
            request: forge::DpuExtensionServicesByIdsRequest {
                service_ids: vec![SERVICE.into(), OTHER_SERVICE.into()],
            }
            .encode_to_vec(),
            response: Ok(forge::DpuExtensionServiceList {
                services: vec![
                    forge::DpuExtensionService {
                        service_id: SERVICE.into(),
                        service_vpc_interfaces: vec![forge::ServiceVpcInterfaceRequirement {
                            address_family: forge::ServiceVpcAddressFamily::Ipv6.into(),
                        }], // This row includes an IPv6 requirement.
                        lifecycle_status: Some(forge::LifecycleStatus {
                            state: r#"{"state":"creating"}"#.into(), // State reported for the deferred registration.
                            state_reason: Some(forge::ControllerStateReason {
                                outcome_msg: Some(DEFERRED.into()), // Reason returned by the mock.
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    forge::DpuExtensionService {
                        service_id: OTHER_SERVICE.into(),
                        ..Default::default() // The plain registration supplies neither a family nor a reason.
                    },
                ],
            }
            .encode_to_vec()),
        },
    ];

    // Run show against the scripted list and details response.
    let output = run_command_with_options(
        &["extension-service", "show"],
        None,
        exchanges,
        CommandOptions {
            max_find_by_ids: 2,
            ..Default::default()
        },
    )
    .await;

    // Assert the header and both value shapes, including an empty reason for the plain service.
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("registration table is UTF-8");
    let rows = table_rows(&stdout);
    let selected_cells =
        |row: &[&str]| [row[8].to_string(), row[11].to_string(), row[12].to_string()];
    assert_eq!(
        selected_cells(&rows[0]),
        [
            "Lifecycle State",
            "Service VPC Interface Families",
            "Lifecycle Reason"
        ]
    );
    assert_eq!(selected_cells(&rows[1]), ["creating", "ipv6", DEFERRED]);
    assert_eq!(selected_cells(&rows[2]), ["Unknown", "", ""]);
}

/// Verifies registration updates send the complete family vector and fence copied requirements
/// with the observed version counter, so Core can reject the write if that counter has advanced.
#[tokio::test]
async fn registration_update_sends_complete_service_vpc_requirements() {
    let requirement =
        |family: forge::ServiceVpcAddressFamily| forge::ServiceVpcInterfaceRequirement {
            address_family: family.into(),
        };
    for (flags, preserve, interfaces) in [
        // An explicit family replaces the complete registration requirement.
        (
            &["--service-vpc-family", "ipv6"][..],
            false,
            vec![requirement(forge::ServiceVpcAddressFamily::Ipv6)],
        ),
        // Clearing sends a present empty message so Core can remove the requirement.
        (&["--clear-service-vpc"][..], false, vec![]),
        // An ordinary revision resubmits the observed family with the observed counter.
        (
            &[][..],
            true,
            vec![requirement(forge::ServiceVpcAddressFamily::Ipv4)],
        ),
    ] {
        // The registration lookup is needed only when preserving the stored family.
        let mut exchanges = Vec::new();
        if preserve {
            exchanges.push(Exchange {
                method: rpc::service_path!("FindDpuExtensionServicesByIds"),
                request: forge::DpuExtensionServicesByIdsRequest {
                    service_ids: vec![SERVICE.into()],
                }
                .encode_to_vec(),
                response: Ok(forge::DpuExtensionServiceList {
                    services: vec![forge::DpuExtensionService {
                        service_id: SERVICE.into(),
                        service_type: forge::DpuExtensionServiceType::DpfHelmChart.into(),
                        active_versions: vec![VERSION.into()], // The live Helm registration retains V1.
                        version_ctr: 7, // Stored version_ctr returned by the registration lookup.
                        service_vpc_interfaces: vec![requirement(
                            forge::ServiceVpcAddressFamily::Ipv4,
                        )], // The mock registration already declares IPv4.
                        ..Default::default()
                    }],
                }
                .encode_to_vec()),
            });
        }
        // Script this case's complete definition, including the presence of the family wrapper.
        exchanges.push(Exchange {
            method: rpc::service_path!("UpdateDpuExtensionService"),
            request: forge::UpdateDpuExtensionServiceRequest {
                service_id: SERVICE.into(),
                data: DATA.into(),
                if_version_ctr_match: preserve.then_some(7), // Fence the copied family with the observed version_ctr.
                service_vpc_interfaces: Some(forge::ServiceVpcInterfaceRequirements { interfaces }), // Keep the wrapper present when clearing.
                // Sending an empty observability message would reject this Helm update.
                observability: None,
                ..Default::default()
            }
            .encode_to_vec(),
            response: Ok(forge::DpuExtensionService::default().encode_to_vec()),
        });

        // Keep chart data fixed while changing only the family's command-line options.
        let mut args = vec![
            "extension-service",
            "update",
            "--id",
            SERVICE,
            "--data",
            DATA,
        ];
        args.extend(flags);

        // Run the public update against the scripted exchanges.
        let output = run_command(&args, None, exchanges).await;

        // The harness matches the wrapper and version-counter fields. The command must succeed.
        assert_success(&output);
    }
}

/// Verifies preserve-mode updates report a retired Helm registration as missing even with
/// a stale counter. A live Failed registration still reports the counter mismatch.
#[tokio::test]
async fn registration_update_checks_retired_helm_before_version_counter() {
    for (active_versions, diagnostic) in [
        // Deletion intent retires V1 before cleanup can put the retained snapshot in Failed.
        (vec![], "extension service not found"),
        // A failed live registration still has V1 and rejects a mismatched counter.
        (
            vec![VERSION.into()],
            "extension service version counter is 7, expected 6",
        ),
    ] {
        // Both results report Failed with the same version_ctr so only the active version differs.
        let exchanges = vec![Exchange {
            method: rpc::service_path!("FindDpuExtensionServicesByIds"),
            request: forge::DpuExtensionServicesByIdsRequest {
                service_ids: vec![SERVICE.into()],
            }
            .encode_to_vec(),
            response: Ok(forge::DpuExtensionServiceList {
                services: vec![forge::DpuExtensionService {
                    service_id: SERVICE.into(),
                    service_type: forge::DpuExtensionServiceType::DpfHelmChart.into(),
                    version_ctr: 7,  // The caller's observed counter is stale.
                    active_versions, // Retiring Helm registrations have no active V1.
                    lifecycle_status: Some(forge::LifecycleStatus {
                        state: r#"{"state":"failed"}"#.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }
            .encode_to_vec()),
        }];

        // Omit the family flags so the update reads the registration with a stale caller counter.
        let output = run_command(
            &[
                "extension-service",
                "update",
                "--id",
                SERVICE,
                "--data",
                DATA,
                "--if-version-ctr-match",
                "6",
            ],
            None,
            exchanges,
        )
        .await;

        // The CLI rejects both before mutation and identifies why each update cannot proceed.
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8(output.stderr).expect("update diagnostic is UTF-8");
        assert!(stderr.contains(diagnostic), "{stderr}");
    }
}

/// Verifies malformed local update fields fail before the preserve lookup so a Core error
/// cannot hide an invalid credential or observability definition.
#[tokio::test]
async fn registration_update_validates_local_fields_before_reading_core() {
    for (flags, diagnostic) in [
        // Incomplete credentials must fail before the lookup, even though no family change was requested.
        (
            &["--username", "test-operator"][..],
            "All of username, password and registry URL are required",
        ),
        // Malformed observability JSON must fail before the lookup for the same preserve mode.
        (&["--observability", "["][..], "error while handling json"),
    ] {
        // Omit family options to exercise the path that would otherwise read the existing registration.
        let mut args = vec![
            "extension-service",
            "update",
            "--id",
            SERVICE,
            "--data",
            DATA,
        ];
        args.extend(flags);

        // Run update with no registration RPCs in the script.
        let output = run_command(&args, None, vec![]).await;

        // Confirm the CLI reports the local validation error after the harness checks the requests.
        let stderr = String::from_utf8(output.stderr).expect("update diagnostic is UTF-8");
        assert_eq!(output.status.code(), Some(1), "{stderr}");
        assert!(stderr.contains(diagnostic), "{stderr}");
    }
}

/// Verifies replacement and detach send the complete instance config with its observed version,
/// so the request preserves unrelated tenant fields and lets Core check the version it read.
#[tokio::test]
async fn instance_update_replaces_or_detaches_extension_services() {
    let networked_selection = format!(
        r#"{{"service_id":"{SERVICE}","version":"{VERSION}","service_vpc_ids":["{VPC}"]}}"#
    );
    let selection_without_vpc = format!(r#"{{"service_id":"{SERVICE}","version":"{VERSION}"}}"#);
    let instance_id = INSTANCE.parse().expect("valid instance ID");

    // Give replacement a networked selection distinct from the instance's old attachment.
    let selected = forge::InstanceDpuExtensionServiceConfig {
        service_id: SERVICE.into(),
        version: VERSION.into(),
        service_vpc_ids: vec![VPC.parse().expect("valid VPC ID")], // VPC selected by the replacement request.
    };
    for (flags, desired) in [
        // A replacement submits only the requested service, allowing Core to terminate omissions.
        (
            &["--extension-service", &networked_selection][..],
            vec![selected.clone()],
        ),
        // Omitting service_vpc_ids submits an empty selection for an endpoint-free attachment.
        (
            &["--extension-service", &selection_without_vpc][..],
            vec![forge::InstanceDpuExtensionServiceConfig {
                service_vpc_ids: vec![],
                ..selected
            }],
        ),
        // Detach explicitly submits an empty service list.
        (&["--detach-all"][..], vec![]),
    ] {
        // Return a complete instance config so the request can show whether only the service list changes.
        let mut config = forge::InstanceConfig {
            tenant: Some(forge::TenantConfig {
                tenant_organization_id: TENANT.into(), // Unrelated tenant data carried across both updates.
                ..Default::default()
            }),
            dpu_extension_services: Some(forge::InstanceDpuExtensionServicesConfig {
                service_configs: vec![forge::InstanceDpuExtensionServiceConfig {
                    service_id: OTHER_SERVICE.into(), // This existing service is omitted from both replacements.
                    version: VERSION.into(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        };
        let readback = forge::InstanceList {
            instances: vec![forge::Instance {
                id: Some(instance_id),
                config_version: VERSION.into(), // Version observed with the instance config.
                config: Some(config.clone()),   // Snapshot returned by the mocked read.
                metadata: Some(forge::Metadata::default()), // Present metadata exercises CLI audit labels.
                ..Default::default()
            }],
        };

        // Build the expected whole-config write by changing only the service list in that snapshot.
        config.dpu_extension_services = Some(forge::InstanceDpuExtensionServicesConfig {
            service_configs: desired,
        });
        let mut args = vec![
            "--cloud-unsafe-op",
            "service-vpc-test",
            "instance",
            "update-extension-services",
            "--instance",
            INSTANCE,
        ];
        args.extend(flags);

        // Run the replacement or detach command against the instance read and expected write.
        let output = run_command(
            &args,
            None,
            vec![
                Exchange {
                    method: rpc::service_path!("FindInstancesByIds"),
                    request: forge::InstancesByIdsRequest {
                        instance_ids: vec![instance_id],
                    }
                    .encode_to_vec(),
                    response: Ok(readback.encode_to_vec()),
                },
                Exchange {
                    method: rpc::service_path!("UpdateInstanceConfig"),
                    request: forge::InstanceConfigUpdateRequest {
                        instance_id: Some(instance_id),
                        if_version_match: Some(VERSION.into()),
                        config: Some(config),
                        metadata: Some(forge::Metadata {
                            labels: vec![
                                forge::Label {
                                    key: "cloud-unsafe-op".into(),
                                    value: None,
                                },
                                forge::Label {
                                    key: "admin-cli-last-modified-by".into(),
                                    value: Some("service-vpc-test".into()),
                                },
                            ],
                            ..Default::default()
                        }),
                    }
                    .encode_to_vec(),
                    response: Ok(forge::Instance::default().encode_to_vec()),
                },
            ],
        )
        .await;
        // The harness matches the full request. Both replacement modes must exit successfully.
        assert_success(&output);
    }
}

/// Verifies allocation sends the service VPC separately from the tenant prefix and surfaces a
/// precondition failure, so an unavailable service network cannot appear to allocate successfully.
#[tokio::test]
async fn allocation_sends_service_vpc_and_surfaces_failed_precondition() {
    let machine_id = MachineId::new(MachineIdSource::Tpm, [1; 32], MachineType::Host);
    let prefix = INSTANCE.parse().expect("valid VPC prefix ID");
    let selection = format!(
        r#"{{"service_id":"{SERVICE}","version":"{VERSION}","service_vpc_ids":["{VPC}"]}}"#
    );
    let diagnostic = "service VPC attachment is unavailable until network resource reconciliation is implemented";

    // Script machine discovery with one eligible host and an allocation precondition failure.
    let exchanges = vec![
        Exchange {
            method: rpc::service_path!("FindMachinesByIds"),
            request: forge::MachinesByIdsRequest {
                machine_ids: vec![machine_id],
                ..Default::default()
            }
            .encode_to_vec(),
            response: Ok(forge::MachineList {
                machines: vec![forge::Machine {
                    id: Some(machine_id),
                    state: "Ready".into(), // Candidate selection can use this host.
                    status: Some(forge::MachineStatus {
                        discovery_info: Some(DiscoveryInfo {
                            network_interfaces: vec![NetworkInterface {
                                pci_properties: Some(PciDeviceProperties {
                                    vendor: "Mellanox".into(),    // The PF qualifies as a DPU interface.
                                    device: "BlueField-3".into(), // The discovered device is carried into the PF request.
                                    ..Default::default()
                                }),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
            }
            .encode_to_vec()),
        },
        Exchange {
            method: rpc::service_path!("AllocateInstance"),
            request: forge::InstanceAllocationRequest {
                machine_id: Some(
                    StableHostMachineId::try_from(machine_id).expect("stable host ID"),
                ),
                config: Some(forge::InstanceConfig {
                    tenant: Some(forge::TenantConfig {
                        tenant_organization_id: TENANT.into(),
                        ..Default::default()
                    }),
                    network: Some(forge::InstanceNetworkConfig {
                        interfaces: vec![forge::InstanceInterfaceConfig {
                            function_type: forge::InterfaceFunctionType::Physical.into(),
                            network_details: Some(
                                forge::instance_interface_config::NetworkDetails::VpcPrefixId(
                                    prefix,
                                ),
                            ),
                            device: Some("BlueField-3".into()),
                            ..Default::default()
                        }], // The tenant PF still selects its own VPC prefix.
                        ..Default::default()
                    }),
                    dpu_extension_services: Some(forge::InstanceDpuExtensionServicesConfig {
                        service_configs: vec![forge::InstanceDpuExtensionServiceConfig {
                            service_id: SERVICE.into(),
                            version: VERSION.into(),
                            service_vpc_ids: vec![VPC.parse().expect("valid service VPC ID")],
                        }],
                    }), // The service selects the VPC ID independently of the tenant PF.
                    ..Default::default()
                }),
                metadata: Some(forge::Metadata {
                    name: "eth0_0".into(),
                    description: "instance created from admin-cli".into(),
                    labels: vec![
                        forge::Label {
                            key: "cloud-unsafe-op".into(),
                            value: None,
                        },
                        forge::Label {
                            key: "admin-cli-last-modified-by".into(),
                            value: Some("service-vpc-test".into()),
                        },
                    ],
                }),
                ..Default::default()
            }
            .encode_to_vec(),
            response: Err(Status::failed_precondition(diagnostic)),
        },
    ];

    // Run service-bearing allocation against the scripted Core responses.
    let output = run_command(
        &[
            "--cloud-unsafe-op",
            "service-vpc-test",
            "instance",
            "allocate",
            "--prefix-name",
            "eth0",
            "--machine-id",
            &machine_id.to_string(),
            "--tenant-org",
            TENANT,
            "--vpc-prefix-id",
            INSTANCE,
            "--extension-service",
            &selection,
        ],
        None,
        exchanges,
    )
    .await;

    // Check the unsuccessful exit and the status description and message from Core.
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr).expect("allocation diagnostic is UTF-8");
    assert!(
        stderr.contains(tonic::Code::FailedPrecondition.description()),
        "{stderr}"
    );
    assert!(stderr.contains(diagnostic), "{stderr}");
}

/// Verifies service-centric inspection shows a terminating attachment's VPC and ID so operators
/// can correlate cleanup, while a legacy attachment without either value keeps both cells empty.
#[tokio::test]
async fn show_instances_renders_retained_vpc_and_attachment_id() {
    // Give the terminating attachment both correlation fields and the legacy attachment neither.
    let response = forge::FindInstancesByDpuExtensionServiceResponse {
        instances: vec![
            forge::InstanceDpuExtensionServiceInfo {
                instance_id: INSTANCE.into(),
                service_id: SERVICE.into(),
                version: VERSION.into(),
                removed: Some("2026-11-20T15:00:00Z".into()), // Cleanup is still in progress.
                attachment_id: Some(common::Uuid {
                    value: ATTACHMENT.into(), // ID of the attachment undergoing cleanup.
                }),
                service_vpc_ids: vec![VPC.parse().expect("valid service VPC ID")], // VPC selection retained during cleanup.
            },
            forge::InstanceDpuExtensionServiceInfo {
                instance_id: OTHER_SERVICE.into(),
                service_id: SERVICE.into(),
                version: VERSION.into(),
                ..Default::default() // The legacy attachment supplies no ID or VPC.
            },
        ],
    };

    // Run the service-centric inspection command against the two attachment rows.
    let output = run_command(
        &[
            "extension-service",
            "show-instances",
            "--service-id",
            SERVICE,
        ],
        None,
        vec![Exchange {
            method: rpc::service_path!("FindInstancesByDpuExtensionService"),
            request: forge::FindInstancesByDpuExtensionServiceRequest {
                service_id: SERVICE.into(),
                version: None,
            }
            .encode_to_vec(),
            response: Ok(response.encode_to_vec()),
        }],
    )
    .await;

    // Verify all headers and cells through the public CLI, including meaningful blanks.
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("attachment table is UTF-8");
    assert_eq!(
        table_rows(&stdout),
        [
            vec![
                "Instance ID",
                "Service ID",
                "Version",
                "Config Status",
                "Service VPC IDs",
                "Attachment ID"
            ],
            vec![INSTANCE, SERVICE, VERSION, "Removing", VPC, ATTACHMENT],
            vec![OTHER_SERVICE, SERVICE, VERSION, "Active", "", ""],
        ]
    );
}

/// Verifies a released endpoint-free attachment appears once as Removing while its last deployment
/// status is RUNNING, so operators can distinguish cleanup from an active deployment.
#[tokio::test]
async fn instance_show_renders_released_attachment_once() {
    let instance_id = INSTANCE.parse().expect("valid instance ID");

    // Model release with Core retaining the desired endpoint-free attachment and marking its status removed.
    let instance = forge::Instance {
        id: Some(instance_id),
        config: Some(forge::InstanceConfig {
            dpu_extension_services: Some(forge::InstanceDpuExtensionServicesConfig {
                service_configs: vec![forge::InstanceDpuExtensionServiceConfig {
                    service_id: SERVICE.into(), // Matches the reported attachment below.
                    version: VERSION.into(),    // The same version still appears in both lists.
                    service_vpc_ids: vec![],    // Release retains this endpoint-free config.
                }],
            }),
            ..Default::default()
        }),
        status: Some(forge::InstanceStatus {
            dpu_extension_services: Some(forge::InstanceDpuExtensionServicesStatus {
                dpu_extension_services: vec![forge::InstanceDpuExtensionServiceStatus {
                    service_id: SERVICE.into(),
                    version: VERSION.into(),
                    // The last observed deployment still reports Running during release.
                    deployment_status:
                        forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning
                            .into(),
                    attachment_id: Some(common::Uuid {
                        value: ATTACHMENT.into(),
                    }),
                    removed: Some("2026-11-20 15:00:00 UTC".into()), // Core synthesizes the removal marker for release.
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Render the instance detail from the scripted Core read.
    let output = run_command(
        &["instance", "show", INSTANCE],
        None,
        vec![Exchange {
            method: rpc::service_path!("FindInstancesByIds"),
            request: forge::InstancesByIdsRequest {
                instance_ids: vec![instance_id],
            }
            .encode_to_vec(),
            response: Ok(forge::InstanceList {
                instances: vec![instance],
            }
            .encode_to_vec()),
        }],
    )
    .await;

    // Confirm the only row preserves the deployment status and identifies the attachment as Removing.
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("instance detail is UTF-8");
    let (_, services) = stdout
        .split_once("EXTENSION SERVICES:")
        .expect("extension services section");
    assert_eq!(
        table_rows(services),
        [
            vec![
                "Service ID",
                "Version",
                "Service VPC IDs",
                "Attachment ID",
                "Deployment Status",
                "Config Status"
            ],
            vec![SERVICE, VERSION, "", ATTACHMENT, "RUNNING", "Removing"],
        ]
    );
}

/// Verifies instance detail pairs a desired VPC with its active attachment and renders an empty
/// VPC for an unrelated terminating attachment, so service selections stay distinct from host interfaces.
#[tokio::test]
async fn instance_show_correlates_service_vpc_with_active_attachment() {
    let instance_id = INSTANCE.parse().expect("valid instance ID");
    let old_attachment_id = "9d0fd398-bf70-45ea-8e83-2d29a850bd37";

    // Report an active networked attachment and a different service that is terminating.
    let instance = forge::Instance {
        id: Some(instance_id),
        config: Some(forge::InstanceConfig {
            dpu_extension_services: Some(forge::InstanceDpuExtensionServicesConfig {
                service_configs: vec![forge::InstanceDpuExtensionServiceConfig {
                    service_id: SERVICE.into(), // Matches the active status row.
                    version: VERSION.into(), // Completes the service/version key shared with status.
                    service_vpc_ids: vec![VPC.parse().expect("valid service VPC")], // VPC selected by the active config.
                }],
            }),
            ..Default::default()
        }),
        status: Some(forge::InstanceStatus {
            dpu_extension_services: Some(forge::InstanceDpuExtensionServicesStatus {
                dpu_extension_services: vec![
                    forge::InstanceDpuExtensionServiceStatus {
                        service_id: OTHER_SERVICE.into(),
                        version: VERSION.into(),
                        deployment_status: forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminating.into(),
                        attachment_id: Some(common::Uuid { value: old_attachment_id.into() }), // ID of the unrelated terminating attachment.
                        removed: Some("2026-11-20T15:00:00Z".into()), // Marks this status as removed.
                        ..Default::default()
                    },
                    forge::InstanceDpuExtensionServiceStatus {
                        service_id: SERVICE.into(),
                        version: VERSION.into(),
                        deployment_status: forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning.into(),
                        attachment_id: Some(common::Uuid { value: ATTACHMENT.into() }), // ID reported for the active service/version.
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Render the existing instance detail command using only its ordinary instance read.
    let output = run_command(
        &["instance", "show", INSTANCE],
        None,
        vec![Exchange {
            method: rpc::service_path!("FindInstancesByIds"),
            request: forge::InstancesByIdsRequest {
                instance_ids: vec![instance_id],
            }
            .encode_to_vec(),
            response: Ok(forge::InstanceList {
                instances: vec![instance],
            }
            .encode_to_vec()),
        }],
    )
    .await;

    // Check that the selected VPC appears alongside Running and the active ID outside the host interfaces.
    // The unrelated terminating row has no VPC selection.
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("instance detail is UTF-8");
    let (instance, services) = stdout
        .split_once("EXTENSION SERVICES:")
        .expect("extension services section");
    assert!(!instance.contains(VPC), "{instance}");
    assert_eq!(
        table_rows(services),
        [
            vec![
                "Service ID",
                "Version",
                "Service VPC IDs",
                "Attachment ID",
                "Deployment Status",
                "Config Status"
            ],
            vec![
                OTHER_SERVICE,
                VERSION,
                "",
                old_attachment_id,
                "TERMINATING",
                "Removing"
            ],
            vec![SERVICE, VERSION, VPC, ATTACHMENT, "RUNNING", "Active"],
        ]
    );
}
