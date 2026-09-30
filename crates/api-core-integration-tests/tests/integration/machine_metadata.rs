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

use carbide_api_core::CarbideError;
use carbide_api_core::test_support::metadata::invalid_metadata_testcases;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;

#[sqlx_test]
async fn test_machine_metadata(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool)
        .with_resource_pools(ResourcePoolBuilder::default().build())
        .build()
        .await;
    let network_controller = env.network_controller();
    let domain = env.test_domain().await;
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let mut mh = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .build()
        .await
        .0;
    mh.host.discover_primary_iface(admin_segment).await;

    let host_machine = mh.host.rpc_machine().await;
    let version1: config_version::ConfigVersion = host_machine.version.parse().unwrap();

    let expected_metadata = rpc::forge::Metadata {
        name: host_machine.id.as_ref().unwrap().to_string(),
        description: "".to_string(),
        labels: Vec::new(),
    };
    assert_eq!(host_machine.metadata.as_ref().unwrap(), &expected_metadata);

    let new_metadata = rpc::forge::Metadata {
        name: "ASDF".to_string(),
        description: "LL1".to_string(),
        labels: vec![
            rpc::forge::Label {
                key: "A".to_string(),
                value: None,
            },
            rpc::forge::Label {
                key: "B".to_string(),
                value: Some("BB".to_string()),
            },
        ],
    };

    // Update with missing Metadata fails
    let err = env
        .api()
        .update_machine_metadata(tonic::Request::new(
            rpc::forge::MachineMetadataUpdateRequest {
                machine_id: host_machine.id,
                if_version_match: None,
                metadata: None,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);

    // Update succeeds
    env.api()
        .update_machine_metadata(tonic::Request::new(
            rpc::forge::MachineMetadataUpdateRequest {
                machine_id: host_machine.id,
                if_version_match: None,
                metadata: Some(new_metadata.clone()),
            },
        ))
        .await
        .unwrap();

    let mut host_machine = mh.host.rpc_machine().await;
    let version2: config_version::ConfigVersion = host_machine.version.parse().unwrap();
    assert_eq!(version2.version_nr(), version1.version_nr() + 1);
    host_machine
        .metadata
        .as_mut()
        .unwrap()
        .labels
        .sort_by(|l1, l2| l1.key.cmp(&l2.key));

    assert_eq!(host_machine.metadata.as_ref().unwrap(), &new_metadata);

    // Conditional updates
    let new_metadata = rpc::forge::Metadata {
        name: "CONDITIONAL".to_string(),
        description: "".to_string(),
        labels: vec![rpc::forge::Label {
            key: "D".to_string(),
            value: None,
        }],
    };

    let err = env
        .api()
        .update_machine_metadata(tonic::Request::new(
            rpc::forge::MachineMetadataUpdateRequest {
                machine_id: host_machine.id,
                if_version_match: Some(version1.to_string()),
                metadata: Some(new_metadata.clone()),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        err.message(),
        CarbideError::ConcurrentModificationError("machine", version1.to_string()).to_string()
    );

    env.api()
        .update_machine_metadata(tonic::Request::new(
            rpc::forge::MachineMetadataUpdateRequest {
                machine_id: host_machine.id,
                if_version_match: Some(version2.to_string()),
                metadata: Some(new_metadata.clone()),
            },
        ))
        .await
        .unwrap();

    let mut host_machine = mh.host.rpc_machine().await;
    let version3: config_version::ConfigVersion = host_machine.version.parse().unwrap();
    assert_eq!(version3.version_nr(), version2.version_nr() + 1);
    host_machine
        .metadata
        .as_mut()
        .unwrap()
        .labels
        .sort_by(|l1, l2| l1.key.cmp(&l2.key));

    assert_eq!(host_machine.metadata.as_ref().unwrap(), &new_metadata);

    // Updates with invalid metadata fail
    for (invalid_metadata, expected_err) in invalid_metadata_testcases(true) {
        let err = env
            .api()
            .update_machine_metadata(tonic::Request::new(
                rpc::forge::MachineMetadataUpdateRequest {
                    machine_id: host_machine.id,
                    if_version_match: None,
                    metadata: Some(invalid_metadata.clone()),
                },
            ))
            .await
            .expect_err(&format!(
                "Invalid metadata of type should not be accepted: {invalid_metadata:?}"
            ));
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains(&expected_err),
            "Testcase: {:?}\nMessage is \"{}\".\nMessage should contain: \"{}\"",
            invalid_metadata,
            err.message(),
            expected_err
        );
    }

    Ok(())
}
