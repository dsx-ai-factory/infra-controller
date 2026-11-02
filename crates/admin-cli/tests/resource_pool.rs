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

#[path = "support/nic_firmware.rs"]
mod support;

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use prost::Message as _;
use rpc::forge::{
    InspectResourcePoolsRequest, InspectResourcePoolsResponse, ListResourcePoolsRequest,
    ResourcePool, ResourcePoolBackendKind, ResourcePoolInspection, ResourcePoolStats,
    ResourcePoolStatsStatus, ResourcePools,
};
use support::{Exchange, assert_success, run_command};
use tonic::{Code, Status};

/// The command must show missing statistics as missing and omit percentages for empty pools.
#[tokio::test]
async fn resource_pool_list_displays_capacity_and_observation_status() {
    let pools = vec![
        ResourcePoolInspection {
            name: "local".to_string(),
            backend_kind: ResourcePoolBackendKind::Integrated.into(),
            remote_pool: "local".to_string(),
            min: Some("1".to_string()),
            max: Some("10".to_string()),
            stats: Some(ResourcePoolStats {
                used: 2,
                free: 8,
                // Totals must include manual values, not only automatic capacity.
                auto_assign_used: 1,
                auto_assign_free: 3,
                non_auto_assign_used: 1,
                non_auto_assign_free: 5,
            }),
            stats_status: ResourcePoolStatsStatus::Known.into(),
            observed_at: Some(std::time::SystemTime::UNIX_EPOCH.into()),
            ..Default::default()
        },
        // A provider that does not implement Stats has unknown capacity, not zero.
        ResourcePoolInspection {
            name: "remote".to_string(),
            backend_kind: ResourcePoolBackendKind::Grpc.into(),
            backend_name: Some("fabric".to_string()),
            remote_pool: "tenant-vnis".to_string(),
            stats_status: ResourcePoolStatsStatus::Unknown.into(),
            stats_message: Some("Statistics are unsupported".to_string()),
            ..Default::default()
        },
        // A failed read stays distinguishable from unsupported statistics.
        ResourcePoolInspection {
            name: "unreachable".to_string(),
            backend_kind: ResourcePoolBackendKind::Grpc.into(),
            remote_pool: "unreachable".to_string(),
            stats_status: ResourcePoolStatsStatus::Unavailable.into(),
            stats_message: Some("Provider connection failed".to_string()),
            ..Default::default()
        },
        // Known zero capacity has no percentage denominator.
        ResourcePoolInspection {
            name: "empty".to_string(),
            backend_kind: ResourcePoolBackendKind::Integrated.into(),
            remote_pool: "empty".to_string(),
            stats: Some(ResourcePoolStats::default()),
            stats_status: ResourcePoolStatsStatus::Known.into(),
            observed_at: Some(std::time::SystemTime::UNIX_EPOCH.into()),
            ..Default::default()
        },
    ];
    let output = run_command(
        &["resource-pool", "list"],
        None,
        vec![Exchange {
            method: "/forge.Forge/AdminInspectResourcePools",
            request: InspectResourcePoolsRequest {}.encode_to_vec(),
            response: Ok(InspectResourcePoolsResponse { pools }.encode_to_vec()),
        }],
    )
    .await;
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 table");
    let rows = table_rows(&stdout);
    assert_eq!(
        rows,
        [
            vec![
                "Name",
                "Backend",
                "Remote Pool",
                "Min",
                "Max",
                "Size",
                "Used",
                "Status"
            ],
            vec![
                "local",
                "Integrated",
                "local",
                "1",
                "10",
                "10",
                "2 (20%)",
                "Known"
            ],
            vec![
                "remote",
                "gRPC (fabric)",
                "tenant-vnis",
                "",
                "",
                "Unknown",
                "Unknown",
                "Unknown"
            ],
            vec![
                "unreachable",
                "gRPC",
                "unreachable",
                "",
                "",
                "Unavailable",
                "Unavailable",
                "Unavailable"
            ],
            vec!["empty", "Integrated", "empty", "", "", "0", "0", "Known"],
        ],
        "{stdout}",
    );
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostics");
    assert!(
        stderr.contains("remote: Statistics are unsupported"),
        "{stderr}"
    );
    assert!(
        stderr.contains("unreachable: Provider connection failed"),
        "{stderr}"
    );
    // Diagnostics belong on stderr so stdout remains the resource-pool table.
    assert!(!stdout.contains("Statistics are unsupported"), "{stdout}");
    assert!(!stdout.contains("Provider connection failed"), "{stdout}");
}

/// Both gRPC and old RBAC rejection paths must fall back to the original list method.
#[tokio::test]
async fn resource_pool_list_falls_back_for_servers_without_inspection() {
    let cases = [
        // The gRPC router does not recognize the new method.
        ("method is unimplemented", Code::Unimplemented),
        // Older RBAC rules reject an unknown method before it reaches the router.
        ("method is absent from RBAC", Code::PermissionDenied),
    ]
    .map(|(scenario, code)| Case {
        scenario,
        input: code,
        expect: Yields(
            [
                "old-server",
                "Integrated",
                "old-server",
                "10",
                "12",
                "3",
                "2 (67%)",
                "Known",
            ]
            .map(str::to_owned)
            .to_vec(),
        ),
    });
    check_cases_async(cases, |code| async move {
        let output = run_command(
            &["resource-pool", "list"],
            None,
            vec![
                Exchange {
                    method: "/forge.Forge/AdminInspectResourcePools",
                    request: InspectResourcePoolsRequest {}.encode_to_vec(),
                    response: Err(Status::new(code, "inspection method is unavailable")),
                },
                Exchange {
                    method: "/forge.Forge/AdminListResourcePools",
                    request: ListResourcePoolsRequest::default().encode_to_vec(),
                    response: Ok(ResourcePools {
                        pools: vec![ResourcePool {
                            name: "old-server".to_string(),
                            min: "10".to_string(),
                            max: "12".to_string(),
                            // A fractional percentage must round rather than truncate.
                            total: 3,
                            allocated: 2,
                        }],
                    }
                    .encode_to_vec()),
                },
            ],
        )
        .await;
        assert_success(&output);
        String::from_utf8(output.stdout).map(|stdout| {
            table_rows(&stdout)[1]
                .iter()
                .map(|cell| (*cell).to_owned())
                .collect()
        })
    })
    .await;
}

/// A failed inspection is not an old server; errors identify the operation that failed.
#[tokio::test]
async fn resource_pool_list_preserves_failure_context_without_unexpected_fallback() {
    let cases = [
        // FailedPrecondition comes from an implemented inspect method. No list follows.
        (
            "inspection rejects the request",
            vec![Exchange {
                method: "/forge.Forge/AdminInspectResourcePools",
                request: InspectResourcePoolsRequest {}.encode_to_vec(),
                response: Err(Status::failed_precondition("pool inspection was rejected")),
            }],
            "failed to inspect resource pools",
        ),
        // A real fallback failure must identify list, rather than hiding its cause.
        (
            "older list rejects the request",
            vec![
                Exchange {
                    method: "/forge.Forge/AdminInspectResourcePools",
                    request: InspectResourcePoolsRequest {}.encode_to_vec(),
                    response: Err(Status::unimplemented("inspection method is unavailable")),
                },
                Exchange {
                    method: "/forge.Forge/AdminListResourcePools",
                    request: ListResourcePoolsRequest::default().encode_to_vec(),
                    response: Err(Status::failed_precondition("pool inspection was rejected")),
                },
            ],
            "failed to list resource pools on a server without inspection",
        ),
    ]
    .map(|(scenario, exchanges, context)| Case {
        scenario,
        input: (exchanges, context),
        expect: Yields(()),
    });
    check_cases_async(cases, |(exchanges, context)| async move {
        let output = run_command(&["resource-pool", "list"], None, exchanges).await;
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostic");
        if output.status.success()
            || !stderr.contains(context)
            || !stderr.contains("pool inspection was rejected")
        {
            return Err(format!("exit status {}: {stderr}", output.status));
        }
        Ok(())
    })
    .await;
}

/// An empty result keeps the command's existing message and unsuccessful exit status.
#[tokio::test]
async fn resource_pool_list_reports_no_defined_pools() {
    let output = run_command(
        &["resource-pool", "list"],
        None,
        vec![Exchange {
            method: "/forge.Forge/AdminInspectResourcePools",
            request: InspectResourcePoolsRequest {}.encode_to_vec(),
            response: Ok(InspectResourcePoolsResponse::default().encode_to_vec()),
        }],
    )
    .await;
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 output"),
        "No resource pools defined\n",
    );
}

/// `table_rows` keeps empty cells when reading the command's actual table output.
fn table_rows(output: &str) -> Vec<Vec<&str>> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect())
        .collect()
}
