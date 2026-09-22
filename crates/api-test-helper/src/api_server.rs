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
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use carbide_secrets::CredentialConfig;
use carbide_utils::HostPortPair;
use ipnet::Ipv4Net;
use tokio::sync::oneshot::Sender;
use tokio_util::sync::CancellationToken;

use crate::utils::LOCALHOST_CERTS;

const DOMAIN_NAME: &str = "forge.integrationtest";

/// BMC network used by API integration tests. The `/23` accommodates both 71-BMC rack fixtures
/// while keeping the SQL allocator's candidate scan bounded.
pub const TEST_BMC_NETWORK_PREFIX: Ipv4Net = Ipv4Net::new_assert(Ipv4Addr::new(127, 0, 0, 0), 23);

/// DHCP relay address for [`TEST_BMC_NETWORK_PREFIX`].
pub const TEST_BMC_DHCP_RELAY_ADDRESS: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 10);

// Use a struct for the args to start() so that callers can see argument names
pub struct StartArgs {
    pub addr: SocketAddr,
    pub metrics_addr: SocketAddr,
    pub root_dir: PathBuf,
    pub db_url: String,
    pub bmc_proxy: Option<HostPortPair>,
    pub firmware_directory: PathBuf,
    pub cancel_token: CancellationToken,
    pub ready_channel: Sender<carbide::ApiServerAddresses>,
    pub credential_config: CredentialConfig,
    pub insecure_discovery: bool,
}

pub async fn start(
    StartArgs {
        // Destructure start args
        addr,
        metrics_addr,
        root_dir,
        db_url,
        bmc_proxy,
        firmware_directory,
        cancel_token,
        ready_channel,
        credential_config,
        insecure_discovery,
    }: StartArgs,
) -> eyre::Result<()> {
    let firmware_directory_str = firmware_directory.to_string_lossy();
    let root_dir_str = root_dir.to_string_lossy();

    let root_cafile_path = LOCALHOST_CERTS.ca_cert.to_str().unwrap();
    let identity_pemfile_path = LOCALHOST_CERTS.server_cert.to_string_lossy();
    let identity_keyfile_path = LOCALHOST_CERTS.server_key.to_string_lossy();

    let carbide_config_str = {
        let bmc_proxy_cfg = if let Some(bmc_proxy) = bmc_proxy {
            format!(r#"bmc_proxy = "{bmc_proxy}""#)
        } else {
            // None is encoded by omitting the option altogether... just drop a comment
            String::from("# no bmc_proxy set")
        };

        let addr = format!("[::]:{}", addr.port());

        format!(
            r#"
        listen = "{addr}"
        metrics_endpoint = "{metrics_addr}"
        alt_metric_prefix = "alt_metric_"
        database_url = "{db_url}"
        max_database_connections = 1000
        asn = 65535
        dhcp_servers = []
        route_servers = []
        enable_route_servers = false
        deny_prefixes = []
        site_fabric_prefixes = []
        dpu_ipmi_tool_impl = "bmc-mock"
        initial_domain_name = "{DOMAIN_NAME}"
        initial_dpu_agent_upgrade_policy = "off"
        max_concurrent_machine_updates = 1
        attestation_enabled = false
        max_find_by_ids = 100
        internet_l3_vni = 1337
        bypass_rbac = true
        allow_insecure_discovery = {insecure_discovery}
        scout_boot_interface_correction_enabled = true

        [ib_config]
        max_partition_per_tenant = 31
        enabled = false
        mtu = 4
        rate_limit = 200
        service_level = 0

        [tls]
        root_cafile_path = "{root_cafile_path}"
        identity_pemfile_path = "{identity_pemfile_path}"
        identity_keyfile_path = "{identity_keyfile_path}"
        admin_root_cafile_path = "nothing_will_read_from_this_during_integration_tests"

        [auth]
        permissive_mode = true
        casbin_policy_file = "{root_dir_str}/crates/api/casbin-policy.csv"

        [auth.trust]
        spiffe_trust_domain="nothing_will_read_from_this_during_integration_tests"
        spiffe_service_base_paths=["/nothing_will_read_from_this_during_integration_tests"]
        spiffe_machine_base_path="nothing_will_read_from_this_during_integration_tests"
        additional_issuer_cns=["nothing_will_read_from_this_during_integration_tests"]

        [pools.vpc-vni]
        type = "integer"

        [[pools.vpc-vni.ranges]]
        start = "2024500"
        end = "2024550"

        [pools.vlan-id]
        type = "integer"

        [[pools.vlan-id.ranges]]
        start = "100"
        end = "501"

        [pools.lo-ip]
        ranges = []
        prefix = "10.180.62.1/26"
        type = "ipv4"

        [pools.vni]
        type = "integer"

        [[pools.vni.ranges]]
        start = "1024500"
        end = "1024550"

        [pools.dpa-vni]
        type = "integer"

        [[pools.dpa-vni.ranges]]
        start = "1024600"
        end = "1024650"

        [pools.vpc-dpu-lo]
        type = "ipv4"
        prefix = "10.181.62.1/26"

        [pools.external-vpc-vni]
        type = "integer"

        [[pools.external-vpc-vni.ranges]]
        start = "51000"
        end = "51007"

        [pools.fnn-asn]
        type = "integer"

        [[pools.fnn-asn.ranges]]
        start = "4268001000"
        end = "4268001999"

        [networks.DEV1-C09-DPU-01]
        type = "underlay"
        prefix = "172.20.1.0/24"
        gateway = "172.20.1.1"
        mtu = 1490
        reserve_first = 5

        [networks.admin]
        type = "admin"
        prefix = "172.20.0.0/24"
        gateway = "172.20.0.1"
        mtu = 9000
        reserve_first = 5

        [networks.DEV1-C09-IPMI-01]
        type = "underlay"
        prefix = "{TEST_BMC_NETWORK_PREFIX}"
        gateway = "{TEST_BMC_DHCP_RELAY_ADDRESS}"
        mtu = 1490
        reserve_first = 0

        [site_explorer]
        enabled = true
        run_interval = "1s"
        concurrent_explorations = 30
        explorations_per_run = 90
        create_machines = true
        machines_created_per_run = 30
        {bmc_proxy_cfg}
        reset_rate_limit = "3600s"

        [machine_state_controller]
        dpu_wait_time = "1s"
        power_down_wait = "1s"
        failure_retry_time = "1s"
        dpu_up_threshold = "31449600s"

        [machine_state_controller.controller]
        iteration_time = "1s"
        processor_dispatch_interval = "500ms"
        max_object_handling_time = "180s"
        max_concurrency = 10
        metric_emission_interval = "1s"
        metric_hold_time = "2s"

        [network_segment_state_controller]
        network_segment_drain_time = "60s"

        [network_segment_state_controller.controller]
        iteration_time = "2s"
        processor_dispatch_interval = "500ms"
        max_object_handling_time = "180s"
        max_concurrency = 10
        metric_emission_interval = "1s"
        metric_hold_time = "2s"

        [ib_partition_state_controller.controller]
        iteration_time = "20s"
        processor_dispatch_interval = "2s"
        max_object_handling_time = "180s"
        max_concurrency = 10
        metric_emission_interval = "1s"
        metric_hold_time = "2s"

        [host_models]

        [rack_profiles.NVL72]
        product_family = "gb200"

        [rack_profiles.NVL72.rack_capabilities.compute]
        name = "GB200"
        count = 18
        vendor = "NVIDIA"

        [rack_profiles.NVL72.rack_capabilities.switch]
        count = 0

        [rack_profiles.NVL72.rack_capabilities.power_shelf]
        count = 0

        [rack_profiles.NVL72_GB300]
        product_family = "gb300"

        [rack_profiles.NVL72_GB300.rack_capabilities.compute]
        name = "GB300"
        count = 18
        vendor = "Lenovo"

        [rack_profiles.NVL72_GB300.rack_capabilities.switch]
        count = 0

        [rack_profiles.NVL72_GB300.rack_capabilities.power_shelf]
        count = 0

        [firmware_global]
        autoupdate = true
        host_enable_autoupdate = []
        host_disable_autoupdate = []
        run_interval = "1s"
        max_uploads = 4
        concurrency_limit = 16
        firmware_directory = "{firmware_directory_str}"

        [fnn.routing_profiles.EXTERNAL]
        internal = false
        route_target_imports = []

        [[fnn.routing_profiles.EXTERNAL.route_targets_on_exports]]
        # Tag routes with the common external route tag
        asn = 65001
        vni = 50500

        [fnn.admin_vpc]
        enabled = true
        vpc_vni = 60100

        [host_health]
        hardware_health_reports = "Disabled"

        [measured_boot_collector]
        enabled = true
        run_interval = "10s"

        [machine_validation_config]
        enabled = true

        # [machine_identity] omitted: default is disabled. Enabling requires
        # current_encryption_key_id and vault/file credentials for encryption_keys;
        # populate_initial_vault_secrets does not seed those for integration tests.
    "#
        )
    };

    let mut tmp = tempfile::NamedTempFile::new()?;
    std::io::Write::write_all(&mut tmp, carbide_config_str.as_bytes())?;
    carbide::run(
        0,
        tmp.path().to_path_buf(),
        None,
        credential_config,
        true,
        // The in-process test server does not serve the admin web UI.
        None,
        cancel_token,
        ready_channel,
    )
    .await
}
