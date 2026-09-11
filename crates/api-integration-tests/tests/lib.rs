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
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{self, Duration};

use ::carbide_utils::HostPortPair;
use ::machine_a_tron::lifecycle_timings::{LifecycleTimingOverrides, PartialLifecycleTimings};
use ::machine_a_tron::{
    BmcMockRegistry, DeviceHandle, DhcpType, LogFormat, MachineATronConfig, MachineConfig,
};
use api_test_helper::api_server::{TEST_BMC_DHCP_RELAY_ADDRESS, TEST_BMC_NETWORK_PREFIX};
use api_test_helper::utils::TestApiServerArgs;
use api_test_helper::{
    IntegrationTestEnvironment, domain, instance, machine, metrics, subnet, tenant, utils, vpc,
    vpc_prefix,
};
use bmc_mock::test_support::TEST_MAC_POOL;
use bmc_mock::{HardwareType, ListenerOrAddress};
use carbide_uuid::machine::StableHostMachineId;
use eyre::ContextCompat;
use futures::FutureExt;
use futures::future::join_all;
use itertools::Itertools;
use mac_address::MacAddress;
use model::machine_boot_interface::BootInterfaceSelectionSource;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

const UNDERLAY_DHCP_RELAY_ADDRESS: Ipv4Addr = Ipv4Addr::new(172, 20, 1, 1);

#[ctor::ctor(unsafe)]
fn setup() {
    api_test_helper::setup_logging()
}

/// Run multiple machine-a-tron integration tests in parallel against a shared carbide API instance.
#[tokio::test(flavor = "multi_thread")]
async fn test_integration() -> eyre::Result<()> {
    // NOTE: These tests run two carbide-api servers, and the clients are configured to randomly
    // switch between them on every API call. This helps prevent issues that arise when multiple API
    // severs may be running in production.
    let Some(mut test_env) =
        IntegrationTestEnvironment::try_from_environment(2, "api_server_test_integration").await?
    else {
        println!("test_integration: SKIPPED (set REPO_ROOT and DATABASE_URL to run)");
        return Ok(());
    };

    let bmc_address_registry = BmcMockRegistry::default();
    let certs_dir = PathBuf::from(format!("{}/crates/bmc-mock", test_env.root_dir.display()));
    let server_config = bmc_mock::tls::server_config(Some(certs_dir)).unwrap();
    let mut bmc_mock_handle = bmc_mock::CombinedServer::run(
        "bmc-mock",
        bmc_address_registry.clone(),
        Some(ListenerOrAddress::Listener(
            // let OS choose available port
            TcpListener::bind("127.0.0.1:0")?,
        )),
        server_config,
    );

    // For preingestion firmware checks to work, carbide needs a directory which exists to be
    // configured as the firmware_directory. It can be empty, because our mocks should be showing
    // the desired firmware verisions to carbide (and thus it won't try to update.) This folder will
    // be deleted on Drop.
    let empty_firmware_dir = temp_dir::TempDir::with_prefix("firmware")?;

    // Begin the integration test by starting an API server. This will be shared between multiple
    // individual machine-a-tron-based tests, which can run in parallel against the same instance.
    let cancel_token = CancellationToken::new();
    let server_handle_1 = utils::start_api_server(
        &mut test_env,
        TestApiServerArgs {
            bmc_proxy: Some(HostPortPair::HostAndPort(
                "127.0.0.1".to_string(),
                bmc_mock_handle.address.port(),
            )),
            firmware_directory: empty_firmware_dir.path().to_owned(),
            addr_index: 0,
            put_dev_bin_in_path: true,
            insecure_discovery: true,
        },
        cancel_token.clone(),
    )
    .await?;
    let server_handle_2 = utils::start_api_server(
        &mut test_env,
        TestApiServerArgs {
            bmc_proxy: Some(HostPortPair::HostAndPort(
                "127.0.0.1".to_string(),
                bmc_mock_handle.address.port(),
            )),
            firmware_directory: empty_firmware_dir.path().to_owned(),
            addr_index: 1,
            put_dev_bin_in_path: true,
            insecure_discovery: true,
        },
        cancel_token.clone(),
    )
    .await?;

    assert_ne!(test_env.carbide_api_addrs[0], test_env.carbide_api_addrs[1]);
    assert_ne!(
        test_env.carbide_metrics_addrs[0],
        test_env.carbide_metrics_addrs[1]
    );
    let carbide_api_addrs = &test_env.carbide_api_addrs;

    let tenant_org_id = "tenant_organization";
    tenant::create(carbide_api_addrs, tenant_org_id, "Tenant Organization").await?;
    let tenant1_vpc = vpc::create(carbide_api_addrs, tenant_org_id).await?;
    let domain_id = domain::create(carbide_api_addrs, "tenant-1.local").await?;
    let managed_segment_id =
        subnet::create(carbide_api_addrs, &tenant1_vpc, &domain_id, 10, false).await?;

    // HostInband segments must live in a Flat VPC -- those VPC types are
    // mutually bound. Create one for the HostInband fixture.
    let flat_vpc = vpc::create_flat(carbide_api_addrs, tenant_org_id).await?;
    let host_inband_segment_id =
        subnet::create(carbide_api_addrs, &flat_vpc, &domain_id, 11, true).await?;

    // Create FNN VPC + VPC prefixes (IPv4 + IPv6) for dual-stack L3 linknet testing.
    let fnn_vpc = vpc::create_fnn(carbide_api_addrs, tenant_org_id).await?;
    let v4_vpc_prefix_id =
        vpc_prefix::create(carbide_api_addrs, &fnn_vpc, "10.10.12.0/24", "fnn-v4").await?;
    let v6_vpc_prefix_id =
        vpc_prefix::create(carbide_api_addrs, &fnn_vpc, "2001:db8:12::/48", "fnn-v6").await?;

    // Create dual-stack L2 segment on the FNN VPC for L2 dual-stack testing.
    let dual_stack_l2_segment_id =
        subnet::create_dual_stack(carbide_api_addrs, &fnn_vpc, &domain_id, 13).await?;

    // Run several tests in parallel.
    let all_tests = join_all([
        test_machine_a_tron_multidpu(
            HardwareType::DellPowerEdgeR750,
            &test_env,
            &bmc_address_registry,
            &managed_segment_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
        test_machine_a_tron_multidpu(
            HardwareType::NvidiaDgxH100,
            &test_env,
            &bmc_address_registry,
            &managed_segment_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
        test_machine_a_tron_multidpu(
            HardwareType::WiwynnGB200Nvl,
            &test_env,
            &bmc_address_registry,
            &managed_segment_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
        test_machine_a_tron_multidpu(
            HardwareType::LenovoGB300Nvl,
            &test_env,
            &bmc_address_registry,
            &managed_segment_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
        test_machine_a_tron_multidpu(
            HardwareType::NvidiaDgxGb300,
            &test_env,
            &bmc_address_registry,
            &managed_segment_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
        test_machine_a_tron_multidpu(
            HardwareType::SupermicroGb300Nvl,
            &test_env,
            &bmc_address_registry,
            &managed_segment_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
        test_machine_a_tron_zerodpu(
            HardwareType::DellPowerEdgeR750,
            &test_env,
            &bmc_address_registry,
            &flat_vpc,
        )
        .boxed(),
        test_machine_a_tron_nic_mode(
            HardwareType::DellPowerEdgeR750,
            &test_env,
            &bmc_address_registry,
            &flat_vpc,
            &host_inband_segment_id,
        )
        .boxed(),
        test_machine_a_tron_nic_mode(
            HardwareType::HpeProliantDl380aGen11,
            &test_env,
            &bmc_address_registry,
            &flat_vpc,
            &host_inband_segment_id,
        )
        .boxed(),
        test_machine_a_tron_nic_mode(
            HardwareType::WiwynnGB200Nvl,
            &test_env,
            &bmc_address_registry,
            &flat_vpc,
            &host_inband_segment_id,
        )
        .boxed(),
        test_machine_a_tron_nic_mode(
            HardwareType::SupermicroGb300Nvl,
            &test_env,
            &bmc_address_registry,
            &flat_vpc,
            &host_inband_segment_id,
        )
        .boxed(),
        // TODO: https://github.com/NVIDIA/infra-controller/issues/3709
        // Re-enable `test_machine_a_tron_dpu_to_nic_mode_reregistration` after the
        // Admin-to-HostInband re-ingestion race is fixed. The scenario currently flakes in CI when
        // the host-facing DPU MAC is re-created on the Admin segment before the NIC-mode
        // transition completes.
        test_machine_a_tron_dual_stack(
            HardwareType::DellPowerEdgeR750,
            &test_env,
            &bmc_address_registry,
            tenant_org_id,
            &v4_vpc_prefix_id,
            &v6_vpc_prefix_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
        test_machine_a_tron_dual_stack_l2(
            HardwareType::DellPowerEdgeR750,
            &test_env,
            &bmc_address_registry,
            &dual_stack_l2_segment_id,
            UNDERLAY_DHCP_RELAY_ADDRESS,
        )
        .boxed(),
    ]);

    tokio::select! {
        results = all_tests => results.into_iter().try_collect()?,
        _ = tokio::time::sleep(Duration::from_secs(20 * 60)) => {
            panic!("Tests did not complete after 20 minutes")
        }
    }

    metrics::wait_for_metric_line(
        &test_env.carbide_metrics_addrs,
        r#"carbide_site_explorer_boot_interface_selections_total{mechanism="redfish_chassis_id"}"#,
    )
    .await?;
    metrics::wait_for_metric_line(
        &test_env.carbide_metrics_addrs,
        r#"carbide_site_explorer_boot_interface_selections_total{mechanism="redfish_serial_number"}"#,
    )
    .await?;
    // The Wiwynn mock deliberately gives the `RedfishChassisId` selection the higher
    // scout PCI slot so the integration path exercises automatic reconciliation.
    metrics::wait_for_metric_line(
        &test_env.carbide_metrics_addrs,
        r#"carbide_scout_pci_evaluations_total{result="differs_from_stored"}"#,
    )
    .await?;

    let metric_infos = metrics::collect_metric_infos(&test_env.carbide_metrics_addrs)?;
    assert!(
        metric_infos.iter().any(|metric| {
            metric.name == "carbide_site_explorer_boot_interface_selections_total"
        }),
        "the multi-DPU integration paths must exercise boot-interface selection observability",
    );
    assert!(
        metric_infos
            .iter()
            .any(|metric| metric.name == "carbide_scout_pci_evaluations_total"),
        "the MaT scout path must exercise PCI comparison observability",
    );
    generate_core_metric_docs(&test_env.carbide_metrics_addrs);

    cancel_token.cancel();
    server_handle_1.wait().await?;
    server_handle_2.wait().await?;
    test_env.db_pool.close().await;
    bmc_mock_handle.stop().await?;
    Ok(())
}

fn generate_core_metric_docs(metrics_endpoints: &[SocketAddr]) {
    let mut infos = metrics::collect_metric_infos(metrics_endpoints).unwrap();
    retain_existing_core_metric_infos(&mut infos);

    // Delete everything with "alt_metric_" prefix
    let mut infos: Vec<_> = infos
        .into_iter()
        .filter(|metric| !metric.name.starts_with("alt_metric"))
        .collect();

    // Sort metrics for consistency
    infos.sort_by(|e1, e2| e1.name.cmp(&e2.name));

    let mut docs = "# NVIDIA Infra Controller (NICo) Core Metrics\n\n".to_string();
    use std::fmt::Write;

    use askama_escape::Escaper;

    writeln!(
        &mut docs,
        "This file contains a list of metrics exported by NVIDIA Infra Controller (NICo). \
        The list is auto-generated from an integration test (`test_integration`). \
        Metrics no test exercises are added with `cargo xtask check-metric-docs --fix`. \
        NVLink partition monitor's metrics are documented in the manual: \
        [NVLink Partitioning](../manuals/nvlink_partitioning.md#metrics)."
    )
    .unwrap();
    writeln!(&mut docs).unwrap();
    writeln!(&mut docs, "<table>").unwrap();
    writeln!(
        &mut docs,
        "<tr><td>Name</td><td>Type</td><td>Description</td></tr>"
    )
    .unwrap();

    for info in &infos {
        write!(&mut docs, "<tr>").unwrap();
        write!(&mut docs, "<td>{}</td>", info.name).unwrap();
        write!(&mut docs, "<td>{}</td>", info.ty).unwrap();
        write!(&mut docs, "<td>").unwrap();
        askama_escape::Html
            .write_escaped(&mut docs, &info.help)
            .unwrap();
        write!(&mut docs, "</td>").unwrap();
        writeln!(&mut docs, "</tr>").unwrap();
    }
    writeln!(&mut docs, "</table>").unwrap();

    let path = std::path::Path::new(METRIC_DOC_PATH);
    assert!(
        path.exists(),
        "Metric path at {} does not exist. Did the directory structure change?",
        path.to_str().unwrap()
    );

    std::fs::write(path, docs).unwrap();
}

fn retain_existing_core_metric_infos(infos: &mut Vec<metrics::MetricInfo>) {
    let mut infos_by_name = infos
        .drain(..)
        .map(|info| (info.name.clone(), info))
        .collect::<HashMap<_, _>>();

    for line in std::fs::read_to_string(METRIC_DOC_PATH)
        .unwrap_or_default()
        .lines()
    {
        if let Some(info) = metrics::MetricInfo::parse_from_docs_line(line) {
            infos_by_name.entry(info.name.clone()).or_insert(info);
        }
    }

    infos.extend(infos_by_name.into_values());
}

trait ParseFromHtmlDocs {
    fn parse_from_docs_line(line: &str) -> Option<Self>
    where
        Self: Sized;
}

impl ParseFromHtmlDocs for metrics::MetricInfo {
    fn parse_from_docs_line(line: &str) -> Option<Self> {
        let row = line.strip_prefix("<tr><td>")?.strip_suffix("</td></tr>")?;
        let cells = row.split("</td><td>").collect::<Vec<_>>();
        let [name, ty, help] = cells.as_slice() else {
            return None;
        };

        if *name == "Name" {
            return None;
        }

        let unescape_html_cell = |value: &str| -> String {
            if value.contains('&') {
                value
                    .replace("&lt;", "<")
                    .replace("&gt;", ">")
                    .replace("&#39;", "'")
                    .replace("&quot;", "\"")
                    .replace("&amp;", "&")
            } else {
                value.to_string()
            }
        };

        Some(metrics::MetricInfo {
            name: unescape_html_cell(name),
            ty: unescape_html_cell(ty),
            help: unescape_html_cell(help),
        })
    }
}

pub(crate) const METRIC_DOC_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/observability/core_metrics.md"
);

/// Run integration tests with machine-a-tron, asserting on metrics. This has to run as its own
/// test, to make the values in the metrics buckets predictable.
#[tokio::test(flavor = "multi_thread", worker_threads = 10)]
async fn test_metrics_integration() -> eyre::Result<()> {
    let Some(mut test_env) =
        IntegrationTestEnvironment::try_from_environment(1, "api_server_test_metrics_integration")
            .await?
    else {
        return Ok(());
    };

    let bmc_address_registry = BmcMockRegistry::default();
    let certs_dir = PathBuf::from(format!("{}/crates/bmc-mock", test_env.root_dir.display()));
    let server_config = bmc_mock::tls::server_config(Some(certs_dir)).unwrap();
    let mut bmc_mock_handle = bmc_mock::CombinedServer::run(
        "bmc-mock",
        bmc_address_registry.clone(),
        Some(ListenerOrAddress::Listener(
            // let OS choose available port
            TcpListener::bind("127.0.0.1:0")?,
        )),
        server_config,
    );

    // For preingestion firmware checks to work, carbide needs a directory which exists to be
    // configured as the firmware_directory. It can be empty, because our mocks should be showing
    // the desired firmware verisions to carbide (and thus it won't try to update.) This folder will
    // be deleted on Drop.
    let empty_firmware_dir = temp_dir::TempDir::with_prefix("firmware")?;

    // Begin the integration test by starting an API server. This will be shared between multiple
    // individual machine-a-tron-based tests, which can run in parallel against the same instance.
    let cancel_token = CancellationToken::new();
    let server_handle = utils::start_api_server(
        &mut test_env,
        TestApiServerArgs {
            bmc_proxy: Some(HostPortPair::HostAndPort(
                "127.0.0.1".to_string(),
                bmc_mock_handle.address.port(),
            )),
            firmware_directory: empty_firmware_dir.path().to_owned(),
            addr_index: 0,
            put_dev_bin_in_path: true,
            insecure_discovery: true,
        },
        cancel_token.clone(),
    )
    .await?;

    // Save typing after the server has replaced the port-zero placeholders.
    let IntegrationTestEnvironment {
        carbide_api_addrs,
        root_dir: _,
        carbide_metrics_addrs,
        db_pool,
        metrics: _,
        db_url: _,
        credential_config: _,
        _vault_handle,
    } = test_env.clone();

    // Before the initial host bootstrap, the dns_records view
    // should contain 0 entries.
    assert_eq!(0i64, db::test_support::dns::record_count(&db_pool).await);

    run_machine_a_tron_machine_test(
        HardwareType::DellPowerEdgeR750,
        1,
        1,
        false,
        &test_env,
        &bmc_address_registry,
        UNDERLAY_DHCP_RELAY_ADDRESS,
        |machine_handle| {
            let db_pool = db_pool.clone();
            let carbide_api_addrs = carbide_api_addrs.to_vec();
            let carbide_metrics_addrs = carbide_metrics_addrs.to_vec();
            async move {
                machine_handle.dpus()[0].wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90)).await?;

                // After the host_bootstrap, the dns_records view
                // should contain 8 entries:
                // - 2x "human friendly" (BMC) for Host + DPU.
                // - 2x "human friendly" (ADM) for Host + DPU.
                // - 2x Machine ID (BMC) for Host + DPU.
                // - 2x Machine ID (ADM) for Host + DPU.
                assert_eq!(8i64, db::test_support::dns::record_count(&db_pool).await);

                // Metrics are only updated after the machine state controller run one more
                // time since the emitted metrics are for states at the start of the iteration.
                // Therefore wait for the updated metrics to show up.
                let metrics = metrics::wait_for_metric_line(
                    &carbide_metrics_addrs,
                    r#"carbide_machines_per_state{fresh="true",state="ready",substate=""} 1"#,
                )
                    .await?;
                metrics::assert_metric_line(&metrics, r#"carbide_machines_total{fresh="true"} 1"#);
                // Also check that metrics are emitted under the configured `alt_metric_prefix`
                metrics::assert_metric_line(&metrics, r#"alt_metric_machines_total{fresh="true"} 1"#);
                metrics::assert_not_metric_line(
                    &metrics,
                    "machine_reboot_attempts_in_booting_with_discovery_image",
                );

                let tenant_org_id = "tenant_organization";
                tenant::create(&carbide_api_addrs, tenant_org_id, "Tenant Organization").await?;
                let vpc_id = vpc::create(&carbide_api_addrs, tenant_org_id).await?;
                let domain_id = domain::create(&carbide_api_addrs, "tenant-1.local").await?;
                let segment_id = subnet::create(&carbide_api_addrs, &vpc_id, &domain_id, 10, false).await?;
                let host_machine_id: StableHostMachineId = machine_handle
                    .observed_machine_id()
                    .expect("Should have gotten a machine ID by now")
                    .try_into()?;

                // Create instance with phone_home enabled
                let instance_id = instance::create(
                    &carbide_api_addrs,
                    &host_machine_id,
                    &segment_id,
                    Some("test"),
                    true,
                    true,
                    &[],
                ).await?;

                let metrics = metrics::wait_for_metric_line(
                    &carbide_metrics_addrs,
                    r#"carbide_machines_per_state{fresh="true",state="assigned",substate="ready"} 1"#,
                )
                    .await?;
                metrics::assert_metric_line(&metrics, r#"carbide_machines_total{fresh="true"} 1"#);
                metrics::assert_not_metric_line(
                    &metrics,
                    r#"carbide_machines_per_state{fresh="true",state="ready",substate=""}"#,
                );
                metrics::assert_not_metric_line(
                    &metrics,
                    "machine_reboot_attempts_in_booting_with_discovery_image",
                );

                instance::release(&carbide_api_addrs, &host_machine_id, &instance_id, true).await?;

                let metrics = metrics::wait_for_metric_line(&carbide_metrics_addrs, r#"carbide_machines_per_state{fresh="true",state="waitingforcleanup",substate="hostcleanup"} 1"#).await?;
                metrics::assert_metric_line(&metrics, r#"carbide_machines_total{fresh="true"} 1"#);

                machine::wait_for_state(
                    &carbide_api_addrs,
                    &host_machine_id,
                    "MachineValidation",
                ).await?;

                machine::wait_for_state(&carbide_api_addrs, &host_machine_id, "Discovered").await?;

                // It stays in Discovered until we notify that reboot happened, which this test doesn't
                let metrics = metrics::wait_for_metric_line(
                    &carbide_metrics_addrs,
                    r#"carbide_machines_per_state{fresh="true",state="hostnotready",substate="discovered"} 1"#,
                )
                    .await?;
                metrics::assert_not_metric_line(
                    &metrics,
                    r#"carbide_machines_per_state{fresh="true",state="assigned""#,
                );

                // Explicitly test that the histogram for `carbide_reboot_attempts_in_booting_with_discovery_image_bucket`
                // uses the custom buckets we defined for retries/attempts
                for &(bucket, count) in &[(0, 0), (1, 1), (2, 1), (3, 1), (5, 1), (10, 1)] {
                    metrics::assert_metric_line(
                        &metrics,
                        &format!(
                            r#"carbide_reboot_attempts_in_booting_with_discovery_image_bucket{{le="{bucket}"}} {count}"#
                        ),
                    );
                }
                metrics::assert_not_metric_line(
                    &metrics,
                    r#"carbide_reboot_attempts_in_booting_with_discovery_image_bucket{le="4"}"#,
                );
                metrics::assert_not_metric_line(
                    &metrics,
                    r#"carbide_reboot_attempts_in_booting_with_discovery_image_bucket{le="6"}"#,
                );
                metrics::assert_metric_line(
                    &metrics,
                    r#"carbide_reboot_attempts_in_booting_with_discovery_image_bucket{le="+Inf"} 1"#,
                );
                metrics::assert_metric_line(
                    &metrics,
                    "carbide_reboot_attempts_in_booting_with_discovery_image_sum 1",
                );
                metrics::assert_metric_line(
                    &metrics,
                    "carbide_reboot_attempts_in_booting_with_discovery_image_count 1",
                );

                Ok(())
            }
        },
    ).await?;

    sleep(time::Duration::from_millis(500)).await;
    bmc_mock_handle.stop().await?;
    cancel_token.cancel();
    server_handle.wait().await?;
    db_pool.close().await;
    Ok(())
}

async fn test_machine_a_tron_multidpu(
    hw_type: HardwareType,
    test_env: &IntegrationTestEnvironment,
    bmc_mock_registry: &BmcMockRegistry,
    segment_id: &str,
    underlay_dhcp_relay_address: Ipv4Addr,
) -> eyre::Result<()> {
    run_machine_a_tron_machine_test(
        hw_type,
        1,
        2,
        false,
        test_env,
        bmc_mock_registry,
        underlay_dhcp_relay_address,
        |machine_handle| {
            let segment_id = segment_id.to_string();
            let carbide_api_addrs = &test_env.carbide_api_addrs;
            let db_pool = test_env.db_pool.clone();
            let expected_selection = (hw_type == HardwareType::WiwynnGB200Nvl).then(|| {
                let dpu = machine_handle
                    .host_info()
                    .dpus
                    .get(1)
                    .expect("Wiwynn GB200 host should contain its DPU with the lower scout PCI slot");
                (
                    dpu.host_mac_address,
                    BootInterfaceSelectionSource::ScoutReportPci,
                )
            });
            async move {
                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                let machine_id: StableHostMachineId = machine_handle
                    .observed_machine_id()
                    .expect("Machine ID should be set if host is ready")
                    .try_into()?;
                if let Some(expected_selection) = expected_selection {
                    let selection: (MacAddress, BootInterfaceSelectionSource) = sqlx::query_as(
                        "SELECT desired_mac_address, selection_source
                         FROM machine_boot_interfaces
                         WHERE machine_id = $1",
                    )
                    .bind(machine_id)
                    .fetch_one(&db_pool)
                    .await?;
                    assert_eq!(
                        selection, expected_selection,
                        "the Wiwynn mock must replace its RedfishChassisId selection with the lower scout PCI slot",
                    );
                }
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Ready, allocating instance",
                );
                let instance_id = instance::create(
                    carbide_api_addrs,
                    &machine_id,
                    &segment_id,
                    None,
                    false,
                    false,
                    &[],
                )
                .await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Assigned/Ready", Duration::from_secs(90))
                    .await?;

                let instances = instance::get_by_machine_id(
                    carbide_api_addrs,
                    machine_handle
                        .observed_machine_id()
                        .expect("HostMachine should have a Machine ID once it's in ready state")
                        .to_string()
                        .as_str(),
                )
                .await?;
                let interface = instances
                    .instances
                    .first()
                    .and_then(|instance| instance.status.as_ref())
                    .and_then(|status| status.network.as_ref())
                    .and_then(|network| network.interfaces.first())
                    .context("allocated instance has no network status interface")?;
                assert_eq!(interface.addresses.len(), 1);
                assert_eq!(interface.gateways.len(), 1);

                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Assigned/Ready, releasing instance",
                );
                instance::release(carbide_api_addrs, &machine_id, &instance_id, false).await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Ready again, all done",
                );
                Ok::<(), eyre::Report>(())
            }
        },
    )
    .await
}

async fn test_machine_a_tron_zerodpu(
    hw_type: HardwareType,
    test_env: &IntegrationTestEnvironment,
    bmc_mock_registry: &BmcMockRegistry,
    flat_vpc_id: &str,
) -> eyre::Result<()> {
    run_machine_a_tron_machine_test(
        hw_type,
        1,
        0,
        false,
        test_env,
        bmc_mock_registry,
        UNDERLAY_DHCP_RELAY_ADDRESS,
        |machine_handle| {
            let carbide_api_addrs = &test_env.carbide_api_addrs;
            let flat_vpc_id = flat_vpc_id.to_string();
            async move {
                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                let machine_id: StableHostMachineId = machine_handle
                    .observed_machine_id()
                    .expect("Machine ID should be set if host is ready")
                    .try_into()?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Ready, allocating instance",
                );

                let instance_id = instance::create_with_auto_host_inband_networking(
                    carbide_api_addrs,
                    &machine_id,
                    &flat_vpc_id,
                )
                .await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Assigned/Ready", Duration::from_secs(90))
                    .await?;
                assert_auto_instance_network(carbide_api_addrs, &instance_id, &flat_vpc_id).await?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Assigned/Ready, releasing instance",
                );

                instance::release(carbide_api_addrs, &machine_id, &instance_id, false).await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Ready again, all done",
                );
                Ok::<(), eyre::Report>(())
            }
        },
    )
    .await
}

async fn test_machine_a_tron_nic_mode(
    hw_type: HardwareType,
    test_env: &IntegrationTestEnvironment,
    bmc_mock_registry: &BmcMockRegistry,
    flat_vpc_id: &str,
    host_inband_segment_id: &str,
) -> eyre::Result<()> {
    run_machine_a_tron_machine_test(
        hw_type,
        1,
        1,
        true,
        test_env,
        bmc_mock_registry,
        UNDERLAY_DHCP_RELAY_ADDRESS,
        |machine_handle| {
            let carbide_api_addrs = &test_env.carbide_api_addrs;
            let flat_vpc_id = flat_vpc_id.to_string();
            let host_inband_segment_id = host_inband_segment_id.to_string();
            let expected_host_mac = machine_handle
                .host_info()
                .dpus
                .first()
                .expect("NIC-mode host should contain at least one DPU NIC")
                .host_mac_address
                .to_string();
            async move {
                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                let machine_id: StableHostMachineId = machine_handle
                    .observed_machine_id()
                    .expect("Machine ID should be set if host is ready")
                    .try_into()?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Ready, allocating instance",
                );

                assert_nic_mode_host(
                    carbide_api_addrs,
                    &machine_id,
                    &expected_host_mac,
                    &host_inband_segment_id,
                )
                .await?;

                let instance_id = instance::create_with_auto_host_inband_networking(
                    carbide_api_addrs,
                    &machine_id,
                    &flat_vpc_id,
                )
                .await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Assigned/Ready", Duration::from_secs(90))
                    .await?;
                assert_auto_instance_network(carbide_api_addrs, &instance_id, &flat_vpc_id).await?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Assigned/Ready, releasing instance",
                );

                instance::release(carbide_api_addrs, &machine_id, &instance_id, false).await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine has made it to Ready again, all done",
                );
                Ok::<(), eyre::Report>(())
            }
        },
    )
    .await
}

async fn assert_nic_mode_host(
    carbide_api_addrs: &[SocketAddr],
    machine_id: &carbide_uuid::machine::MachineId,
    expected_host_mac: &str,
    host_inband_segment_id: &str,
) -> eyre::Result<()> {
    let machine = machine::get_by_id(carbide_api_addrs, machine_id).await?;
    let status = machine
        .status
        .context("NIC-mode host has no machine status")?;
    let associated_dpus = &status.associated_dpu_machine_ids;
    eyre::ensure!(
        associated_dpus.is_empty(),
        "NIC-mode host {machine_id} still has associated DPUs: {associated_dpus:?}"
    );

    let interfaces = &status.interfaces;
    eyre::ensure!(
        interfaces
            .iter()
            .all(|interface| interface.attached_dpu_machine_id.is_none()),
        "NIC-mode host {machine_id} still has a DPU-backed interface: {interfaces:?}"
    );

    let has_expected_primary_host_inband_interface = interfaces.iter().any(|interface| {
        interface
            .mac_address
            .eq_ignore_ascii_case(expected_host_mac)
            && interface.primary_interface
            && interface
                .segment_id
                .is_some_and(|id| id.to_string() == host_inband_segment_id)
            && interface.interface_type != Some(rpc::forge::InterfaceType::Bmc as i32)
    });
    eyre::ensure!(
        has_expected_primary_host_inband_interface,
        "NIC-mode host {machine_id} did not promote DPU host-facing PF {expected_host_mac} as its primary HostInband interface"
    );
    Ok(())
}

async fn assert_auto_instance_network(
    carbide_api_addrs: &[SocketAddr],
    instance_id: &str,
    flat_vpc_id: &str,
) -> eyre::Result<()> {
    let instance = instance::get_by_id(carbide_api_addrs, instance_id).await?;
    let network = instance
        .config
        .as_ref()
        .and_then(|config| config.network.as_ref())
        .context("automatically-networked instance has no network config")?;
    eyre::ensure!(
        network.auto_config.is_some(),
        "instance {instance_id} did not retain auto networking: {network:?}"
    );
    eyre::ensure!(
        network.interfaces.is_empty(),
        "instance {instance_id} exposed resolved interfaces in its external config: {network:?}"
    );
    eyre::ensure!(
        network
            .auto_config
            .as_ref()
            .and_then(|config| config.vpc_id)
            .is_some_and(|id| id.to_string() == flat_vpc_id),
        "instance {instance_id} did not retain flat VPC {flat_vpc_id}: {network:?}"
    );

    let network_status = instance
        .status
        .as_ref()
        .and_then(|status| status.network.as_ref())
        .context("automatically-networked instance has no network status")?;
    let status_interfaces = &network_status.interfaces;
    eyre::ensure!(
        !status_interfaces.is_empty()
            && status_interfaces.iter().all(|interface| {
                interface
                    .vpc_id
                    .is_some_and(|id| id.to_string() == flat_vpc_id)
                    && interface
                        .mac_address
                        .as_ref()
                        .is_some_and(|mac| !mac.is_empty())
                    && !interface.addresses.is_empty()
                    && !interface.gateways.is_empty()
                    && !interface.prefixes.is_empty()
            }),
        "instance {instance_id} status does not contain resolved flat VPC networking: {status_interfaces:?}"
    );
    eyre::ensure!(
        network_status.configs_synced == rpc::forge::SyncState::Synced as i32,
        "instance {instance_id} network status is not synced: {network_status:?}"
    );
    Ok(())
}

async fn test_machine_a_tron_dual_stack(
    hw_type: HardwareType,
    test_env: &IntegrationTestEnvironment,
    bmc_mock_registry: &BmcMockRegistry,
    tenant_organization_id: &str,
    v4_vpc_prefix_id: &str,
    v6_vpc_prefix_id: &str,
    underlay_dhcp_relay_address: Ipv4Addr,
) -> eyre::Result<()> {
    run_machine_a_tron_machine_test(
        hw_type,
        1,
        1,
        false,
        test_env,
        bmc_mock_registry,
        underlay_dhcp_relay_address,
        |machine_handle| {
            let v4_prefix_id = v4_vpc_prefix_id.to_string();
            let v6_prefix_id = v6_vpc_prefix_id.to_string();
            let tenant_organization_id = tenant_organization_id.to_string();
            let carbide_api_addrs = &test_env.carbide_api_addrs;
            async move {
                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                let machine_id: StableHostMachineId = machine_handle
                    .observed_machine_id()
                    .expect("Machine ID should be set if host is ready")
                    .try_into()?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine is Ready, allocating dual-stack instance via ipv6 config",
                );
                let instance_id = instance::create_with_vpc_prefixes(
                    carbide_api_addrs,
                    &machine_id,
                    &tenant_organization_id,
                    &[&v4_prefix_id, &v6_prefix_id],
                )
                .await?;

                machine_handle
                    .wait_until_machine_up_with_api_state(
                        "Assigned/Ready",
                        Duration::from_secs(90),
                    )
                    .await?;

                // Wait for the agent to report interface addresses. The agent runs
                // a network observation loop that populates addresses asynchronously
                // after the instance reaches Assigned/Ready.
                let machine_id_str = machine_id.to_string();
                let mut addrs = vec![];
                for _ in 0..30 {
                    let instances = instance::get_by_machine_id(
                        carbide_api_addrs,
                        &machine_id_str,
                    )
                    .await?;
                    if let Some(addresses) = instances
                        .instances
                        .first()
                        .and_then(|instance| instance.status.as_ref())
                        .and_then(|status| status.network.as_ref())
                        .and_then(|network| network.interfaces.first())
                        .map(|interface| &interface.addresses)
                        && !addresses.is_empty()
                    {
                        addrs = addresses.clone();
                        break;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }

                let addr_strings: Vec<&str> = addrs.iter().map(String::as_str).collect();
                let has_ipv4 = addr_strings.iter().any(|a| a.contains('.'));
                let has_ipv6 = addr_strings.iter().any(|a| a.contains(':'));
                assert!(
                    has_ipv4,
                    "Dual-stack interface should have an IPv4 address, got: {addr_strings:?}"
                );
                assert!(
                    has_ipv6,
                    "Dual-stack interface should have an IPv6 address, got: {addr_strings:?}"
                );
                assert_eq!(
                    addr_strings.len(),
                    2,
                    "Dual-stack interface should have exactly 2 addresses (IPv4 + IPv6), got: {addr_strings:?}"
                );

                tracing::info!(
                    machine_id = %machine_id,
                    addresses = ?addr_strings,
                    "Machine dual-stack allocation verified",
                );

                instance::release(carbide_api_addrs, &machine_id, &instance_id, false)
                    .await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine back to Ready after dual-stack release",
                );
                Ok::<(), eyre::Report>(())
            }
        },
    )
    .await
}

/// Tests dual-stack on an FNN L2 segment (shared subnet with SVI/VRR).
/// The segment is pre-created with both IPv4 and IPv6 prefixes, and the
/// handler allocates SVI IPs for both. Instances get one IP per prefix.
async fn test_machine_a_tron_dual_stack_l2(
    hw_type: HardwareType,
    test_env: &IntegrationTestEnvironment,
    bmc_mock_registry: &BmcMockRegistry,
    dual_stack_segment_id: &str,
    underlay_dhcp_relay_address: Ipv4Addr,
) -> eyre::Result<()> {
    run_machine_a_tron_machine_test(
        hw_type,
        1,
        1,
        false,
        test_env,
        bmc_mock_registry,
        underlay_dhcp_relay_address,
        |machine_handle| {
            let segment_id = dual_stack_segment_id.to_string();
            let carbide_api_addrs = &test_env.carbide_api_addrs;
            async move {
                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                let machine_id: StableHostMachineId = machine_handle
                    .observed_machine_id()
                    .expect("Machine ID should be set if host is ready")
                    .try_into()?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine is Ready, allocating dual-stack L2 instance",
                );
                let instance_id = instance::create(
                    carbide_api_addrs,
                    &machine_id,
                    &segment_id,
                    None,
                    false,
                    false,
                    &[],
                )
                .await?;

                machine_handle
                    .wait_until_machine_up_with_api_state(
                        "Assigned/Ready",
                        Duration::from_secs(120),
                    )
                    .await?;

                tracing::info!(
                    machine_id = %machine_id,
                    "Machine dual-stack L2 instance allocated and reached Assigned/Ready",
                );

                instance::release(carbide_api_addrs, &machine_id, &instance_id, false).await?;

                machine_handle
                    .wait_until_machine_up_with_api_state("Ready", Duration::from_secs(90))
                    .await?;
                tracing::info!(
                    machine_id = %machine_id,
                    "Machine back to Ready after dual-stack L2 release",
                );
                Ok::<(), eyre::Report>(())
            }
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_machine_a_tron_machine_test<F, O>(
    hw_type: HardwareType,
    host_count: u32,
    dpu_per_host_count: u32,
    dpus_in_nic_mode: bool,
    test_env: &IntegrationTestEnvironment,
    bmc_mock_registry: &BmcMockRegistry,
    underlay_dhcp_relay_address: Ipv4Addr,
    run_assertions: F,
) -> eyre::Result<()>
where
    F: Fn(DeviceHandle) -> O,
    O: Future<Output = eyre::Result<()>>,
{
    let api_addr = test_env
        .carbide_api_addrs
        .first()
        .copied()
        .context("no carbide API addresses configured")?;
    let additional_api_urls = test_env.carbide_api_addrs[1..]
        .iter()
        .map(|a| format!("https://{}:{}", a.ip(), a.port()))
        .collect();
    let mat_config = MachineATronConfig {
        racks: BTreeMap::new(),
        machines: BTreeMap::from([(
            "config".to_string(),
            Arc::new(MachineConfig {
                rack_id: None,
                rack_placement: None,
                hw_type,
                host_count,
                dpu_per_host_count,
                dpu_reboot_delay: 1,
                host_reboot_delay: 1,
                timing_overrides: Some(LifecycleTimingOverrides {
                    host: PartialLifecycleTimings {
                        reboot: Some(Duration::from_secs(1)),
                        // ZERO disables the BMC self-reset offline window entirely,
                        // keeping ingestion at its pre-feature pace
                        bmc_reset: Some(Duration::ZERO),
                        ..Default::default()
                    },
                    dpu: PartialLifecycleTimings {
                        reboot: Some(Duration::from_secs(1)),
                        // ZERO disables the BMC self-reset offline window entirely,
                        // keeping ingestion at its pre-feature pace
                        bmc_reset: Some(Duration::ZERO),
                        ..Default::default()
                    },
                }),
                acceleration_factor: 1.0,
                underlay_dhcp_relay_address,
                // Keep this distinct from the DPU Underlay relay so NIC-mode tests fail if
                // machine-a-tron sends direct host DHCP through the DPU network.
                host_inband_dhcp_relay_address: Some(Ipv4Addr::new(10, 10, 11, 2)),
                bmc_dhcp_relay_address: TEST_BMC_DHCP_RELAY_ADDRESS,
                run_interval_idle: Duration::from_secs(1),
                run_interval_working: Duration::from_millis(100),
                network_status_run_interval: Duration::from_secs(1),
                scout_run_interval: Duration::from_secs(1),
                discovery_retry_interval: Duration::from_millis(100),
                dpus_in_nic_mode,
                dpu_firmware_versions: None,
                host_firmware_versions: None,
                dpu_agent_version: None,
            }),
        )]),
        carbide_api_url: format!("https://{}:{}", api_addr.ip(), api_addr.port()),
        dhcp: DhcpType::Api {},
        log_file: None,
        log_format: LogFormat::Compact,
        bmc_mock_port: 0, // unused, we're using dynamic ports on localhost
        bmc_mock_certs_dir: None,
        configure_carbide_bmc_proxy_host: None,
        persist_dir: None,
        cleanup_on_quit: false,
        register_expected_machines: true,
        host_bmc_password: None,
        dpu_bmc_password: None,
        api_refresh_interval: Duration::from_millis(500),
        mock_bmc_ssh_server: false,
        enable_ipmi_simulation: false,
        hw_mac_address_ranges: None,
        mac_address_pool: None,
        ufm_mock: Default::default(),
    };

    let (provisionable_handles, mat_handle) = api_test_helper::machine_a_tron::run_local(
        mat_config,
        additional_api_urls,
        &test_env.root_dir,
        bmc_mock_registry.clone(),
        TEST_MAC_POOL.clone(),
    )
    .await
    .unwrap();

    let results = join_all(provisionable_handles.into_iter().map(|machine_handle| {
        let relay_assertion_handle = machine_handle.clone();
        let assertions = run_assertions(machine_handle);
        async move {
            assertions.await?;
            assert_relay_selection(
                &relay_assertion_handle,
                dpus_in_nic_mode,
                underlay_dhcp_relay_address,
            )
        }
    }))
    .await;
    let result_count = results.len();
    let assertion_result: eyre::Result<()> = results.into_iter().try_collect();
    let shutdown_result = mat_handle.shutdown().await;

    assert_eq!(result_count, host_count as usize);
    assertion_result?;
    shutdown_result
}

fn assert_relay_selection(
    machine_handle: &DeviceHandle,
    dpus_in_nic_mode: bool,
    underlay_dhcp_relay_address: Ipv4Addr,
) -> eyre::Result<()> {
    let host_bmc_ip = machine_handle
        .bmc_ip()
        .context("host BMC DHCP did not return an address")?;
    eyre::ensure!(
        TEST_BMC_NETWORK_PREFIX.contains(&host_bmc_ip),
        "host BMC DHCP used the underlay relay: {host_bmc_ip}"
    );

    for dpu in machine_handle.dpus() {
        let dpu_bmc_ip = dpu.bmc_ip().context("DPU doesn't have BMC IP")?;
        eyre::ensure!(
            TEST_BMC_NETWORK_PREFIX.contains(&dpu_bmc_ip),
            "DPU BMC DHCP used the underlay relay: {dpu_bmc_ip}"
        );

        if !dpus_in_nic_mode {
            let dpu_underlay_ip = dpu
                .machine_ip()
                .context("DPU doesn't have machine IP address")?;
            eyre::ensure!(
                dpu_underlay_ip.octets()[..3] == underlay_dhcp_relay_address.octets()[..3],
                "DPU OOB boot DHCP used the BMC relay: {dpu_underlay_ip}"
            );
        }
    }

    Ok(())
}
