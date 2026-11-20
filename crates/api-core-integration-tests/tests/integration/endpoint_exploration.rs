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

use std::net::IpAddr;

use carbide_test_harness::MockEndpointExplorer;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::site_explorer::{EndpointExplorationError, ExploredEndpoint};
use model::test_support::ManagedHostConfig;
use tonic::Request;

#[sqlx_test]
async fn test_manual_exploration_preserves_endpoint_results_when_sitewide_password_loading_fails(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let endpoint_explorer = MockEndpointExplorer::default();
    let env = TestHarness::builder(pool)
        .with_api_builder_fn({
            let endpoint_explorer = endpoint_explorer.clone();
            move |builder| builder.with_endpoint_explorer(endpoint_explorer)
        })
        .build()
        .await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let (mut mh, build_data) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default())
        .with_dpu_primary_interfaces(admin_segment)
        .with_dpu_network_status_reported()
        .build()
        .await;
    mh.host.discover_primary_iface(admin_segment).await;
    mh.advance_to_converged_ready().await;
    let bmc_ip = build_data.host_bmc_ip();
    let initial = explored_endpoint(&env, bmc_ip).await?;
    let load_error = EndpointExplorationError::MissingCredentials {
        key: "machines/bmc/site/root".to_string(),
        cause: "site-wide password unavailable".to_string(),
    };
    endpoint_explorer.set_sitewide_bmc_password_result(Err(load_error.clone()));

    for (scenario, endpoint_result) in [
        ("stored BMC credentials", Ok(initial.report.clone())),
        ("site-wide password required", Err(load_error)),
    ] {
        let expected_error = endpoint_result.as_ref().err().cloned();
        endpoint_explorer.insert_endpoint_result(bmc_ip, endpoint_result);
        let baseline = explored_endpoint(&env, bmc_ip).await?;
        let calls_before = endpoint_explorer.explore_endpoint_call_count();
        let response = env
            .api()
            .refresh_endpoint_report(Request::new(rpc::forge::RefreshEndpointReportRequest {
                ip_address: bmc_ip.to_string(),
            }))
            .await?
            .into_inner();
        let refreshed = explored_endpoint(&env, bmc_ip).await?;
        assert!(
            refreshed.report_version.version_nr() > baseline.report_version.version_nr(),
            "{scenario}"
        );
        assert_eq!(
            refreshed.report.last_exploration_error, expected_error,
            "{scenario}"
        );
        assert_eq!(
            response
                .report
                .expect("refresh returns a report")
                .last_exploration_error
                .is_some(),
            expected_error.is_some(),
            "{scenario}"
        );

        let result = env
            .api()
            .explore(Request::new(rpc::forge::BmcEndpointRequest {
                ip_address: bmc_ip.to_string(),
                mac_address: None,
            }))
            .await;
        if expected_error.is_some() {
            let error = result.expect_err("ad-hoc exploration returns the endpoint error directly");
            assert_eq!(error.code(), tonic::Code::Internal, "{scenario}");
            assert!(
                error.message().contains("machines/bmc/site/root"),
                "{scenario}: {error}"
            );
        } else {
            result?;
        }
        assert_eq!(
            endpoint_explorer.explore_endpoint_call_count(),
            calls_before + 2,
            "{scenario}: each manual request must reach the endpoint explorer"
        );
        assert_eq!(
            explored_endpoint(&env, bmc_ip).await?.report_version,
            refreshed.report_version,
            "{scenario}: ad-hoc exploration must preserve the stored report"
        );
    }

    Ok(())
}

async fn explored_endpoint(
    env: &TestHarness,
    bmc_ip: IpAddr,
) -> Result<ExploredEndpoint, Box<dyn std::error::Error>> {
    let endpoint = db::explored_endpoints::find_all_by_ip(bmc_ip, &env.api().database_connection)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| format!("expected endpoint {bmc_ip} to exist"))?;
    Ok(endpoint)
}
