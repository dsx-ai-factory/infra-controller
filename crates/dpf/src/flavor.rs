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

//! DPUFlavor configuration for HBN.

use std::collections::BTreeSet;
use std::fmt::Write;

use carbide_libmlx_model::nvconfig::{DpuNvConfigProfile, GB200_B3240_V1_PF_TOTAL_SF};
use kube::core::ObjectMeta;
use sha2::{Digest, Sha256};

use crate::crds::dpuflavors_generated::{
    DPUFlavor, DpuFlavorConfigFiles, DpuFlavorConfigFilesContentFrom,
    DpuFlavorConfigFilesContentFromConfigMapKeyRef, DpuFlavorConfigFilesOperation,
    DpuFlavorConfigFilesType, DpuFlavorContainerdConfig, DpuFlavorDpuMode,
    DpuFlavorEwNicConfigurations, DpuFlavorEwNicConfigurationsNetworkBay,
    DpuFlavorEwNicConfigurationsRawNvConfig, DpuFlavorEwNicConfigurationsSpectrumXOptimized,
    DpuFlavorEwNicConfigurationsSpectrumXOptimizedMultiplaneMode,
    DpuFlavorEwNicConfigurationsSpectrumXOptimizedOverlay, DpuFlavorGrub, DpuFlavorNvconfig,
    DpuFlavorNvconfigDevice, DpuFlavorOvs, DpuFlavorServiceReadiness,
    DpuFlavorServiceReadinessGate, DpuFlavorSpec, DpuFlavorSysctl, DpuFlavorSystemdServices,
    DpuFlavorSystemdServicesOperation,
};
use crate::crds::dpuflavortemplates_generated::{DPUFlavorTemplate, DpuFlavorTemplateSpec};
use crate::service_vpc_slot::ServiceVpcSlots;
use crate::types::{
    DEFAULT_DPU_NUM_OF_VFS, DEFAULT_PF_TOTAL_SF_RESERVED, DOCA_HBN_SERVICE_NAME,
    DpfInterceptBridge, DpfInterceptBridging, DpfProxyDetails, DpuDeploymentType,
    DpuServiceInterfaceTemplateDefinition, DpuServiceInterfaceTemplateType,
};

pub const DEFAULT_FLAVOR_NAME: &str = "dpu-flavor";
const DELAY_HOST_OS_INIT_PARAMETER: &str = "DELAY_HOST_OS_INIT=0x3";
const OVN_ENCAP_SERVICE_NAME: &str = "nico-ovn-encap-ip.service";
const OVN_ENCAP_SCRIPT_PATH: &str = "/usr/local/sbin/nico-configure-ovn-encap-ip";

fn host_service_readiness() -> DpuFlavorServiceReadiness {
    DpuFlavorServiceReadiness {
        gate: Some(DpuFlavorServiceReadinessGate::DpuServiceCriticalPodsReady),
    }
}

impl DPUFlavor {
    /// Returns `"{default_flavor_name}-{hash}"` where the hash is the first 8 bytes (16 hex chars)
    /// of a stable SHA-256 digest of the spec. The name changes whenever the spec changes, which
    /// causes outdated DPUs to be reprovisioned by MachineUpdateManager.
    pub fn unique_name(&self, default_flavor_name: &str) -> Result<String, crate::error::DpfError> {
        let json = serde_json::to_string(&self.spec)?;
        let short_hash = hex::encode(&Sha256::digest(json.as_bytes())[..8]);
        Ok(format!("{default_flavor_name}-{short_hash}"))
    }
}

impl DPUFlavorTemplate {
    /// Returns a hash-derived name that changes whenever the template changes.
    pub fn unique_name(&self, default_flavor_name: &str) -> Result<String, crate::error::DpfError> {
        let json = serde_json::to_string(&self.spec)?;
        let short_hash = hex::encode(&Sha256::digest(json.as_bytes())[..8]);
        Ok(format!("{default_flavor_name}-{short_hash}"))
    }
}

#[derive(serde::Serialize)]
struct DpuFlavorTemplateBody<'a> {
    spec: &'a DpuFlavorSpec,
}

/// Wraps a DPUFlavor spec in the body DPF expects in a DPUFlavorTemplate.
pub(crate) fn flavor_template_from_flavor(
    flavor: &DPUFlavor,
) -> Result<DPUFlavorTemplate, crate::error::DpfError> {
    Ok(DPUFlavorTemplate {
        metadata: ObjectMeta {
            name: None,
            namespace: flavor.metadata.namespace.clone(),
            ..Default::default()
        },
        spec: DpuFlavorTemplateSpec {
            dpu_resources: None,
            system_reserved_resources: None,
            template: serde_yaml::to_string(&DpuFlavorTemplateBody { spec: &flavor.spec })
                .map_err(|error| {
                    crate::error::DpfError::ConfigError(format!(
                        "failed to serialize DPUFlavorTemplate: {error}"
                    ))
                })?,
        },
    })
}

fn get_default_ovs_defaults_base() -> String {
    concat!(
        "_ovs-vsctl() {\n",
            "ovs-vsctl --timeout 15 \"$@\"\n",
        "}\n",

        "# Remove default OVS configuration on the DPU and ensure no leftovers on the OVS kernel side\n",
        "_ovs-vsctl --if-exists del-br ovsbr1\n",
        "_ovs-vsctl --if-exists del-br ovsbr2\n",
        "ovs-appctl --timeout 15 dpctl/del-dp system@ovs-system || true\n",

        "_ovs-vsctl set Open_vSwitch . other_config:doca-init=true\n",
        "_ovs-vsctl set Open_vSwitch . other_config:dpdk-max-memzones=50000\n",
        "_ovs-vsctl set Open_vSwitch . other_config:hw-offload=true\n",
        "_ovs-vsctl set Open_vSwitch . other_config:pmd-quiet-idle=true\n",
        "_ovs-vsctl set Open_vSwitch . other_config:max-idle=20000\n",
        "_ovs-vsctl set Open_vSwitch . other_config:max-revalidator=5000\n",
        "_ovs-vsctl remove Open_vSwitch . other_config default-datapath-type || true\n",

        "if systemctl list-unit-files openvswitch-switch.service &>/dev/null; then\n",
            "systemctl restart openvswitch-switch\n",
        "elif systemctl list-unit-files openvswitch.service &>/dev/null; then\n",
            "systemctl restart openvswitch\n",
        "fi\n",
        "_ovs-vsctl --may-exist add-br br-sfc\n",
        "_ovs-vsctl set bridge br-sfc datapath_type=netdev\n",
        "_ovs-vsctl set bridge br-sfc fail_mode=secure\n",
        "_ovs-vsctl --may-exist add-port br-sfc p0\n",
        "_ovs-vsctl set Interface p0 type=dpdk\n",
        "_ovs-vsctl set Interface p0 mtu_request=9216\n",
        "_ovs-vsctl set Port p0 external_ids:dpf-type=physical\n",
        "_ovs-vsctl --if-exists del-br br-hbn\n",
        "_ovs-vsctl --may-exist add-br br-hbn\n",
        "_ovs-vsctl set bridge br-hbn datapath_type=netdev\n",
        "_ovs-vsctl set bridge br-hbn fail_mode=secure\n",
    )
    .to_string()
}

/// OVS raw config script for the BF4 flavor.
fn get_bf4_ovs_defaults_base() -> String {
    concat!(
        "_ovs-vsctl() {\n",
        "    ovs-vsctl --timeout 15 \"$@\"\n",
        "}\n",
        // Exported so the post-OVS hook inherits the helper, as on Astra.
        "export -f _ovs-vsctl\n",

        "# Remove default OVS configuration on the DPU and ensure no leftovers on the OVS kernel side\n",
        "for i in $(seq 1 99); do\n",
        "    ovs-vsctl --if-exists del-br \"ovsbr${i}\"\n",
        "done\n",

        "ovs-appctl --timeout 15 dpctl/del-dp system@ovs-system || true\n",

        "_ovs-vsctl set Open_vSwitch . other_config:doca-init=true\n",
        "_ovs-vsctl set Open_vSwitch . other_config:dpdk-max-memzones=50000\n",
        "_ovs-vsctl set Open_vSwitch . other_config:hw-offload=true\n",
        "_ovs-vsctl set Open_vSwitch . other_config:pmd-quiet-idle=true\n",
        "_ovs-vsctl set Open_vSwitch . other_config:max-idle=20000\n",
        "_ovs-vsctl set Open_vSwitch . other_config:max-revalidator=5000\n",
        "_ovs-vsctl set Open_vSwitch . other_config:doca-congestion-threshold=60\n",
        "_ovs-vsctl set Open_vSwitch . other_config:flow-limit=500000\n",
        "_ovs-vsctl set Open_vSwitch . other_config:hw-offload-ct-unidir-udp-enabled=true\n",
        "_ovs-vsctl remove Open_vSwitch . other_config default-datapath-type || true\n",

        "if systemctl list-unit-files openvswitch-switch.service &>/dev/null; then\n",
        "    systemctl restart openvswitch-switch\n",
        "elif systemctl list-unit-files openvswitch.service &>/dev/null; then\n",
        "    systemctl restart openvswitch\n",
        "fi\n",

        "_ovs-vsctl --may-exist add-br br-sfc\n",
        "_ovs-vsctl set bridge br-sfc datapath_type=netdev\n",
        "_ovs-vsctl set bridge br-sfc fail_mode=secure\n",
        "_ovs-vsctl --may-exist add-port br-sfc p0\n",
        "_ovs-vsctl set Interface p0 type=dpdk\n",
        "_ovs-vsctl set Interface p0 mtu_request=9216\n",
        "_ovs-vsctl set Port p0 external_ids:dpf-type=physical\n",

        // br-hbn is absent on a fresh DPU, so a bare del-br would fail the run.
        "_ovs-vsctl --if-exists del-br br-hbn\n",
        "_ovs-vsctl --may-exist add-br br-hbn\n",
        "_ovs-vsctl set bridge br-hbn datapath_type=netdev\n",
        "_ovs-vsctl set bridge br-hbn fail_mode=secure\n",

        "mst start\n",
    )
    .to_string()
}

/// Builds the BF3 OVS bootstrap with deterministic configured peer bridges.
fn get_default_ovs_defaults_with_topology(
    topology: Option<&DpfInterceptBridging>,
    service_vpc_slots: ServiceVpcSlots,
) -> String {
    // Retain the BF3 base verbatim, then append normalized intercept-bridge state.
    let mut script = get_default_ovs_defaults_base();
    if let Some(topology) = topology {
        append_peer_bridge_bootstrap(&mut script, topology, |interface| {
            format!("'{}'", interface.identity.bf3_raw_netdev_name())
        });
    }
    service_vpc_slots.append_ovs_bridges(&mut script);
    append_ovn_encap_ip_bootstrap(&mut script);
    script
}

/// Builds the generic-BF4 OVS bootstrap after preflighting every configured PF.
fn get_bf4_ovs_defaults_with_topology(
    topology: Option<&DpfInterceptBridging>,
    service_vpc_slots: ServiceVpcSlots,
) -> String {
    // Explicit bash, as on Astra: the base uses `export -f`, which errors under dash.
    let mut script = String::from("#!/bin/bash\n");
    append_pre_ovs_hook(&mut script);
    // Preflight is prepended so no inherited or configured OVS operation can run first.
    script.push_str(&topology.map_or_else(String::new, render_bf4_pf_preflight));
    script.push_str(&get_bf4_ovs_defaults_base());
    if let Some(topology) = topology {
        append_peer_bridge_bootstrap(&mut script, topology, |interface| {
            let variable =
                bf4_pf_variable(interface.identity.controller_id, interface.identity.pf_id);
            match interface.identity.vf_id {
                Some(vf_id) => format!("\"${{{variable}}}vf{vf_id}\""),
                None => format!("\"${{{variable}}}\""),
            }
        });
    }
    service_vpc_slots.append_ovs_bridges(&mut script);
    append_ovn_encap_ip_bootstrap(&mut script);
    append_post_ovs_hook(&mut script);
    script
}

/// Appends the operator's pre-OVS hook, which runs before anything else.
fn append_pre_ovs_hook(script: &mut String) {
    script.push_str(
        "if [ -x /opt/dpf/extra-script-pre-ovs.sh ]; then /opt/dpf/extra-script-pre-ovs.sh; fi\n",
    );
}

/// Appends the operator's post-OVS hook, which runs last.
fn append_post_ovs_hook(script: &mut String) {
    script.push_str(
        "if [ -x /opt/dpf/extra-script-post-ovs.sh ]; then /opt/dpf/extra-script-post-ovs.sh; fi\n",
    );
}

/// Appends the per-DPU OVN address update to provisioning-time OVS configuration.
fn append_ovn_encap_ip_bootstrap(script: &mut String) {
    // Owner contract: DPF runs rawConfigScript after oob_net0 is configured. Set the value here
    // so provisioning establishes it directly even when the installed DPF API prunes the retained
    // systemdServices request. Keep this attempt best-effort because rawConfigScript must not fail
    // provisioning when management addressing is not ready; the independently ordered oneshot
    // executes the same body directly and retains its strict exit status.
    script.push_str("# Configure the per-DPU OVN encapsulation address during OVS provisioning.\n");
    script.push_str("(\n");
    script.push_str(ovn_encap_ip_commands());
    script.push_str(") || true\n");
}

/// Appends idempotent bridge creation and tolerant raw-representor attachment.
fn append_peer_bridge_bootstrap(
    script: &mut String,
    topology: &DpfInterceptBridging,
    raw_netdev: impl Fn(&DpfInterceptBridge) -> String,
) {
    // Normalized topology order keeps the script stable across map iteration order.
    for interface in topology.interfaces() {
        writeln!(script, "host_representor={}", raw_netdev(interface)).ok();
        writeln!(
            script,
            "_ovs-vsctl --may-exist add-br '{}'",
            interface.bridge
        )
        .ok();
        writeln!(
            script,
            "_ovs-vsctl set bridge '{}' datapath_type=netdev",
            interface.bridge
        )
        .ok();
        writeln!(
            script,
            "_ovs-vsctl --if-exists del-port \"$host_representor\" -- --may-exist add-port '{}' \"$host_representor\" -- set interface \"$host_representor\" type=dpdk mtu_request=9216 external_ids='{{}}' || true",
            interface.bridge
        )
        .ok();
        writeln!(
            script,
            "_ovs-vsctl br-set-external-id '{}' bridge-uplink '{}'",
            interface.bridge, interface.patch_port
        )
        .ok();
    }
}

/// Resolves configured generic-BF4 PF identities to runtime netdevs before any OVS mutation.
///
/// Topology identifies hardware by controller/PF identity rather than its runtime kernel name.
/// The rendered preflight therefore finds the unique netdev whose `phys_port_name` carries that
/// identity and stores it in a shell variable used by the later bridge bootstrap. Resolving every
/// parent first prevents a missing or ambiguous PF from leaving partially modified OVS state.
fn render_bf4_pf_preflight(topology: &DpfInterceptBridging) -> String {
    // Collapse PF and VF entries to their parent identities. VF netdev names are derived from the
    // resolved parent later, so every parent needs only one sysfs lookup.
    let pf_identities: BTreeSet<_> = topology
        .interfaces()
        .iter()
        .map(|interface| {
            let mut identity = interface.identity;
            identity.vf_id = None;
            identity
        })
        .collect();
    if pf_identities.is_empty() {
        return String::new();
    }

    // Search the semantic `phys_port_name`, rejecting both absence and ambiguity rather than
    // selecting whichever runtime netdev happens to appear first.
    let mut script = String::from(concat!(
        "sys_class_net=${NICO_SYS_CLASS_NET:-/sys/class/net}\n",
        "resolve_dpf_pf() {\n",
        "    local semantic_name=$1 result_variable=$2 phys_port_name netdev\n",
        "    local -a matches=()\n",
        "    for phys_port_name in \"$sys_class_net\"/*/phys_port_name; do\n",
        "        [[ -r \"$phys_port_name\" ]] || continue\n",
        "        if [[ \"$(<\"$phys_port_name\")\" == \"$semantic_name\" ]]; then\n",
        "            netdev=$(basename \"${phys_port_name%/phys_port_name}\")\n",
        "            matches+=(\"$netdev\")\n",
        "        fi\n",
        "    done\n",
        "    if (( ${#matches[@]} != 1 )); then\n",
        "        echo \"expected exactly one BF4 PF netdev with phys_port_name ${semantic_name}; found ${#matches[@]}\" >&2\n",
        "        return 1\n",
        "    fi\n",
        "    printf -v \"$result_variable\" '%s' \"${matches[0]}\"\n",
        "}\n",
    ));

    for identity in &pf_identities {
        // Persist each resolved netdev in an identity-specific variable consumed by bridge setup.
        writeln!(
            script,
            "resolve_dpf_pf '{}' '{}' || exit 1",
            identity.bf4_phys_port_name(),
            bf4_pf_variable(identity.controller_id, identity.pf_id)
        )
        .ok();
    }

    let identities: Vec<_> = pf_identities.into_iter().collect();
    // Keep resolved parents distinct before deriving VF netdev names. This is defensive while the
    // current topology contract permits only one selected controller/PF parent.
    for (index, left_identity) in identities.iter().enumerate() {
        for right_identity in &identities[index + 1..] {
            let left = bf4_pf_variable(left_identity.controller_id, left_identity.pf_id);
            let right = bf4_pf_variable(right_identity.controller_id, right_identity.pf_id);
            writeln!(
                script,
                "if [[ \"${{{left}}}\" == \"${{{right}}}\" ]]; then echo 'BF4 PF identities {} and {} resolved to the same netdev' >&2; exit 1; fi",
                left_identity.bf4_phys_port_name(),
                right_identity.bf4_phys_port_name()
            )
            .ok();
        }
    }
    script
}

/// Returns the shell variable holding one discovered generic-BF4 PF netdev.
fn bf4_pf_variable(controller_id: u8, pf_id: u8) -> String {
    format!("dpf_c{controller_id}p{pf_id}_netdev")
}

/// Builds the Astra OVS bootstrap while preserving its fixed xplane configuration.
// The OVS scripts have strict checks to exit on failure. DPF will retry the
// entire OVS scripts operation on failure. This includes the
// initialize-sox-maintenance script for the newer cerebro based implementation.
fn get_bf4_astra_ovs_defaults(
    topology: Option<&DpfInterceptBridging>,
    partial_multiplane_enabled: bool,
) -> String {
    // Preserve the topology-free prefix and fixed body byte-for-byte so existing template hashes
    // remain stable, while resolving configured PF identities before the fixed scripts mutate OVS.
    let mut script = String::from("#!/bin/bash\n");
    if topology.is_some() {
        // Stop after a required stage fails because later topology setup assumes it completed.
        script.push_str("set -e\n");
    }
    append_pre_ovs_hook(&mut script);
    script.push_str(&topology.map_or_else(String::new, render_bf4_pf_preflight));
    script.push_str(concat!(
        "# Shared helper used by the called scripts; exported so they inherit it\n",
        "\n",
        "_ovs-vsctl() {\n",
        "  ovs-vsctl --timeout 30 \"$@\"\n",
        "}\n",
        "export -f _ovs-vsctl\n",
        "\n",
        "{{ if and .dpu_device_underlay_rows (index .dpu_device_underlay_rows 0).cerebro_ifname }}\n",
        "# New-format inventory (Cerebro interface names): configure OVS, altnames, lldp and netplan,\n",
        "# then initialize maintenance once if partial-multiplane is enabled.\n",
        "/usr/local/sbin/configure-xplane-network.sh || exit $?\n",
    ));
    if partial_multiplane_enabled {
        script.push_str("/usr/local/sbin/initialize-spx-maintenance.sh || exit $?\n");
    }
    script.push_str(concat!(
        "{{ else }}\n",
        "# Legacy MAC/IP inventory: configure networking without maintenance initialization.\n",
        "# 1. Configure OVS bridges and xplane ports\n",
    ));
    if topology.is_some() {
        // A separately launched Bash child does not inherit the parent script's errexit option.
        // Pass it explicitly because configured topology assumes the fixed setup completes.
        script.push_str("bash -e /etc/mellanox/ovs-script.sh\n");
    } else {
        // Keep topology-free output byte-for-byte stable for immutable flavor hashing.
        script.push_str("/etc/mellanox/ovs-script.sh\n");
    }
    script.push_str(concat!(
        "\n",
        "# 2. Enable OVS metrics for xplane and Weave\n",
        "_ovs-vsctl set Open_vSwitch . \\\n",
        "  'other_config:flow-metric-labels=\"to_plane,from_plane,device_name,group,plane\"' \\\n",
        "  other_config:doca-telemetry-interval=\"1000\" \\\n",
        "  other_config:doca-telemetry-ipc=\"true\" \\\n",
        "  other_config:doca-telemetry-source-id=\"xplane\"\n",
        "\n",
        "# 3. Configure rail bridge addressing (netplan)\n",
        "/usr/local/sbin/configure-xplane-network.sh\n",
        "{{ end }}\n",
    ));

    // Layer configured intercept bridges onto the established Astra xplane topology after its fixed setup.
    if let Some(topology) = topology {
        append_peer_bridge_bootstrap(&mut script, topology, |interface| {
            let variable =
                bf4_pf_variable(interface.identity.controller_id, interface.identity.pf_id);
            match interface.identity.vf_id {
                Some(vf_id) => format!("\"${{{variable}}}vf{vf_id}\""),
                None => format!("\"${{{variable}}}\""),
            }
        });
    }
    append_post_ovs_hook(&mut script);
    script
}

/// Rejects bf.cfg parameters carrying the Go template opening delimiter.
///
/// Astra serializes its flavor spec into a `DPUFlavorTemplate` whose body DPF renders as a Go
/// template before creating each DPU's flavor, so a `{{` there is an action rather than literal
/// text: it interpolates device values, or fails the render when it names a key the device does
/// not have. Escaping is not reliably expressible, because the Go escape `{{ "{{" }}` carries
/// quotes that YAML serialization escapes in turn.
///
/// BF3 and generic BF4 write a DPUFlavor directly and would pass `{{` through untouched, but they
/// are held to the same restriction so the configuration contract does not vary by deployment
/// type. A parameter that works on one deployment and silently breaks on another is a worse trap
/// than a rule that applies everywhere.
fn reject_template_delimiters(parameters: &[String]) -> Result<(), crate::error::DpfError> {
    // Report the position, never the value: a parameter may carry a password hash.
    if let Some((index, _)) = parameters
        .iter()
        .enumerate()
        .find(|(_, parameter)| parameter.contains("{{"))
    {
        return Err(crate::error::DpfError::ConfigError(format!(
            "resolved bf.cfg parameter {index} contains the Go template delimiter `{{{{`, which \
             cannot reach bf.cfg verbatim: BF4 Astra renders its DPUFlavorTemplate body as a Go \
             template, so the value would be interpreted instead"
        )));
    }
    Ok(())
}

/// Rejects proxy strings containing characters that would break a systemd `Environment="..."` line:
/// double-quotes (break the quoting), newlines / carriage returns (break the unit-file line), and
/// any other ASCII control character (< 0x20 or DEL 0x7f).
fn validate_proxy_string(value: &str, field: &str) -> Result<(), crate::error::DpfError> {
    if value.chars().any(|c| c == '"' || c < '\x20' || c == '\x7f') {
        return Err(crate::error::DpfError::ConfigError(format!(
            "proxy {field} contains characters that are not allowed in a systemd \
             Environment= value (quotes, newlines, or control characters)"
        )));
    }
    Ok(())
}

/// Build a DPUFlavor for BF3 or generic BF4. If `proxy` is set, a containerd proxy drop-in
/// config file is appended so the DPU can pull images through the proxy.
///
/// Production Astra initialization goes through [`crate::DpfSdk`] and builds a DPUFlavorTemplate.
///
/// Returns `ConfigError` if any proxy string contains characters that would break the generated
/// systemd `Environment="..."` lines (quotes, newlines, or other control characters).
///
/// `metadata.name` is left unset; callers must set it (typically via [`DPUFlavor::unique_name`])
/// before creating the resource in the cluster.
pub fn default_flavor_for(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
    // Selects the DPUFlavor variant to build for the given deployment type.
    deployment_type: DpuDeploymentType,
) -> Result<DPUFlavor, crate::error::DpfError> {
    if matches!(deployment_type, DpuDeploymentType::Bf4Astra) {
        return Err(crate::error::DpfError::ConfigError(
            "BF4 Astra uses DPUFlavorTemplate; use flavor_bf4_astra instead".to_string(),
        ));
    }

    let pf_total_sf = match deployment_type {
        DpuDeploymentType::Bf3 | DpuDeploymentType::Bf3Gb200 | DpuDeploymentType::Bf4Generic => {
            DEFAULT_PF_TOTAL_SF_RESERVED
        }
        DpuDeploymentType::Bf4Astra => unreachable!("handled above"),
    };

    default_flavor_for_with_topology(
        namespace,
        proxy,
        deployment_type,
        DEFAULT_DPU_NUM_OF_VFS,
        pf_total_sf,
        None,
        None,
        ServiceVpcSlots::default(),
        &[],
        false,
    )
}

/// Builds a platform flavor from validated VF, SF, topology, and effective-inventory inputs.
///
/// `extra_bfcfg_parameters` are operator-supplied bf.cfg lines appended verbatim to the built-in
/// `bfcfgParameters` of every deployment type.
///
/// WARNING: Every argument here feeds the flavor hash. Changing one will generate a new
/// DPUFlavor, reprovisioning the deployment's DPUs.
// Each argument is an independent site input; a struct would move the same list one level out.
#[allow(clippy::too_many_arguments)]
pub(crate) fn default_flavor_for_with_topology(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
    deployment_type: DpuDeploymentType,
    num_of_vfs: u32,
    pf_total_sf: u32,
    intercept_bridging: Option<&DpfInterceptBridging>,
    dhcp_acl_interfaces: Option<&[DpuServiceInterfaceTemplateDefinition]>,
    service_vpc_slots: ServiceVpcSlots,
    extra_bfcfg_parameters: &[String],
    enable_delay_host_init: bool,
) -> Result<DPUFlavor, crate::error::DpfError> {
    match deployment_type {
        DpuDeploymentType::Bf4Generic => flavor_bf4_with_topology(
            namespace,
            proxy,
            num_of_vfs,
            pf_total_sf,
            intercept_bridging,
            dhcp_acl_interfaces,
            service_vpc_slots,
            extra_bfcfg_parameters,
            enable_delay_host_init,
        ),
        DpuDeploymentType::Bf4Astra => Err(crate::error::DpfError::ConfigError(
            "BF4 Astra uses DPUFlavorTemplate; call flavor_bf4_astra() instead".to_string(),
        )),
        DpuDeploymentType::Bf3 | DpuDeploymentType::Bf3Gb200 => default_flavor_with_topology(
            namespace,
            proxy,
            deployment_type,
            num_of_vfs,
            pf_total_sf,
            intercept_bridging,
            dhcp_acl_interfaces,
            service_vpc_slots,
            extra_bfcfg_parameters,
            enable_delay_host_init,
        ),
    }
}

/// Build the BF4 (generic) DPUFlavor spec, with BF4-specific grub and OVS configuration.
/// If `proxy` is set, a containerd proxy drop-in config file is appended so the DPU can pull
/// images through the proxy.
///
/// Returns `ConfigError` if any proxy string contains characters that would break the generated
/// systemd `Environment="..."` lines (quotes, newlines, or other control characters).
///
/// `metadata.name` is left unset; callers must set it (typically via [`DPUFlavor::unique_name`])
/// before creating the resource in the cluster.
pub fn flavor_bf4(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
) -> Result<DPUFlavor, crate::error::DpfError> {
    flavor_bf4_with_topology(
        namespace,
        proxy,
        DEFAULT_DPU_NUM_OF_VFS,
        DEFAULT_PF_TOTAL_SF_RESERVED,
        None,
        None,
        ServiceVpcSlots::default(),
        &[],
        false,
    )
}

/// Builds generic BF4 flavor state from the validated site VF count and intercept-bridging topology.
// Each argument is an independent site input; a struct would move the same list one level out.
#[allow(clippy::too_many_arguments)]
fn flavor_bf4_with_topology(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
    num_of_vfs: u32,
    pf_total_sf: u32,
    intercept_bridging: Option<&DpfInterceptBridging>,
    dhcp_acl_interfaces: Option<&[DpuServiceInterfaceTemplateDefinition]>,
    service_vpc_slots: ServiceVpcSlots,
    extra_bfcfg_parameters: &[String],
    enable_delay_host_init: bool,
) -> Result<DPUFlavor, crate::error::DpfError> {
    reject_template_delimiters(extra_bfcfg_parameters)?;
    let mut bfcfg_parameters = vec![
        "UPDATE_ATF_UEFI=yes".to_string(),
        "UPDATE_DPU_OS=yes".to_string(),
        "WITH_NIC_FW_UPDATE=yes".to_string(),
    ];
    bfcfg_parameters.extend_from_slice(extra_bfcfg_parameters);
    Ok(DPUFlavor {
        metadata: ObjectMeta {
            name: None,
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: DpuFlavorSpec {
            scalable_functions: None,
            virtual_functions: None,
            dpu_mode: Some(DpuFlavorDpuMode::ZeroTrust),
            dpu_resources: None,
            bfcfg_parameters: Some(bfcfg_parameters),
            config_files: Some(get_config_files(
                proxy,
                DpuDeploymentType::Bf4Generic,
                dhcp_acl_interfaces,
            )?),
            containerd_config: None,
            grub: Some(bf4_grub_params()),
            host_network_interface_configs: None,
            nvconfig: Some(vec![get_bf4_nvconfig(
                num_of_vfs,
                pf_total_sf,
                enable_delay_host_init,
            )]),
            ovs: Some(crate::crds::dpuflavors_generated::DpuFlavorOvs {
                raw_config_script: Some(get_bf4_ovs_defaults_with_topology(
                    intercept_bridging,
                    service_vpc_slots,
                )),
            }),
            sysctl: None,
            system_reserved_resources: None,
            ew_nic_configurations: None,
            packages: None,
            // rawConfigScript sets the value during provisioning. Retain this ordered oneshot so
            // DPF versions with systemdServices support also enforce it after network readiness.
            systemd_services: Some(vec![ovn_encap_systemd_service()]),
            dma: None,
            service_readiness: enable_delay_host_init.then(host_service_readiness),
        },
    })
}

/// Convenience builder for a BF4 Astra template without intercept bridging, with
/// partial-multiplane maintenance enabled.
///
/// Used by tests in this repository. Production initialization goes through [`crate::DpfSdk`].
/// which calls `flavor_bf4_astra_with_topology` with the resolved topology, interface inventory.
pub fn flavor_bf4_astra(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
    pf_total_sf: u32,
    extra_bfcfg_parameters: &[String],
    enable_delay_host_init: bool,
) -> Result<DPUFlavorTemplate, crate::error::DpfError> {
    flavor_bf4_astra_with_topology(
        namespace,
        proxy,
        pf_total_sf,
        None,
        None,
        extra_bfcfg_parameters,
        enable_delay_host_init,
        true,
    )
}

/// Production Astra template builder called by the SDK with the resolved topology, interface
/// inventory, and site-level partial-multiplane setting. Topology and inventory may be absent.
///
/// Keeping both inputs here ensures the OVS bootstrap and DHCP ACL consume the same PF/VF
/// projection as the generated ServiceInterfaces and service chains.
/// The DPF operator renders this template for each DPU and creates the resulting DPUFlavor.
#[allow(clippy::too_many_arguments)]
pub(crate) fn flavor_bf4_astra_with_topology(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
    pf_total_sf: u32,
    intercept_bridging: Option<&DpfInterceptBridging>,
    dhcp_acl_interfaces: Option<&[DpuServiceInterfaceTemplateDefinition]>,
    extra_bfcfg_parameters: &[String],
    enable_delay_host_init: bool,
    partial_multiplane_enabled: bool,
) -> Result<DPUFlavorTemplate, crate::error::DpfError> {
    reject_template_delimiters(extra_bfcfg_parameters)?;
    let flavor_spec = DpuFlavorSpec {
        bfcfg_parameters: (!extra_bfcfg_parameters.is_empty())
            .then(|| extra_bfcfg_parameters.to_vec()),
        config_files: Some(get_bf4_astra_config_files(
            proxy,
            dhcp_acl_interfaces,
            partial_multiplane_enabled,
        )?),
        containerd_config: Some(DpuFlavorContainerdConfig {
            registry_endpoint: None,
            sandbox_image: None,
        }),
        dpu_mode: None,
        dpu_resources: None,
        scalable_functions: None,
        virtual_functions: None,
        ew_nic_configurations: Some(bf4_astra_ew_nic_configurations()),
        grub: Some(bf4_astra_grub_params()),
        host_network_interface_configs: None,
        nvconfig: Some(vec![get_bf4_astra_nvconfig(
            pf_total_sf,
            enable_delay_host_init,
        )]),
        ovs: Some(DpuFlavorOvs {
            raw_config_script: Some(get_bf4_astra_ovs_defaults(
                intercept_bridging,
                partial_multiplane_enabled,
            )),
        }),
        packages: Some(vec![]),
        sysctl: Some(DpuFlavorSysctl {
            parameters: Some(vec![]),
        }),
        system_reserved_resources: None,
        systemd_services: Some(vec![]),
        dma: None,
        service_readiness: enable_delay_host_init.then(host_service_readiness),
    };

    let flavor = DPUFlavor {
        metadata: ObjectMeta {
            name: None,
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: flavor_spec,
    };
    flavor_template_from_flavor(&flavor)
}

/// Default grub kernel parameters for the BF4 flavor.
pub fn bf4_grub_params() -> DpuFlavorGrub {
    DpuFlavorGrub {
        kernel_parameters: Some(
            vec![
                "console=hvc0",
                "console=ttyAMA0",
                "net.ifnames=0",
                "biosdevname=0",
                "iommu.passthrough=1",
                "cgroup_no_v1=net_prio,net_cls",
                "hugepagesz=2048kB",
                "hugepages=250",
            ]
            .into_iter()
            .map(|x| x.to_string())
            .collect(),
        ),
    }
}

/// Default grub kernel parameters for the BF4 astra flavor.
pub fn bf4_astra_grub_params() -> DpuFlavorGrub {
    DpuFlavorGrub {
        kernel_parameters: Some(
            vec![
                "console=hvc0",
                "console=ttyAMA0",
                "fixrttc",
                "net.ifnames=0",
                "biosdevname=0",
                "iommu.passthrough=1",
                "cgroup_no_v1=net_prio,net_cls",
                "hugepagesz=2048kB",
                "hugepages=8072",
            ]
            .into_iter()
            .map(|x| x.to_string())
            .collect(),
        ),
    }
}

fn bf4_astra_ew_nic_configurations() -> Vec<DpuFlavorEwNicConfigurations> {
    vec![DpuFlavorEwNicConfigurations {
        force: None,
        link_type: None,
        network_bay: Some(DpuFlavorEwNicConfigurationsNetworkBay {
            conf: "conf1".to_string(),
        }),
        num_vfs: 1,
        raw_nv_config: Some(vec![
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "BOARD_CONFIGURATION_MODE".to_string(),
                value: "0".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "LOAD_BALANCE_MODE_P1".to_string(),
                value: "2".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "LAG_RESOURCE_ALLOCATION".to_string(),
                value: "1".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "FLEX_PARSER_PROFILE_ENABLE".to_string(),
                value: "10".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "RDE_DISABLE".to_string(),
                value: "1".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "VF_LOG_BAR_SIZE".to_string(),
                value: "5".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "SRIOV_EN".to_string(),
                value: "1".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "NUM_OF_VFS".to_string(),
                value: "1".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "KEEP_ETH_LINK_UP_P1".to_string(),
                value: "0".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "ROCE_ADAPTIVE_ROUTING_EN".to_string(),
                value: "1".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "USER_PROGRAMMABLE_CC".to_string(),
                value: "1".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "TX_SCHEDULER_LOCALITY_MODE".to_string(),
                value: "2".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "ROCE_RTT_RESP_DSCP_P1".to_string(),
                value: "48".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "ROCE_RTT_RESP_DSCP_MODE_P1".to_string(),
                value: "1".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "ROCE_CC_STEERING_EXT".to_string(),
                value: "2".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "NUM_OF_PLANES_P1".to_string(),
                value: "4".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "LINK_TYPE_P1".to_string(),
                value: "2".to_string(),
            },
            DpuFlavorEwNicConfigurationsRawNvConfig {
                name: "HIDE_PORT2_PF".to_string(),
                value: "1".to_string(),
            },
        ]),
        spectrum_x_optimized: Some(DpuFlavorEwNicConfigurationsSpectrumXOptimized {
            enabled: true,
            multiplane_mode: Some(
                DpuFlavorEwNicConfigurationsSpectrumXOptimizedMultiplaneMode::Hwplb,
            ),
            number_of_planes: Some(4),
            overlay: Some(DpuFlavorEwNicConfigurationsSpectrumXOptimizedOverlay::None),
            version: "RA2.2-runtime".to_string(),
        }),
    }]
}

/// Build the default DPUFlavor spec. If `proxy` is set, a containerd proxy drop-in config file
/// is appended so the DPU can pull images through the proxy.
///
/// Returns `ConfigError` if any proxy string contains characters that would break the generated
/// systemd `Environment="..."` lines (quotes, newlines, or other control characters).
///
/// `metadata.name` is left unset; callers must set it (typically via [`DPUFlavor::unique_name`])
/// before creating the resource in the cluster.
pub fn default_flavor(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
) -> Result<DPUFlavor, crate::error::DpfError> {
    default_flavor_with_topology(
        namespace,
        proxy,
        DpuDeploymentType::Bf3,
        DEFAULT_DPU_NUM_OF_VFS,
        DEFAULT_PF_TOTAL_SF_RESERVED,
        None,
        None,
        ServiceVpcSlots::default(),
        &[],
        false,
    )
}

/// Builds BF3 flavor state from the validated site VF count and intercept-bridging topology.
// Each argument is an independent site input; a struct would move the same list one level out.
#[allow(clippy::too_many_arguments)]
fn default_flavor_with_topology(
    namespace: &str,
    proxy: &Option<DpfProxyDetails>,
    deployment_type: DpuDeploymentType,
    num_of_vfs: u32,
    pf_total_sf: u32,
    intercept_bridging: Option<&DpfInterceptBridging>,
    dhcp_acl_interfaces: Option<&[DpuServiceInterfaceTemplateDefinition]>,
    service_vpc_slots: ServiceVpcSlots,
    extra_bfcfg_parameters: &[String],
    enable_delay_host_init: bool,
) -> Result<DPUFlavor, crate::error::DpfError> {
    reject_template_delimiters(extra_bfcfg_parameters)?;
    let mut bfcfg_parameters = vec![
        "UPDATE_ATF_UEFI=yes".to_string(),
        "UPDATE_DPU_OS=yes".to_string(),
        "WITH_NIC_FW_UPDATE=yes".to_string(),
    ];
    bfcfg_parameters.extend_from_slice(extra_bfcfg_parameters);
    Ok(DPUFlavor {
        metadata: ObjectMeta {
            name: None,
            namespace: Some(namespace.to_string()),
            ..Default::default()
        },
        spec: DpuFlavorSpec {
            dpu_mode: Some(DpuFlavorDpuMode::ZeroTrust),
            dpu_resources: None,
            scalable_functions: None,
            virtual_functions: None,
            bfcfg_parameters: Some(bfcfg_parameters),
            config_files: Some(get_config_files(
                proxy,
                deployment_type,
                dhcp_acl_interfaces,
            )?),
            containerd_config: None,
            grub: Some(get_default_grub()),
            host_network_interface_configs: None,
            nvconfig: Some(vec![get_bf3_nvconfig(
                num_of_vfs,
                pf_total_sf,
                deployment_type,
                enable_delay_host_init,
            )]),
            ovs: Some(crate::crds::dpuflavors_generated::DpuFlavorOvs {
                raw_config_script: Some(get_default_ovs_defaults_with_topology(
                    intercept_bridging,
                    service_vpc_slots,
                )),
            }),
            sysctl: None,
            system_reserved_resources: None,
            ew_nic_configurations: None,
            packages: None,
            // rawConfigScript sets the value during provisioning. Retain this ordered oneshot so
            // DPF versions with systemdServices support also enforce it after network readiness.
            systemd_services: Some(vec![ovn_encap_systemd_service()]),
            dma: None,
            service_readiness: enable_delay_host_init.then(host_service_readiness),
        },
    })
}

fn get_default_grub() -> DpuFlavorGrub {
    DpuFlavorGrub {
        kernel_parameters: Some(
            vec![
                "console=hvc0",
                "console=ttyAMA0",
                "earlycon=pl011,0x13010000",
                "fixrttc",
                "net.ifnames=0",
                "biosdevname=0",
                "iommu.passthrough=1",
                "cgroup_no_v1=net_prio,net_cls",
                "hugepagesz=2048kB",
                "hugepages=3072",
            ]
            .into_iter()
            .map(|x| x.to_string())
            .collect(),
        ),
    }
}

/// Returns HBN's DPF reference AppArmor extensions.
///
/// The rsyslog policy permits the complete rotation chain, while the tcpdump policy accepts
/// signals from `runc`. Cloud-init writes both before DPF's provisioning reboot loads the profiles.
/// Since these files are part of the flavor hash, changing them reprovisions every DPF DPU.
fn hbn_apparmor_config_files() -> [DpuFlavorConfigFiles; 2] {
    [
        DpuFlavorConfigFiles {
            path: "/etc/apparmor.d/local/usr.sbin.rsyslogd".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(
                concat!(
                    "signal (receive) peer=runc,\n",
                    "capability chown,\n",
                    "/usr/{bin,sbin}/* ixr,\n",
                    "/etc/logrotate.d/* rk,\n",
                    "/var/lib/logrotate/{,**} rwk,\n",
                )
                .to_string(),
            ),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/apparmor.d/local/usr.bin.tcpdump".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some("signal (receive) peer=runc,\n".to_string()),
            content_from: None,
            r#type: None,
        },
    ]
}

/// Returns the base set of config files, plus an optional containerd proxy drop-in if `proxy` is set.
///
/// `deployment_type` selects the few settings that differ between the deployments sharing this
/// base set (both BF3 variants and BF4 generic); [`get_bf4_astra_config_files`] builds the BF4
/// Astra set.
fn get_config_files(
    proxy: &Option<DpfProxyDetails>,
    deployment_type: DpuDeploymentType,
    dhcp_acl_interfaces: Option<&[DpuServiceInterfaceTemplateDefinition]>,
) -> Result<Vec<DpuFlavorConfigFiles>, crate::error::DpfError> {
    let mut mlnx_bf_conf = concat!(
        "ALLOW_SHARED_RQ=\"no\"\n",
        "IPSEC_FULL_OFFLOAD=\"no\"\n",
        "ENABLE_ESWITCH_MULTIPORT=\"yes\"\n"
    )
    .to_string();
    if matches!(deployment_type, DpuDeploymentType::Bf4Generic) {
        mlnx_bf_conf.push_str("SNAP_DMA_SF=\"no\"\n");
    }

    let mut config_files = vec![
        DpuFlavorConfigFiles {
            path: "/var/lib/hbn/etc/supervisor/conf.d/acltool.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(
                concat!(
                    "[program: cl-acltool]\n",
                    "command = bash -c \"sleep 5 && ",
                    "/usr/cumulus/bin/cl-acltool -i\"\n",
                    "startsecs = 0\n",
                    "autorestart = false\n",
                    "priority = 200\n",
                )
                .to_string(),
            ),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/var/lib/hbn/etc/cumulus/acl/policy.d/10-dhcp.rules".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(dhcp_acl_rules(dhcp_acl_interfaces)),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/lldpd.d/lldp-interfaces.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some("configure system interface pattern *\n".to_string()),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/default/lldpd".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some("DAEMON_ARGS=\"-M 1\"\n".to_string()),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/mellanox/mlnx-bf.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(mlnx_bf_conf),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/mellanox/mlnx-ovs.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(concat!("CREATE_OVS_BRIDGES=\"no\"\n", "OVS_DOCA=\"yes\"\n").to_string()),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/mellanox/mlnx-sf.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some("".to_string()),
            content_from: None,
            r#type: None,
        },
    ];

    config_files.extend(hbn_apparmor_config_files());
    config_files.extend(ovn_encap_config_files());

    if let Some(proxy) = proxy {
        validate_proxy_string(&proxy.https_proxy, "https_proxy")?;

        let mut raw = format!(
            "[Service]\nEnvironment=\"HTTPS_PROXY={0}\"\nEnvironment=\"https_proxy={0}\"\n",
            proxy.https_proxy
        );
        let mut entries: Vec<&str> = proxy
            .no_proxy
            .iter()
            .map(|e| e.trim())
            .filter(|e| !e.is_empty())
            .collect();
        if !entries.is_empty() {
            for entry in &entries {
                validate_proxy_string(entry, "no_proxy entry")?;
            }
            entries.sort_unstable();
            entries.dedup();
            let no_proxy = entries.join(",");
            raw.push_str(&format!(
                "Environment=\"NO_PROXY={0}\"\nEnvironment=\"no_proxy={0}\"\n",
                no_proxy
            ));
        }
        config_files.push(DpuFlavorConfigFiles {
            path: "/etc/systemd/system/containerd.service.d/socks-proxy.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(raw),
            content_from: None,
            r#type: None,
        });
    }

    // ROLLOUT SAFETY: these entries change the flavor hash, so adding them
    // reprovisions every existing BF4 DPU once. ConfigMap edits do not.
    if deployment_type == DpuDeploymentType::Bf4Generic {
        config_files.push(DpuFlavorConfigFiles {
            content_from: Some(DpuFlavorConfigFilesContentFrom {
                config_map_key_ref: Some(DpuFlavorConfigFilesContentFromConfigMapKeyRef {
                    name: Some("extra-script-pre-ovs-bf4-generic".to_string()),
                    key: "script".to_string(),
                    optional: None,
                }),
            }),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/opt/dpf/extra-script-pre-ovs.sh".to_string(),
            permissions: Some("0755".to_string()),
            raw: None,
            r#type: Some(DpuFlavorConfigFilesType::AgentApplied),
        });
        config_files.push(DpuFlavorConfigFiles {
            // CRD allows exactly one of `raw` and `contentFrom`.
            content_from: Some(DpuFlavorConfigFilesContentFrom {
                config_map_key_ref: Some(DpuFlavorConfigFilesContentFromConfigMapKeyRef {
                    name: Some("extra-script-post-ovs-bf4-generic".to_string()),
                    key: "script".to_string(),
                    optional: None,
                }),
            }),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/opt/dpf/extra-script-post-ovs.sh".to_string(),
            permissions: Some("0755".to_string()),
            raw: None,
            r#type: Some(DpuFlavorConfigFilesType::AgentApplied),
        });
    }

    Ok(config_files)
}

/// Returns the provisioning files that set `ovn-encap-ip` after networking and OVS.
fn ovn_encap_config_files() -> Vec<DpuFlavorConfigFiles> {
    vec![
        DpuFlavorConfigFiles {
            path: OVN_ENCAP_SCRIPT_PATH.to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0755".to_string()),
            // Keep the oneshot body identical to the provisioning-time rawConfigScript action.
            raw: Some(format!(
                "#!/bin/bash\nset -euo pipefail\n{}",
                ovn_encap_ip_commands()
            )),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: format!("/etc/systemd/system/{OVN_ENCAP_SERVICE_NAME}"),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(format!(
                concat!(
                    "[Unit]\n",
                    "Description=Configure OVN tunnel encapsulation address\n",
                    "Wants=network-online.target\n",
                    "After=network-online.target openvswitch-switch.service openvswitch.service\n",
                    "\n",
                    "[Service]\n",
                    "Type=oneshot\n",
                    "ExecStart={}\n",
                    "RemainAfterExit=yes\n",
                    "\n",
                    "[Install]\n",
                    "WantedBy=multi-user.target\n",
                ),
                OVN_ENCAP_SCRIPT_PATH
            )),
            content_from: None,
            r#type: None,
        },
    ]
}

/// Shell commands shared by provisioning-time OVS bootstrap and the retained systemd oneshot.
fn ovn_encap_ip_commands() -> &'static str {
    concat!(
        "mapfile -t oob_ipv4_addresses < <(\n",
        "    ip -4 -o address show dev oob_net0 scope global |\n",
        "        awk '{sub(/\\/.*/, \"\", $4); if (NF) print $4}'\n",
        ")\n",
        "if (( ${#oob_ipv4_addresses[@]} != 1 )); then\n",
        "    echo \"expected exactly one global IPv4 address on oob_net0; found ${#oob_ipv4_addresses[@]}\" >&2\n",
        "    exit 1\n",
        "fi\n",
        "ovs-vsctl --timeout 15 set Open_vSwitch . \"external_ids:ovn-encap-ip=${oob_ipv4_addresses[0]}\"\n",
    )
}

/// Returns the DPUFlavor request that enables and starts the OVN address oneshot.
fn ovn_encap_systemd_service() -> DpuFlavorSystemdServices {
    DpuFlavorSystemdServices {
        name: OVN_ENCAP_SERVICE_NAME.to_string(),
        operation: DpuFlavorSystemdServicesOperation::EnableAndStart,
    }
}

/// Builds generic-BF4 nvconfig with the validated site VF population.
fn get_bf4_nvconfig(
    num_of_vfs: u32,
    pf_total_sf: u32,
    enable_delay_host_init: bool,
) -> DpuFlavorNvconfig {
    let mut parameters = vec![
        "PF_BAR2_ENABLE=0".to_string(),
        "PER_PF_NUM_SF=1".to_string(),
        format!("PF_TOTAL_SF={pf_total_sf}"),
        "PF_SF_BAR_SIZE=14".to_string(),
        "NUM_PF_MSIX_VALID=0".to_string(),
        "PF_NUM_PF_MSIX_VALID=1".to_string(),
        "PF_NUM_PF_MSIX=228".to_string(),
        "INTERNAL_CPU_MODEL=1".to_string(),
        "INTERNAL_CPU_OFFLOAD_ENGINE=0".to_string(),
        "SRIOV_EN=1".to_string(),
        "LAG_RESOURCE_ALLOCATION=1".to_string(),
        format!("NUM_OF_VFS={num_of_vfs}"),
        "LINK_TYPE_P1=ETH".to_string(),
        "LINK_TYPE_P2=ETH".to_string(),
    ];
    if enable_delay_host_init {
        parameters.push(DELAY_HOST_OS_INIT_PARAMETER.to_string());
    }

    DpuFlavorNvconfig {
        // DPF does not allow anyother wild card. It takes only '*'
        device: Some(DpuFlavorNvconfigDevice::KopiumVariant0), //"*"
        parameters: Some(parameters),
        force: None,
    }
}

/// Returns Astra config files, deriving DHCP ACLs from a configured effective interface inventory.
///
/// An absent inventory retains the legacy ACL verbatim, which keeps topology-free template hashes
/// stable. A configured inventory limits the ACL to the PF/VFs selected for interception.
fn get_bf4_astra_config_files(
    proxy: &Option<DpfProxyDetails>,
    dhcp_acl_interfaces: Option<&[DpuServiceInterfaceTemplateDefinition]>,
    partial_multiplane_enabled: bool,
) -> Result<Vec<DpuFlavorConfigFiles>, crate::error::DpfError> {
    let mut config_files = vec![
        DpuFlavorConfigFiles {
            path: "/var/lib/hbn/etc/supervisor/conf.d/acltool.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(
                concat!(
                    "[program: cl-acltool]\n",
                    "command = bash -c \"sleep 5 && ",
                    "/usr/cumulus/bin/cl-acltool -i\"\n",
                    "startsecs = 0\n",
                    "autorestart = false\n",
                    "priority = 200\n",
                )
                .to_string(),
            ),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/var/lib/hbn/etc/cumulus/acl/policy.d/10-dhcp.rules".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(dhcp_acl_rules(dhcp_acl_interfaces)),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/mellanox/interface-fixed-mapping.csv".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(interface_fixed_mapping_raw()),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/lldpd.d/lldp-interfaces.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some("configure system interface pattern *\n".to_string()),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            path: "/etc/default/lldpd".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some("DAEMON_ARGS=\"-M 1\"\n".to_string()),
            content_from: None,
            r#type: None,
        },
        DpuFlavorConfigFiles {
            content_from: None,
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/etc/mellanox/mlnx-bf.conf".to_string(),
            permissions: Some("0644".to_string()),
            raw: Some(
                concat!(
                    "ALLOW_SHARED_RQ=\"no\"\n",
                    "IPSEC_FULL_OFFLOAD=\"no\"\n",
                    "ENABLE_ESWITCH_MULTIPORT=\"yes\"\n",
                    "SNAP_DMA_SF=\"no\"\n",
                )
                .to_string(),
            ),
            r#type: None,
        },
        DpuFlavorConfigFiles {
            content_from: None,
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/etc/mellanox/mlnx-ovs.conf".to_string(),
            permissions: Some("0644".to_string()),
            raw: Some(concat!("CREATE_OVS_BRIDGES=\"no\"\n", "OVS_DOCA=\"yes\"\n",).to_string()),
            r#type: None,
        },
        DpuFlavorConfigFiles {
            content_from: None,
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/etc/mellanox/mlnx-sf.conf".to_string(),
            permissions: Some("0644".to_string()),
            raw: Some(String::new()),
            r#type: None,
        },
        DpuFlavorConfigFiles {
            content_from: Some(DpuFlavorConfigFilesContentFrom {
                config_map_key_ref: Some(DpuFlavorConfigFilesContentFromConfigMapKeyRef {
                    name: Some("ra2.2-runtime".to_string()),
                    key: "RA2.2-runtime.yaml".to_string(),
                    optional: None,
                }),
            }),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/bindata/spectrum-x/RA2.2-runtime.yaml".to_string(),
            permissions: Some("0644".to_string()),
            raw: None,
            r#type: Some(DpuFlavorConfigFilesType::AgentApplied),
        },
        DpuFlavorConfigFiles {
            content_from: None,
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/etc/mellanox/ovs-script.sh".to_string(),
            permissions: Some("0755".to_string()),
            raw: Some(ovs_script_raw()),
            r#type: None,
        },
        DpuFlavorConfigFiles {
            content_from: None,
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/usr/local/sbin/configure-xplane-network.sh".to_string(),
            permissions: Some("0755".to_string()),
            raw: Some(configure_xplane_network_script_raw()),
            r#type: None,
        },
        DpuFlavorConfigFiles {
            content_from: Some(DpuFlavorConfigFilesContentFrom {
                config_map_key_ref: Some(DpuFlavorConfigFilesContentFromConfigMapKeyRef {
                    name: Some("extra-script-pre-ovs-bf4-astra".to_string()),
                    key: "script".to_string(),
                    optional: None,
                }),
            }),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/opt/dpf/extra-script-pre-ovs.sh".to_string(),
            permissions: Some("0755".to_string()),
            raw: None,
            r#type: Some(DpuFlavorConfigFilesType::AgentApplied),
        },
        DpuFlavorConfigFiles {
            // CRD allows exactly one of `raw` and `contentFrom`.
            content_from: Some(DpuFlavorConfigFilesContentFrom {
                config_map_key_ref: Some(DpuFlavorConfigFilesContentFromConfigMapKeyRef {
                    name: Some("extra-script-post-ovs-bf4-astra".to_string()),
                    key: "script".to_string(),
                    optional: None,
                }),
            }),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/opt/dpf/extra-script-post-ovs.sh".to_string(),
            permissions: Some("0755".to_string()),
            raw: None,
            r#type: Some(DpuFlavorConfigFilesType::AgentApplied),
        },
    ];

    if partial_multiplane_enabled {
        config_files.push(DpuFlavorConfigFiles {
            content_from: None,
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            path: "/usr/local/sbin/initialize-spx-maintenance.sh".to_string(),
            permissions: Some("0755".to_string()),
            raw: Some(initialize_spx_maintenance_script_raw()),
            r#type: None,
        });
    }

    config_files.extend(hbn_apparmor_config_files());

    if let Some(proxy) = proxy {
        validate_proxy_string(&proxy.https_proxy, "https_proxy")?;

        let mut raw = format!(
            "[Service]\nEnvironment=\"HTTPS_PROXY={0}\"\nEnvironment=\"https_proxy={0}\"\n",
            proxy.https_proxy
        );
        let mut entries: Vec<&str> = proxy
            .no_proxy
            .iter()
            .map(|e| e.trim())
            .filter(|e| !e.is_empty())
            .collect();
        if !entries.is_empty() {
            for entry in &entries {
                validate_proxy_string(entry, "no_proxy entry")?;
            }
            entries.sort_unstable();
            entries.dedup();
            let no_proxy = entries.join(",");
            raw.push_str(&format!(
                "Environment=\"NO_PROXY={0}\"\nEnvironment=\"no_proxy={0}\"\n",
                no_proxy
            ));
        }
        config_files.push(DpuFlavorConfigFiles {
            path: "/etc/systemd/system/containerd.service.d/socks-proxy.conf".to_string(),
            operation: Some(DpuFlavorConfigFilesOperation::Override),
            permissions: Some("0644".to_string()),
            raw: Some(raw),
            content_from: None,
            r#type: None,
        });
    }

    Ok(config_files)
}

/// Builds BF3 NVConfig with the validated site VF population and platform profile.
///
/// GB200 deliberately replaces the validated SF-capacity result with its platform-fixed
/// `PF_TOTAL_SF=128`.
fn get_bf3_nvconfig(
    num_of_vfs: u32,
    pf_total_sf: u32,
    deployment_type: DpuDeploymentType,
    enable_delay_host_init: bool,
) -> DpuFlavorNvconfig {
    let pf_total_sf = if deployment_type == DpuDeploymentType::Bf3Gb200 {
        GB200_B3240_V1_PF_TOTAL_SF
    } else {
        pf_total_sf
    };
    let mut parameters = vec![
        "PF_BAR2_ENABLE=0".to_string(),
        "PER_PF_NUM_SF=1".to_string(),
        format!("PF_TOTAL_SF={pf_total_sf}"),
        "PF_SF_BAR_SIZE=10".to_string(),
        "NUM_PF_MSIX_VALID=0".to_string(),
        "PF_NUM_PF_MSIX_VALID=1".to_string(),
        "PF_NUM_PF_MSIX=228".to_string(),
        "INTERNAL_CPU_MODEL=1".to_string(),
        "INTERNAL_CPU_OFFLOAD_ENGINE=0".to_string(),
        "SRIOV_EN=1".to_string(),
        "LAG_RESOURCE_ALLOCATION=1".to_string(),
        format!("NUM_OF_VFS={num_of_vfs}"),
        "HIDE_PORT2_PF=True".to_string(),
        "NUM_OF_PF=1".to_string(),
        "LINK_TYPE_P1=ETH".to_string(),
        "LINK_TYPE_P2=ETH".to_string(),
    ];
    if enable_delay_host_init {
        parameters.push(DELAY_HOST_OS_INIT_PARAMETER.to_string());
    }

    if deployment_type == DpuDeploymentType::Bf3Gb200 {
        let configured_parameter_names = parameters
            .iter()
            .map(|parameter| nvconfig_parameter_name(parameter).to_string())
            .collect::<BTreeSet<_>>();

        // These two assignments only restate firmware defaults, so omit them to keep the profile
        // minimal. Values already present in the BF3 base stay in their native DPF representation.
        parameters.extend(
            DpuNvConfigProfile::Gb200B3240V1
                .parameters()
                .iter()
                .filter(|parameter| {
                    !configured_parameter_names.contains(nvconfig_parameter_name(parameter))
                        && !matches!(
                            **parameter,
                            "PCI_SWITCH0_UPSTREAM_PORT_BUS=0" | "PCI_SWITCH0_UPSTREAM_PORT_PEX=0"
                        )
                })
                .map(|parameter| (*parameter).to_string()),
        );
    }

    DpuFlavorNvconfig {
        // DPF does not allow anyother wild card. It takes only '*'
        device: Some(DpuFlavorNvconfigDevice::KopiumVariant0), //"*"
        parameters: Some(parameters),
        force: None,
    }
}

fn nvconfig_parameter_name(parameter: &str) -> &str {
    parameter
        .split_once('=')
        .map_or(parameter, |(name, _)| name)
}

fn get_bf4_astra_nvconfig(pf_total_sf: u32, enable_delay_host_init: bool) -> DpuFlavorNvconfig {
    let mut parameters = vec![
        "PF_BAR2_ENABLE=0".to_string(),
        "PER_PF_NUM_SF=1".to_string(),
        format!("PF_TOTAL_SF={pf_total_sf}"),
        "PF_SF_BAR_SIZE=14".to_string(),
        "NUM_PF_MSIX_VALID=0".to_string(),
        "PF_NUM_PF_MSIX_VALID=1".to_string(),
        "PF_NUM_PF_MSIX=228".to_string(),
        "INTERNAL_CPU_MODEL=1".to_string(),
        "INTERNAL_CPU_OFFLOAD_ENGINE=0".to_string(),
        "SRIOV_EN=1".to_string(),
        "NUM_OF_VFS=46".to_string(),
        "LAG_RESOURCE_ALLOCATION=1".to_string(),
        "LINK_TYPE_P1=ETH".to_string(),
        "LINK_TYPE_P2=ETH".to_string(),
    ];
    if enable_delay_host_init {
        parameters.push(DELAY_HOST_OS_INIT_PARAMETER.to_string());
    }

    DpuFlavorNvconfig {
        // DPF does not allow anyother wild card. It takes only '*'
        device: Some(DpuFlavorNvconfigDevice::KopiumVariant0), //"*"
        parameters: Some(parameters),
        force: None,
    }
}

/// Renders DHCP broadcast drops for the host-facing interfaces in the effective inventory.
fn dhcp_acl_rules(interfaces: Option<&[DpuServiceInterfaceTemplateDefinition]>) -> String {
    let mut rules = String::from("[iptables]\n");
    let mut append_rule = |interface_name: &str| {
        rules.push_str(&format!(
            "-t filter -A FORWARD -p udp -d 255.255.255.255 \
             --dport 67 -m physdev --physdev-in {interface_name} \
             -m comment --comment 'offload:0' -j DROP\n"
        ));
    };

    if let Some(interfaces) = interfaces {
        // Configured PF/VF identities are represented as Patch interfaces. Read their HBN endpoint
        // names directly so sparse inventories receive rules for exactly their selected endpoints.
        for interface_name in interfaces
            .iter()
            .filter(|interface| {
                matches!(
                    &interface.iface_type,
                    DpuServiceInterfaceTemplateType::Patch(_)
                )
            })
            .flat_map(|interface| interface.chained_svc_if.iter().flatten())
            .filter_map(|(service_name, interface_name)| {
                (service_name == DOCA_HBN_SERVICE_NAME).then_some(interface_name)
            })
        {
            append_rule(interface_name);
        }
    } else {
        // ROLLOUT COMPATIBILITY (DPU REPROVISIONING): retain the byte-for-byte legacy ACL for
        // inventory-free BF3/BF4 flavors. Deriving it from the smaller static ServiceInterface
        // inventory would alter every existing flavor hash even though their policy is unchanged.
        append_rule("pf0hpf_if");
        for vf_id in 0..=15 {
            append_rule(&format!("pf0vf{vf_id}_if"));
        }
    }
    rules
}

// Fixed interface mappings for VR200
fn interface_fixed_mapping_raw() -> String {
    concat!(
        "pci_address,phys_port_name,cerebro_interface_name\n",
        "0005:03:00.0,p0,C1-1-L1\n",
        "0005:03:00.1,p1,C1-1-L2\n",
        "0005:03:00.2,p2,C1-1-L3\n",
        "0005:03:00.3,p3,C1-1-L4\n",
        "0005:06:00.0,p0,C1-2-L1\n",
        "0005:06:00.1,p1,C1-2-L2\n",
        "0005:06:00.2,p2,C1-2-L3\n",
        "0005:06:00.3,p3,C1-2-L4\n",
        "0004:03:00.0,p0,C3-1-L1\n",
        "0004:03:00.1,p1,C3-1-L2\n",
        "0004:03:00.2,p2,C3-1-L3\n",
        "0004:03:00.3,p3,C3-1-L4\n",
        "0004:06:00.0,p0,C3-2-L1\n",
        "0004:06:00.1,p1,C3-2-L2\n",
        "0004:06:00.2,p2,C3-2-L3\n",
        "0004:06:00.3,p3,C3-2-L4\n",
        "0001:03:00.0,p0,C5-1-L1\n",
        "0001:03:00.1,p1,C5-1-L2\n",
        "0001:03:00.2,p2,C5-1-L3\n",
        "0001:03:00.3,p3,C5-1-L4\n",
        "0001:06:00.0,p0,C5-2-L1\n",
        "0001:06:00.1,p1,C5-2-L2\n",
        "0001:06:00.2,p2,C5-2-L3\n",
        "0001:06:00.3,p3,C5-2-L4\n",
        "0000:03:00.0,p0,C7-1-L1\n",
        "0000:03:00.1,p1,C7-1-L2\n",
        "0000:03:00.2,p2,C7-1-L3\n",
        "0000:03:00.3,p3,C7-1-L4\n",
        "0000:06:00.0,p0,C7-2-L1\n",
        "0000:06:00.1,p1,C7-2-L2\n",
        "0000:06:00.2,p2,C7-2-L3\n",
        "0000:06:00.3,p3,C7-2-L4\n",
    )
    .to_string()
}

fn ovs_script_raw() -> String {
    // Keep YAML block indentation and place the selected script's shebang on this line.
    [
        "{{ if and .dpu_device_underlay_rows (index .dpu_device_underlay_rows 0).cerebro_ifname }}",
        ovs_cerebro_script_raw(),
        "{{ else }}",
        ovs_legacy_script_raw(),
        "{{ end }}\n",
    ]
    .concat()
}

fn ovs_cerebro_script_raw() -> &'static str {
    indoc::indoc! {r#"
        #!/bin/bash

        # Remove default OVS configuration on the DPU and ensure no leftovers on the OVS kernel side
        seq -f 'ovsbr%g' 1 99 | xargs -r -n1 ovs-vsctl --if-exists del-br

        ovs-appctl --timeout 15 dpctl/del-dp system@ovs-system || true

        # Configure OVS
        _ovs-vsctl set Open_vSwitch . other_config:doca-init=true
        _ovs-vsctl set Open_vSwitch . other_config:dpdk-max-memzones=50000
        _ovs-vsctl set Open_vSwitch . other_config:hw-offload=true
        _ovs-vsctl set Open_vSwitch . other_config:pmd-quiet-idle=true
        _ovs-vsctl set Open_vSwitch . other_config:max-idle=20000
        _ovs-vsctl set Open_vSwitch . other_config:max-revalidator=5000
        _ovs-vsctl set Open_vSwitch . other_config:doca-congestion-threshold=60
        _ovs-vsctl set Open_vSwitch . other_config:flow-limit=500000
        _ovs-vsctl set Open_vSwitch . other_config:hw-offload-ct-unidir-udp-enabled=true
        _ovs-vsctl remove Open_vSwitch . other_config default-datapath-type || true

        if systemctl list-unit-files openvswitch-switch.service &>/dev/null; then
          systemctl restart openvswitch-switch
        elif systemctl list-unit-files openvswitch.service &>/dev/null; then
          systemctl restart openvswitch
        fi

        # Add br-sfc bridge
        _ovs-vsctl --may-exist add-br br-sfc
        _ovs-vsctl set bridge br-sfc datapath_type=netdev
        _ovs-vsctl set bridge br-sfc fail_mode=secure

        # Add br-hbn bridge
        _ovs-vsctl --if-exists del-br br-hbn
        _ovs-vsctl --may-exist add-br br-hbn
        _ovs-vsctl set bridge br-hbn datapath_type=netdev
        _ovs-vsctl set bridge br-hbn fail_mode=secure

        # Pre plug p0 to br-sfc
        _ovs-vsctl --may-exist add-port br-sfc p0
        _ovs-vsctl set Interface p0 type=dpdk
        _ovs-vsctl set Interface p0 mtu_request=9216
        _ovs-vsctl set Port p0 external_ids:dpf-type=physical

        # Pre plug p1 to br-sfc
        _ovs-vsctl --may-exist add-port br-sfc p1
        _ovs-vsctl set Interface p1 type=dpdk
        _ovs-vsctl set Interface p1 mtu_request=9216
        _ovs-vsctl set Port p1 external_ids:dpf-type=physical

        # Add br-xplane bridge
        _ovs-vsctl --may-exist add-br br-xplane
        _ovs-vsctl set bridge br-xplane datapath_type=netdev
        _ovs-vsctl set bridge br-xplane fail_mode=secure

        # Add brcx-<netdev prefix> bridges and add A<x>p<y> netdev ports
        # to br-xplane bridge with the netdev prefix as the group ID.
        RAILS=(0 1 2 3)
        SW_PLANES=(0 1)
        HW_PLANES=(0 1 2 3)
        for rail in "${RAILS[@]}"; do
          for sw_plane in "${SW_PLANES[@]}"; do
            logical_ifname="eth_r${rail}_p$((4 * sw_plane))"
            netdev_name="${NETDEV_BY_INTERFACES[$logical_ifname]:-}"
            if [[ ! "$netdev_name" =~ ^(.+)p[0-9]+$ ]]; then
              echo "ovs-script.sh: netdev $netdev_name for $logical_ifname has no p<port> suffix" >&2
              return 1
            fi
            bridge="brcx-${BASH_REMATCH[1]}"
            _ovs-vsctl --may-exist add-br "$bridge"
            _ovs-vsctl set bridge "$bridge" datapath_type=netdev
            _ovs-vsctl set bridge "$bridge" fail_mode=standalone
            _ovs-vsctl set Interface "$bridge" mtu_request=9216
            for hw_plane in "${HW_PLANES[@]}"; do
              logical_ifname="eth_r${rail}_p$((4*${sw_plane}+${hw_plane}))"
              netdev_name="${NETDEV_BY_INTERFACES[$logical_ifname]:-}"
              if [ -z "$netdev_name" ]; then
                echo "ovs-script.sh: no discovered netdev for interface ${logical_ifname}" >&2
                return 1
              fi

              _ovs-vsctl --may-exist add-port br-xplane "$netdev_name"
              _ovs-vsctl set Interface "$netdev_name" type=dpdk
              _ovs-vsctl set Interface "$netdev_name" mtu_request=9216
              _ovs-vsctl set Interface "$netdev_name" external_ids:xplane=true
              # Set the group id to match the downlink patch's physical NIC group ID, e.g. A56p2 -> A56.
              _ovs-vsctl set Interface "$netdev_name" external_ids:xplane-group-id="${netdev_name%p[0-9]}"
              _ovs-vsctl set Interface "$netdev_name" external_ids:xplane-uplink=true
              _ovs-vsctl set Interface "$netdev_name" external_ids:xplane-plane-id="$hw_plane"
            done
          done
        done

        mst start

        _ovs-vsctl set Open_vSwitch . 'other_config:flow-metric-labels="to_plane,from_plane,device_name,group,plane"' other_config:doca-telemetry-interval="1000" other_config:doca-telemetry-ipc="true" other_config:doca-telemetry-source-id="xplane"
    "#}
}

fn initialize_spx_maintenance_script_raw() -> String {
    [
        "{{ if and .dpu_device_underlay_rows (index .dpu_device_underlay_rows 0).cerebro_ifname }}",
        indoc::indoc! {r#"
            #!/bin/bash
            set -euo pipefail

            # Initialize once per provisioning and add a marker -
            # this marker is intended to survive ordinary reboots but
            # DPF reprovisioning, including re-ingestion after force delete, reinstalls
            # the DPU OS, removes the marker so maintenance is initialized again. Skip initialization if the marker exists.
            state_dir=/var/lib/nico
            marker="$state_dir/spx-maintenance-initialized"
            install -d -m 0755 "$state_dir"
            [[ ! -e "$marker" ]] || exit 0

            # Provisioning has already assigned altnames strictly. Resolve them again
            # instead of reusing NETDEV_BY_INTERFACES to check that the live mappings exist.
            # Check every OVS interface exists before applying one atomic transaction.
            args=()
            for rail in 0 1 2 3; do
              for plane in 0 1 2 3 4 5 6 7; do
                logical="eth_r${rail}_p${plane}"
                netdev=$(ip -j link show dev "$logical" | jq -er '.[0].ifname')
                ovs-vsctl --timeout=30 get Interface "$netdev" name >/dev/null
                args+=(-- set Interface "$netdev" external_ids:spx-arc-maintenance=true)
              done
            done
            ovs-vsctl --timeout=30 "${args[@]}"

            # Failed initialization must be retried; never mark success before OVS commits.
            touch "$marker"
        "#},
        "{{ else }}\n#!/bin/bash\n# Legacy inventory does not support partial-plane maintenance.\nexit 0\n",
        "{{ end }}\n",
    ]
    .concat()
}

fn ovs_legacy_script_raw() -> &'static str {
    indoc::indoc! {r#"
        #!/bin/bash

        # Remove default OVS configuration on the DPU and ensure no leftovers on the OVS kernel side
        seq -f 'ovsbr%g' 1 99 | xargs -r -n1 ovs-vsctl --if-exists del-br

        ovs-appctl --timeout 15 dpctl/del-dp system@ovs-system || true

        # Configure OVS
        _ovs-vsctl set Open_vSwitch . other_config:doca-init=true
        _ovs-vsctl set Open_vSwitch . other_config:dpdk-max-memzones=50000
        _ovs-vsctl set Open_vSwitch . other_config:hw-offload=true
        _ovs-vsctl set Open_vSwitch . other_config:pmd-quiet-idle=true
        _ovs-vsctl set Open_vSwitch . other_config:max-idle=20000
        _ovs-vsctl set Open_vSwitch . other_config:max-revalidator=5000
        _ovs-vsctl set Open_vSwitch . other_config:doca-congestion-threshold=60
        _ovs-vsctl set Open_vSwitch . other_config:flow-limit=500000
        _ovs-vsctl set Open_vSwitch . other_config:hw-offload-ct-unidir-udp-enabled=true
        _ovs-vsctl remove Open_vSwitch . other_config default-datapath-type || true

        if systemctl list-unit-files openvswitch-switch.service &>/dev/null; then
          systemctl restart openvswitch-switch
        elif systemctl list-unit-files openvswitch.service &>/dev/null; then
          systemctl restart openvswitch
        fi

        _ovs-vsctl --may-exist add-br br-sfc
        _ovs-vsctl set bridge br-sfc datapath_type=netdev
        _ovs-vsctl set bridge br-sfc fail_mode=secure

        _ovs-vsctl --if-exists del-br br-hbn
        _ovs-vsctl --may-exist add-br br-hbn
        _ovs-vsctl set bridge br-hbn datapath_type=netdev
        _ovs-vsctl set bridge br-hbn fail_mode=secure

        # Pre plug p0 to br-sfc
        _ovs-vsctl --may-exist add-port br-sfc p0
        _ovs-vsctl set Interface p0 type=dpdk
        _ovs-vsctl set Interface p0 mtu_request=9216
        _ovs-vsctl set Port p0 external_ids:dpf-type=physical

        # Pre plug p1 to br-sfc
        _ovs-vsctl --may-exist add-port br-sfc p1
        _ovs-vsctl set Interface p1 type=dpdk
        _ovs-vsctl set Interface p1 mtu_request=9216
        _ovs-vsctl set Port p1 external_ids:dpf-type=physical

        # Configure xplane bridge.
        _ovs-vsctl --may-exist add-br br-xplane
        _ovs-vsctl set bridge br-xplane datapath_type=netdev
        _ovs-vsctl set bridge br-xplane fail_mode=secure

        # Configure brcx-<interface prefix> bridges using the mappings below.
        # Each row is:
        # interface prefix | PCI address | bridge | xplane group ID
        XPLANE_ROWS=(
            "A56|0005:06:00.0|brcx-A56|A56"
            "A43|0004:03:00.0|brcx-A43|A43"
            "A53|0005:03:00.0|brcx-A53|A53"
            "A46|0004:06:00.0|brcx-A46|A46"
            "A6|0000:06:00.0|brcx-A6|A6"
            "A13|0001:03:00.0|brcx-A13|A13"
            "A3|0000:03:00.0|brcx-A3|A3"
            "A16|0001:06:00.0|brcx-A16|A16"
        )
        HW_PLANES=(0 1 2 3)

        for row in "${XPLANE_ROWS[@]}"; do
            IFS='|' read -r iface_prefix pci_address bridge group_id <<< "$row"
            bridge="brcx-${iface_prefix}"
            _ovs-vsctl --may-exist add-br "$bridge"
            _ovs-vsctl set bridge "$bridge" datapath_type=netdev
            _ovs-vsctl set bridge "$bridge" fail_mode=standalone
            for hw_plane in "${HW_PLANES[@]}"; do
                interface_val="${iface_prefix}p${hw_plane}"

                _ovs-vsctl --may-exist add-port br-xplane "$interface_val"
                _ovs-vsctl set Interface "$interface_val" type=dpdk
                _ovs-vsctl set Interface "$interface_val" mtu_request=9216
                _ovs-vsctl set Interface "$interface_val" external_ids:xplane=true
                # Match the downlink patch's physical NIC group ID, e.g. A56.
                _ovs-vsctl set Interface "$interface_val" external_ids:xplane-group-id="$iface_prefix"
                _ovs-vsctl set Interface "$interface_val" external_ids:xplane-uplink=true
                _ovs-vsctl set Interface "$interface_val" external_ids:xplane-plane-id="$hw_plane"
            done
        done
        mst start
    "#}
}

fn configure_xplane_network_script_raw() -> String {
    // Go whitespace trimming here would remove the YAML block scalar's required newline.
    [
        "{{ if and .dpu_device_underlay_rows (index .dpu_device_underlay_rows 0).cerebro_ifname }}",
        xplane_network_preamble_script_raw(),
        &xplane_cerebro_network_script_raw(),
        "{{ else }}",
        &xplane_legacy_network_script_raw(),
        "{{ end }}\n",
    ]
    .concat()
}

fn xplane_legacy_network_script_raw() -> String {
    indoc::indoc! {r#"
        #!/bin/bash
        NETPLAN_FILE="/etc/netplan/99-cx9-rails.yaml"
        set -e
        # Preserve the active netplan until every bridge lookup and write succeeds.
        NETPLAN_TMP="$(mktemp "${NETPLAN_FILE}.tmp.XXXXXX")"
        # Keep a failed candidate for debugging; successful writes rename it into place.
        trap 'if [ -f "$NETPLAN_TMP" ]; then printf "xplane-bridge.sh: retained failed netplan candidate at %s\n" "$NETPLAN_TMP" >&2; fi' EXIT

        # interface prefix | PCI address | bridge. The MAC-to-PCI association is read
        # from the SmartNIC PF config for each interface prefix's p0 port.
        PCI_BRIDGE_ROWS=(
            "A53|0005:03:00.0|brcx-A53"
            "A56|0005:06:00.0|brcx-A56"
            "A43|0004:03:00.0|brcx-A43"
            "A46|0004:06:00.0|brcx-A46"
            "A3|0000:03:00.0|brcx-A3"
            "A6|0000:06:00.0|brcx-A6"
            "A13|0001:03:00.0|brcx-A13"
            "A16|0001:06:00.0|brcx-A16"
        )

        # MAC address | address | gateway | /16 route | /13 route
        DPU_DEVICE_UNDERLAY_ROWS=(
        {{- range .dpu_device_underlay_rows }}
        {{- if .ip }}
            "{{ .mac_address }}|{{ .ip }}|{{ .gateway }}|{{ .rail_route }}|{{ .sw_plane_route }}"
        {{- end }}
        {{- end }}
        )

        bridge_for_mac() {
            local target_mac="$1" row iface_prefix pci bridge iface_val config_path
            for row in "${PCI_BRIDGE_ROWS[@]}"; do
                IFS='|' read -r iface_prefix pci bridge <<< "$row"
                bridge="brcx-${iface_prefix}"
                iface_val="${iface_prefix}p0"
                config_path="/sys/bus/pci/devices/${pci}/net/${iface_val}/smart_nic/pf/config"
                if [ -r "$config_path" ] && grep -qiF "$target_mac" "$config_path"; then
                    printf '%s\n' "$bridge"
                    return 0
                fi
            done
            return 1
        }

        {
            echo "network:"
            echo "  version: 2"
            echo "  ethernets:"

            for row in "${DPU_DEVICE_UNDERLAY_ROWS[@]}"; do
                IFS='|' read -r mac address gateway route1 route2 <<< "$row"
                bridge="$(bridge_for_mac "$mac")" || {
                    echo "xplane-bridge.sh: no bridge found for underlay MAC ${mac}" >&2
                    exit 1
                }
                echo "    ${bridge}:"
                echo "      addresses:"
                echo "        - ${address}"
                echo "      routes:"
                echo "        - to: ${route1}"
                echo "          via: ${gateway}"
                echo "        - to: ${route2}"
                echo "          via: ${gateway}"
            done
        } > "$NETPLAN_TMP"
        # Both files are in the same directory, so replacement is atomic.
        mv -- "$NETPLAN_TMP" "$NETPLAN_FILE"

        netplan apply

        # Block until oob_net0 has an IP again, since netplan
        # apply can transiently drop it. Avoids a race with
        # dpuagent joining the cluster afterwards.
        OOB_IFACE="oob_net0"
        OOB_WAIT_TIMEOUT=120
        SECONDS=0

        while :; do
            if ip -4 -o addr show dev "$OOB_IFACE" scope global 2>/dev/null | grep -q "inet "; then
                echo "xplane-bridge.sh: ${OOB_IFACE} has an IP after ${SECONDS}s"
                break
            fi

            if [ "$SECONDS" -ge "$OOB_WAIT_TIMEOUT" ]; then
                echo "xplane-bridge.sh: timed out after ${OOB_WAIT_TIMEOUT}s waiting for ${OOB_IFACE} to have an IP" >&2
                exit 1
            fi

            echo "xplane-bridge.sh: waiting for ${OOB_IFACE} to get an IP (${SECONDS}s elapsed)"
            sleep 2
        done

    "#}
    .to_string()
}

fn xplane_network_preamble_script_raw() -> &'static str {
    indoc::indoc! {r#"
        #!/bin/bash
        set -euo pipefail

        MAPPING_FILE="${MAPPING_FILE:-/etc/mellanox/interface-fixed-mapping.csv}"
        SYSFS_PCI_DEVICES_DIR="${SYSFS_PCI_DEVICES_DIR:-/sys/bus/pci/devices}"
        LLDP_FILE="${LLDP_FILE:-/etc/lldpd.d/lldp-interfaces.conf}"
        NETPLAN_FILE="${NETPLAN_FILE:-/etc/netplan/99-cx9-rails.yaml}"
        OVS_CONFIG_SCRIPT="${OVS_CONFIG_SCRIPT:-/etc/mellanox/ovs-script.sh}"

        # Escape the template value for Bash ANSI-C quoting without shell expansion.
        SERVER_NAME=$'{{ with index . "server_name" }}{{ js . }}{{ end }}'
        if [[ "$SERVER_NAME" == *$'\n'* || "$SERVER_NAME" == *$'\r'* ]]; then
            echo "configure-xplane-network.sh: server name must be a single line" >&2
            exit 1
        fi

        declare -A NETDEV_BY_INTERFACES=()
        declare -a INTERFACES=()
        declare -A PCI_BY_INTERFACE=()
        declare -A PHYS_PORT_BY_INTERFACE=()
        declare -A CEREBRO_BY_INTERFACE=()
        declare -A INTERFACE_BY_CEREBRO=()
        declare -A BRIDGE_BY_CEREBRO=()
        # Names needing a new or corrected altname assignment. Names already
        # owned by the expected netdev are skipped to avoid needless delete/add.
        # Queue changes during validation and apply them only after all checks.
        declare -a ALTNAMES_TO_ADD=()

        # Retain any incomplete configuration files for debugging.
        trap 'for candidate in "${LLDP_TMP_FILE:-}" "${netplan_tmp:-}"; do if [ -f "$candidate" ]; then printf "configure-xplane-network.sh: retained failed candidate at %s\n" "$candidate" >&2; fi; done' EXIT

        # Build DPU_DEVICE_UNDERLAY_ROWS array from values set in the DPU device
        # Cerebro name | MAC | address | gateway | rail route | software-plane route | logical name
        DPU_DEVICE_UNDERLAY_ROWS=(
        {{- range .dpu_device_underlay_rows }}
            "{{ .cerebro_ifname }}|{{ .mac_address }}|{{ .ip }}|{{ .gateway }}|{{ .rail_route }}|{{ .sw_plane_route }}|{{ .logical_ifname }}"
        {{- end }}
        )

    "#}
}

fn xplane_build_interface_named_mappings_script_raw() -> &'static str {
    indoc::indoc! {r#"
        if [ ! -r "$MAPPING_FILE" ]; then
            echo "configure-xplane-network.sh: cannot read mapping file: $MAPPING_FILE" >&2
            exit 1
        fi

        # Build
        #   INTERFACES+=("$logical_ifname")
        #   PCI_BY_INTERFACE["$logical_ifname"]="$pci_address"
        #   PHYS_PORT_BY_INTERFACE["$logical_ifname"]="$phys_port_name"
        #   CEREBRO_BY_INTERFACE["$logical_ifname"]="$cerebro_ifname"
        #   INTERFACE_BY_CEREBRO["$cerebro_ifname"]="$logical_ifname"
        # where logical_ifname is eth_r0_p0, etc using
        # (1) dpu device underlay rows has the cerebro_ifname to logical_ifname mapping
        # (2) fixed interface map has the cerebro_ifname to pci map.
        declare -A seen_ports=()
        for row in "${DPU_DEVICE_UNDERLAY_ROWS[@]}"; do
            IFS='|' read -r cerebro_ifname mac address gateway route1 route2 logical_ifname <<< "$row"
            if [[ ! "$logical_ifname" =~ ^[a-zA-Z0-9_.-]+$ ]] || [[ ! "$cerebro_ifname" =~ ^[a-zA-Z0-9_.:-]+$ ]] || [ "${#logical_ifname}" -gt 127 ]; then
                echo "configure-xplane-network.sh: invalid logical or Cerebro interface name: ${logical_ifname}, ${cerebro_ifname}" >&2
                exit 1
            fi
            if [[ -n "${PCI_BY_INTERFACE[$logical_ifname]:-}" ]] || [[ -n "${INTERFACE_BY_CEREBRO[$cerebro_ifname]:-}" ]]; then
                echo "configure-xplane-network.sh: duplicate declared logical or Cerebro interface: ${logical_ifname}, ${cerebro_ifname}" >&2
                exit 1
            fi
            line_number=0
            matched_port=""
            while IFS=, read -r pci_address phys_port_name cerebro_name extra; do
                line_number=$((line_number + 1))
                cerebro_name="${cerebro_name%$'\r'}"
                if [ "$line_number" -eq 1 ]; then
                    if [ "$pci_address,$phys_port_name,$cerebro_name" != "pci_address,phys_port_name,cerebro_interface_name" ] || [ -n "${extra:-}" ]; then
                        echo "configure-xplane-network.sh: invalid CSV header" >&2
                        exit 1
                    fi
                    continue
                fi
                [[ -z "$pci_address$phys_port_name$cerebro_name${extra:-}" ]] && continue
                [ "$cerebro_name" = "$cerebro_ifname" ] || continue
                if [ -n "${extra:-}" ] || [[ ! "$pci_address" =~ ^[0-9a-fA-F]{4}:[0-9a-fA-F]{2}:[0-9a-fA-F]{2}\.[0-7]$ ]] || [[ ! "$phys_port_name" =~ ^[a-zA-Z0-9_.-]+$ ]]; then
                    echo "configure-xplane-network.sh: invalid fixed mapping for ${cerebro_ifname} on line ${line_number}" >&2
                    exit 1
                fi
                if [ -n "$matched_port" ]; then
                    echo "configure-xplane-network.sh: duplicate fixed mapping for ${cerebro_ifname}" >&2
                    exit 1
                fi
                matched_port="$pci_address|$phys_port_name"
            done < "$MAPPING_FILE"
            if [ -z "$matched_port" ]; then
                echo "configure-xplane-network.sh: no fixed mapping for Cerebro name ${cerebro_ifname}" >&2
                exit 1
            fi
            IFS='|' read -r pci_address phys_port_name <<< "$matched_port"
            port_key="$pci_address|$phys_port_name"
            if [[ -n "${seen_ports[$port_key]:-}" ]]; then
                echo "configure-xplane-network.sh: duplicate PCI/physical port: $port_key" >&2
                exit 1
            fi

            INTERFACES+=("$logical_ifname")
            PCI_BY_INTERFACE["$logical_ifname"]="$pci_address"
            PHYS_PORT_BY_INTERFACE["$logical_ifname"]="$phys_port_name"
            CEREBRO_BY_INTERFACE["$logical_ifname"]="$cerebro_ifname"
            INTERFACE_BY_CEREBRO["$cerebro_ifname"]="$logical_ifname"
            seen_ports["$port_key"]=1
        done
    "#}
}

fn xplane_find_netdevs_and_build_altnames_script_raw() -> &'static str {
    indoc::indoc! {r#"
        find_netdev_name() {
            # Finds netdev names for input pci address in
            # /sys/bus/pci/devices/$pci_address/net and returns
            # matched_netdev which matches input port p<y>
            local pci_address="$1"
            local expected_phys_port_name="$2"
            local net_dir="$SYSFS_PCI_DEVICES_DIR/$pci_address/net"
            local netdev_path netdev_name actual_phys_port_name
            local matched_netdev=""

            if [ ! -d "$net_dir" ]; then
                echo "configure-xplane-network.sh: no net directory for PCI address $pci_address" >&2
                return 1
            fi

            shopt -s nullglob
            local netdev_paths=("$net_dir"/*)
            shopt -u nullglob

            for netdev_path in "${netdev_paths[@]}"; do
                if [ ! -r "$netdev_path/phys_port_name" ]; then
                    continue
                fi
                netdev_name="${netdev_path##*/}"
                IFS= read -r actual_phys_port_name < "$netdev_path/phys_port_name"
                if [ "$actual_phys_port_name" != "$expected_phys_port_name" ]; then
                    continue
                fi
                if [ -n "$matched_netdev" ]; then
                    echo "configure-xplane-network.sh: multiple netdevs match PCI $pci_address and phys_port_name $expected_phys_port_name" >&2
                    return 1
                fi
                matched_netdev="$netdev_name"
            done

            if [ -z "$matched_netdev" ]; then
                echo "configure-xplane-network.sh: no netdev matches PCI $pci_address and phys_port_name $expected_phys_port_name" >&2
                return 1
            fi

            printf '%s\n' "$matched_netdev"
        }

        declare -A seen_netdevs=()

        for interface_name in "${INTERFACES[@]}"; do
            netdev_name="$(find_netdev_name "${PCI_BY_INTERFACE[$interface_name]}" "${PHYS_PORT_BY_INTERFACE[$interface_name]}")" || exit 1

            if [[ -n "${seen_netdevs[$netdev_name]:-}" ]]; then
                echo "configure-xplane-network.sh: netdev $netdev_name matched more than one mapped interface" >&2
                exit 1
            fi

            NETDEV_BY_INTERFACES["$interface_name"]="$netdev_name"
            seen_netdevs["$netdev_name"]=1

            # Check the live netdev and altname ownership before making changes.
            # ALTNAMES_TO_ADD is a subset of INTERFACES where we remove
            # interfaces that already have the altname configured previously
            # and the netdev "owner" matches so don't need to redo it.
            ip -o link show dev "$netdev_name" >/dev/null || exit 1
            if link_info="$(ip -o link show dev "$interface_name" 2>/dev/null)"; then
                read -r link_index owner link_details <<< "$link_info"
                owner="${owner%:}"
                owner="${owner%%@*}"
                if [ "$owner" = "$netdev_name" ]; then
                    continue
                fi
            fi
            ALTNAMES_TO_ADD+=("$interface_name")
        done
    "#}
}

fn xplane_check_net_config_and_build_bridge_by_cerebro_script_raw() -> &'static str {
    indoc::indoc! {r#"
        bridge_for_cerebro() {
            local cerebro_name="$1" rail sw_plane port interface_name netdev_name
            for rail in 0 1 2 3; do
                for sw_plane in 0 1; do
                    for port in 0 1 2 3; do
                        interface_name="eth_r${rail}_p$((4 * sw_plane + port))"
                        if [ "${CEREBRO_BY_INTERFACE[$interface_name]:-}" = "$cerebro_name" ]; then
                            netdev_name="${NETDEV_BY_INTERFACES[$interface_name]:-}"
                            [[ "$netdev_name" =~ ^(.+)p[0-9]+$ ]] || return 1
                            printf 'brcx-%s\n' "${BASH_REMATCH[1]}"
                            return 0
                        fi
                    done
                done
            done
            return 1
        }

        valid_ipv4() {
            local value="$1" octet
            local -a octets=()
            [[ "$value" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]] || return 1
            IFS=. read -r -a octets <<< "$value"
            for octet in "${octets[@]}"; do
                ((10#$octet <= 255)) || return 1
            done
        }

        valid_ipv4_cidr() {
            local value="$1" prefix="${1##*/}"
            [[ "$value" == */* && "$prefix" =~ ^[0-9]{1,2}$ ]] || return 1
            ((10#$prefix <= 32)) || return 1
            valid_ipv4 "${value%/*}"
        }

        # OVS expects all four ports of each rail/software-plane group.
        for rail in 0 1 2 3; do
            for plane in 0 1 2 3 4 5 6 7; do
                interface_name="eth_r${rail}_p${plane}"
                if [ -z "${NETDEV_BY_INTERFACES[$interface_name]:-}" ]; then
                    echo "configure-xplane-network.sh: no discovered netdev for interface $interface_name" >&2
                    exit 1
                fi
            done
        done
        if [ ! -r "$OVS_CONFIG_SCRIPT" ]; then
            echo "configure-xplane-network.sh: cannot read OVS script: $OVS_CONFIG_SCRIPT" >&2
            exit 1
        fi
        bash -n "$OVS_CONFIG_SCRIPT" || exit 1
        for destination in "$LLDP_FILE" "$NETPLAN_FILE"; do
            if [ -d "$destination" ]; then
                echo "configure-xplane-network.sh: configuration destination is a directory: $destination" >&2
                exit 1
            fi
        done

        declare -A seen_bridges=()
        declare -A bridge_by_address=()
        for row in "${DPU_DEVICE_UNDERLAY_ROWS[@]}"; do
            IFS='|' read -r cerebro_ifname mac address gateway route1 route2 logical_ifname <<< "$row"
            [ -n "$address" ] || continue
            bridge_name="$(bridge_for_cerebro "$cerebro_ifname")" || {
                echo "configure-xplane-network.sh: no bridge found for Cerebro name $cerebro_ifname" >&2
                exit 1
            }
            bridge_configuration="$address|$gateway|$route1|$route2"
            if [[ -n "${seen_bridges[$bridge_name]:-}" && "${seen_bridges[$bridge_name]}" != "$bridge_configuration" ]]; then
                echo "configure-xplane-network.sh: conflicting underlay entries for bridge $bridge_name" >&2
                exit 1
            fi
            if [[ -n "${bridge_by_address[$address]:-}" && "${bridge_by_address[$address]}" != "$bridge_name" ]]; then
                echo "configure-xplane-network.sh: address $address is shared by different bridges" >&2
                exit 1
            fi
            if ! valid_ipv4_cidr "$address" || ! valid_ipv4 "$gateway" ||
                ! valid_ipv4_cidr "$route1" || ! valid_ipv4_cidr "$route2"; then
                echo "configure-xplane-network.sh: invalid address, gateway, or route for $cerebro_ifname" >&2
                exit 1
            fi
            BRIDGE_BY_CEREBRO["$cerebro_ifname"]="$bridge_name"
            seen_bridges["$bridge_name"]="$bridge_configuration"
            bridge_by_address["$address"]="$bridge_name"
        done
    "#}
}

fn xplane_build_lldp_tmp_configuration_script_raw() -> &'static str {
    indoc::indoc! {r#"
        lldp_dir="$(dirname -- "$LLDP_FILE")"
        mkdir -p "$lldp_dir" || exit 1

        LLDP_TMP_FILE="$(mktemp "$lldp_dir/.lldp-interfaces.conf.XXXXXX")" || exit 1

        printf '%s\n' \
            'configure system interface pattern *' \
            'configure lldp portidsubtype macaddress' > "$LLDP_TMP_FILE" || exit 1
        for interface_name in "${INTERFACES[@]}"; do
            netdev_name="${NETDEV_BY_INTERFACES[$interface_name]}"
            port_description="${CEREBRO_BY_INTERFACE[$interface_name]}"
            if [ -n "${SERVER_NAME:-}" ]; then
                port_description="$SERVER_NAME $port_description"
            fi
            # Quote the description for lldpcli's configuration parser.
            port_description="${port_description//\\/\\\\}"
            port_description="${port_description//\"/\\\"}"
            printf 'configure ports %s lldp portdescription "%s"\n' \
                "$netdev_name" "$port_description" \
                >> "$LLDP_TMP_FILE" || exit 1
        done

        chmod 0644 "$LLDP_TMP_FILE" || exit 1
    "#}
}

fn xplane_build_netplan_tmp_configuration_script_raw() -> &'static str {
    indoc::indoc! {r#"
        mkdir -p "$(dirname -- "$NETPLAN_FILE")" || exit 1
        # Preserve the active configuration until every lookup and write succeeds.
        netplan_tmp="$(mktemp "${NETPLAN_FILE}.tmp.XXXXXX")" || exit 1
        # Each four-lane group repeats its bridge configuration; emit the bridge once.
        declare -A written_bridges=()
        {
            printf '%s\n' 'network:' '  version: 2' '  ethernets:' || exit 1

            for row in "${DPU_DEVICE_UNDERLAY_ROWS[@]}"; do
                IFS='|' read -r cerebro_ifname mac address gateway route1 route2 logical_ifname <<< "$row"
                [ -n "$address" ] || continue
                bridge_name="${BRIDGE_BY_CEREBRO[$cerebro_ifname]:-}"
                if [ -z "$bridge_name" ]; then
                    echo "configure-xplane-network.sh: no validated bridge for $cerebro_ifname" >&2
                    exit 1
                fi
                [[ -z "${written_bridges[$bridge_name]:-}" ]] || continue
                written_bridges["$bridge_name"]=1
                printf '%s\n' \
                    "    ${bridge_name}:" \
                    "      mtu: 9216" \
                    "      addresses:" \
                    "        - ${address}" \
                    "      routes:" \
                    "        - to: ${route1}" \
                    "          via: ${gateway}" \
                    "        - to: ${route2}" \
                    "          via: ${gateway}" || exit 1
            done
        } > "$netplan_tmp" || exit 1
    "#}
}

fn xplane_execute_verified_network_configuration_script_raw() -> &'static str {
    indoc::indoc! {r#"
        # All altnames must be assigned before configuring OVS or installing LLDP/netplan files.
        for interface_name in "${ALTNAMES_TO_ADD[@]}"; do
            netdev_name="${NETDEV_BY_INTERFACES[$interface_name]}"
            # Recheck ownership because it may have changed since validation.
            if link_info="$(ip -o link show dev "$interface_name" 2>/dev/null)"; then
                read -r link_index owner link_details <<< "$link_info"
                owner="${owner%:}"
                owner="${owner%%@*}"
                [ "$owner" != "$netdev_name" ] || continue
                if ! ip link property del dev "$owner" altname "$interface_name"; then
                    echo "configure-xplane-network.sh: failed to remove altname $interface_name from $owner" >&2
                    exit 1
                fi
            fi
            if ! ip link property add dev "$netdev_name" altname "$interface_name"; then
                echo "configure-xplane-network.sh: failed to set altname $interface_name on $netdev_name" >&2
                exit 1
            fi
        done

        # Source OVS setup so it can use the discovered NETDEV_BY_INTERFACES array.
        source "$OVS_CONFIG_SCRIPT"

        # Both candidates are complete before replacing either active file.
        mv -fT -- "$LLDP_TMP_FILE" "$LLDP_FILE" || exit 1
        mv -fT -- "$netplan_tmp" "$NETPLAN_FILE" || exit 1

        # Both candidates are installed; stop reporting retained temporary files on exit.
        trap - EXIT

        systemctl daemon-reload
        systemctl restart lldpd.service
        netplan apply
    "#}
}

fn xplane_write_netdev_logical_ifname_mapping_script_raw() -> &'static str {
    indoc::indoc! {r#"
        # Publish the verified logical-to-netdev mapping for the containerized agent.
        # Mount the directory so atomic replacements remain visible to running pods.
        NETDEV_MAPPING_FILE="${NETDEV_MAPPING_FILE:-/etc/nico/xplane/logical-to-netdev-mapping}"
        install -d -m 0755 -- "$(dirname -- "$NETDEV_MAPPING_FILE")" || exit 1
        mapping_tmp="$(mktemp "${NETDEV_MAPPING_FILE}.tmp.XXXXXX")" || exit 1
        if ! (
            for interface_name in "${INTERFACES[@]}"; do
                printf '%s\t%s\n' "$interface_name" "${NETDEV_BY_INTERFACES[$interface_name]}" || exit 1
            done
        ) > "$mapping_tmp" || ! chmod 0644 "$mapping_tmp" || ! mv -fT -- "$mapping_tmp" "$NETDEV_MAPPING_FILE"; then
            rm -f -- "$mapping_tmp"
            exit 1
        fi
    "#}
}

fn xplane_oob_network_wait_script_raw() -> &'static str {
    indoc::indoc! {r#"
        # Block until oob_net0 has an IP again, since netplan apply can
        # transiently drop it. This avoids a race with dpuagent joining
        # the cluster afterwards.
        oob_iface="oob_net0"
        oob_wait_timeout=120
        SECONDS=0

        while :; do
            if ip -4 -o addr show dev "$oob_iface" scope global 2>/dev/null | grep -q "inet "; then
                echo "configure-xplane-network.sh: ${oob_iface} has an IP after ${SECONDS}s"
                break
            fi
            if [ "$SECONDS" -ge "$oob_wait_timeout" ]; then
                echo "configure-xplane-network.sh: timed out after ${oob_wait_timeout}s waiting for ${oob_iface} to have an IP" >&2
                exit 1
            fi
            echo "configure-xplane-network.sh: waiting for ${oob_iface} to get an IP (${SECONDS}s elapsed)"
            sleep 2
        done
    "#}
}

// Generate netdev altnames, lldp config and netplan. Build temporary
// files/buffers while checking the config. Once all checks are
// verified, install the new configuration from the temporary files/buffers.
fn xplane_cerebro_network_script_raw() -> String {
    [
        // Validation: check mappings, netdevs, altname ownership, bridges, and addresses.
        // Build the named mappings and queue altname changes without changing the network.
        xplane_build_interface_named_mappings_script_raw(),
        xplane_find_netdevs_and_build_altnames_script_raw(),
        xplane_check_net_config_and_build_bridge_by_cerebro_script_raw(),
        // Preparation: generate both temporary files while keeping active files unchanged.
        xplane_build_lldp_tmp_configuration_script_raw(),
        xplane_build_netplan_tmp_configuration_script_raw(),
        // Execution: only after validation and generation succeed, apply the queued
        // altnames, configure OVS, install both files, and reload/apply the services.
        xplane_execute_verified_network_configuration_script_raw(),
        xplane_write_netdev_logical_ifname_mapping_script_raw(),
        // Wait for the OOB address to return after netplan apply.
        xplane_oob_network_wait_script_raw(),
    ]
    .concat()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command, Output};

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};

    use super::*;
    use crate::types::{
        DpfInterceptBridge, DpfInterceptBridging, DpfInterfaceIdentity, DpfProxyDetails,
    };

    fn proxy(https_proxy: &str, no_proxy: &[&str]) -> Option<DpfProxyDetails> {
        Some(DpfProxyDetails {
            https_proxy: https_proxy.to_string(),
            no_proxy: no_proxy.iter().map(|s| s.to_string()).collect(),
        })
    }

    fn expected_astra_pf_total_sf_parameter() -> String {
        let interfaces = crate::sdk::build_astra_dpu_interfaces_vec();
        let pf_total_sf = crate::sdk::calculate_astra_pf_total_sf(interfaces.as_slice())
            .expect("canonical Astra inventory must have valid SF capacity");
        format!("PF_TOTAL_SF={pf_total_sf}")
    }

    fn astra_flavor_spec(
        proxy: &Option<DpfProxyDetails>,
        enable_delay_host_init: bool,
    ) -> DpuFlavorSpec {
        let interfaces = crate::sdk::build_astra_dpu_interfaces_vec();
        let pf_total_sf = crate::sdk::calculate_astra_pf_total_sf(interfaces.as_slice()).unwrap();
        let template =
            flavor_bf4_astra("astra-ns", proxy, pf_total_sf, &[], enable_delay_host_init).unwrap();
        flavor_spec_from_template(&template)
    }

    fn flavor_spec_from_template(template: &DPUFlavorTemplate) -> DpuFlavorSpec {
        let body: serde_yaml::Value = serde_yaml::from_str(&template.spec.template).unwrap();
        serde_yaml::from_value(body["spec"].clone()).unwrap()
    }

    /// The `raw` body of the trailing (proxy) config file built by `default_flavor`.
    fn proxy_file_raw(https_proxy: &str, no_proxy: &[&str]) -> String {
        let flavor = default_flavor("ns", &proxy(https_proxy, no_proxy)).unwrap();
        let files = flavor.spec.config_files.unwrap();
        files.last().unwrap().raw.clone().unwrap()
    }

    /// `unique_name` of the default flavor for the given proxy, with the standard prefix.
    fn name_for(proxy: &Option<DpfProxyDetails>) -> String {
        default_flavor("ns", proxy)
            .unwrap()
            .unique_name("dpu-flavor")
            .unwrap()
    }

    /// Provides deterministic PF/VF entries whose fixed names make flavor topology assertions
    /// exact.
    fn intercept_bridging() -> DpfInterceptBridging {
        DpfInterceptBridging::new(
            vec![
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: Some(4),
                    },
                    "br-vf4",
                    "p-vf4",
                ),
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: None,
                    },
                    "br-pf3",
                    "p-pf3",
                ),
            ],
            16,
        )
        .expect("flavor topology fixture must be valid")
    }

    /// Executes generic-BF4 preflight against a synthetic `sys/class/net` tree.
    fn run_bf4_preflight(
        topology: &DpfInterceptBridging,
        phys_port_names: &[(&str, &str)],
        prefix: &str,
        suffix: &str,
    ) -> Output {
        let fixture =
            std::env::temp_dir().join(format!("carbide-dpf-bf4-sysfs-{}", uuid::Uuid::new_v4()));

        // Materialize only the semantic sysfs surface consumed by provisioning.
        for (netdev, phys_port_name) in phys_port_names {
            let netdev_dir = fixture.join(netdev);
            fs::create_dir_all(&netdev_dir).expect("synthetic netdev directory must be created");
            fs::write(netdev_dir.join("phys_port_name"), phys_port_name)
                .expect("synthetic phys_port_name must be written");
        }
        let script = format!("{prefix}\n{}\n{suffix}", render_bf4_pf_preflight(topology));
        let output = Command::new("bash")
            .arg("-c")
            .arg(script)
            .env("NICO_SYS_CLASS_NET", &fixture)
            .output()
            .expect("bash must execute synthetic BF4 preflight");
        fs::remove_dir_all(&fixture).expect("synthetic BF4 sysfs fixture must be removed");
        output
    }

    /// Executes the complete topology-backed Astra raw script against controlled external
    /// boundaries so ordering and propagation are tested without modifying host OVS or netplan.
    fn run_astra_topology_bootstrap(failure: &str) -> (Output, Vec<String>) {
        let fixture = std::env::temp_dir().join(format!(
            "carbide-dpf-astra-bootstrap-{}",
            uuid::Uuid::new_v4()
        ));
        // Map the semantic PF to a different runtime name so command checks prove resolution.
        let netdev = fixture.join("sys-class-net/en8f2");
        fs::create_dir_all(&netdev).expect("synthetic Astra netdev directory must be created");
        fs::write(netdev.join("phys_port_name"), "c2pf3")
            .expect("synthetic Astra phys_port_name must be written");

        // Use an executable child so failure exercises the production hook's guarded invocation.
        let pre_ovs = fixture.join("pre-ovs.sh");
        fs::write(
            &pre_ovs,
            r#"#!/bin/bash
run_stage pre
if [[ "$TEST_FAILURE" == pre ]]; then
    run_stage injected:pre
    exit 1
fi
"#,
        )
        .expect("synthetic pre-OVS hook must be written");
        let mut permissions = fs::metadata(&pre_ovs)
            .expect("synthetic pre-OVS hook metadata must be readable")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&pre_ovs, permissions)
            .expect("synthetic pre-OVS hook must be executable");

        // These child scripts expose completion markers around the external script boundaries.
        let fixed_ovs = fixture.join("fixed-ovs.sh");
        fs::write(
            &fixed_ovs,
            r#"#!/bin/bash
run_stage fixed
if [[ "$TEST_FAILURE" == fixed ]]; then
    run_stage injected:fixed
    false
fi
run_stage fixed-complete
"#,
        )
        .expect("synthetic fixed OVS script must be written");
        let xplane = fixture.join("xplane.sh");
        fs::write(
            &xplane,
            r#"#!/bin/bash
set -e
run_stage xplane-entered
if [[ "$TEST_FAILURE" == xplane ]]; then
    run_stage injected:xplane
    false
fi
run_stage xplane-complete
"#,
        )
        .expect("synthetic xplane script must be written");

        // Fake ovs-vsctl records all operations and injects one requested boundary failure.
        let harness = r#"
run_stage() {
    printf '%s\n' "$1" >> "$TEST_LOG"
}
ovs-vsctl() {
    printf 'ovs:%s\n' "$*" >> "$TEST_LOG"
    if [[ "$TEST_FAILURE" == telemetry
        && "$*" == *"doca-telemetry-source-id=xplane"* ]]; then
        run_stage injected:telemetry
        return 1
    fi
    if [[ "$TEST_FAILURE" == required-topology
        && "$*" == *"--may-exist add-br br-pf3"* ]]; then
        run_stage injected:required-topology
        return 1
    fi
    if [[ "$TEST_FAILURE" == tolerated-representor
        && "$*" == *"--may-exist add-port br-pf3"* ]]; then
        run_stage injected:tolerated-representor
        return 1
    fi
}
export -f run_stage ovs-vsctl
"#;
        let log = fixture.join("stages.log");
        // Replace external hooks and fixed scripts while leaving generated topology commands
        // intact for exact inspection.
        // The generated script wraps its fixed OVS and xplane invocations in a Go-template
        // `{{ if ...cerebro_ifname }}` branch that DPF renders per device. Render the legacy
        // (non-Cerebro) branch here so the staged bash below is executable while still exercising
        // the fixed OVS, telemetry, and xplane ordering around the topology commands.
        let template = get_bf4_astra_ovs_defaults(Some(&intercept_bridging()), true);
        let (prefix, after_if) = template
            .split_once("{{ if ")
            .expect("astra script must contain the cerebro branch guard");
        let (_, after_cond) = after_if
            .split_once(" }}\n")
            .expect("astra cerebro branch guard must close");
        let (_, after_else) = after_cond
            .split_once("{{ else }}\n")
            .expect("astra script must contain the legacy branch");
        let (legacy, suffix) = after_else
            .split_once("{{ end }}\n")
            .expect("astra legacy branch must close");
        let script = format!("{prefix}{legacy}{suffix}")
            .replace("/opt/dpf/extra-script-pre-ovs.sh", "\"$TEST_PRE_OVS\"")
            .replace(
                "bash -e /etc/mellanox/ovs-script.sh",
                "bash -e \"$TEST_FIXED_OVS\"",
            )
            .replace(
                "/usr/local/sbin/configure-xplane-network.sh",
                "bash \"$TEST_XPLANE\"",
            )
            .replace(
                "if [ -x /opt/dpf/extra-script-post-ovs.sh ]; then /opt/dpf/extra-script-post-ovs.sh; fi",
                concat!(
                    "run_stage post\n",
                    "if [[ \"$TEST_FAILURE\" == post ]]; then\n",
                    "    run_stage injected:post\n",
                    "    false\n",
                    "fi",
                ),
            );

        // Do not supply errexit from the harness; the generated script must own that contract.
        let output = Command::new("bash")
            .args(["-c", &format!("{harness}\n{script}")])
            .env("NICO_SYS_CLASS_NET", fixture.join("sys-class-net"))
            .env("TEST_FAILURE", failure)
            .env("TEST_PRE_OVS", &pre_ovs)
            .env("TEST_FIXED_OVS", &fixed_ovs)
            .env("TEST_XPLANE", &xplane)
            .env("TEST_LOG", &log)
            .output()
            .expect("bash must execute the topology-backed Astra bootstrap");
        let stages = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        fs::remove_dir_all(&fixture).expect("Astra bootstrap fixture must be removed");
        (output, stages)
    }

    /// Verifies the topology-backed Astra bootstrap orders prerequisites before dependent setup and
    /// preserves strict versus best-effort failures, so required failures stop later stages without
    /// making a tolerated representor attachment block completion.
    #[test]
    fn astra_topology_bootstrap_order_and_failure_boundaries() {
        // The successful execution proves hooks surround fixed OVS, telemetry, xplane, and topology.
        let (output, stages) = run_astra_topology_bootstrap("");
        assert!(
            output.status.success(),
            "successful bootstrap failed: {output:?}"
        );
        let exact_position = |stage: &str| {
            stages
                .iter()
                .position(|observed| observed == stage)
                .unwrap_or_else(|| panic!("missing stage {stage:?} in {stages:?}"))
        };
        let containing_position = |fragment: &str| {
            stages
                .iter()
                .position(|observed| observed.contains(fragment))
                .unwrap_or_else(|| panic!("missing fragment {fragment:?} in {stages:?}"))
        };
        let order = [
            exact_position("pre"),
            exact_position("fixed"),
            exact_position("fixed-complete"),
            containing_position("doca-telemetry-source-id=xplane"),
            exact_position("xplane-entered"),
            exact_position("xplane-complete"),
            containing_position("--may-exist add-br br-pf3"),
            exact_position("post"),
        ];
        assert!(
            order
                .windows(2)
                .all(|positions| positions[0] < positions[1]),
            "unexpected Astra bootstrap order: {stages:?}"
        );

        // The resolved PF and its derived VF must reach the exact representor attachments.
        for expected in [
            // The selected PF must attach its resolved runtime netdev to its configured bridge.
            concat!(
                "ovs:--timeout 30 --if-exists del-port en8f2 -- --may-exist add-port ",
                "br-pf3 en8f2 -- set interface en8f2 type=dpdk mtu_request=9216 ",
                "external_ids={}",
            ),
            // The selected VF derives its runtime netdev from that PF before attachment.
            concat!(
                "ovs:--timeout 30 --if-exists del-port en8f2vf4 -- --may-exist add-port ",
                "br-vf4 en8f2vf4 -- set interface en8f2vf4 type=dpdk mtu_request=9216 ",
                "external_ids={}",
            ),
        ] {
            assert!(
                stages.iter().any(|stage| stage.as_str() == expected),
                "missing exact representor command {expected:?} in {stages:?}"
            );
        }

        struct FailureCase {
            name: &'static str,
            failure: &'static str,
            expected_success: bool,
            required: &'static [&'static str],
            forbidden: &'static [&'static str],
        }
        let cases = [
            // A failing operator pre-hook must stop before every built-in bootstrap stage.
            FailureCase {
                name: "pre hook failure",
                failure: "pre",
                expected_success: false,
                required: &["injected:pre"],
                forbidden: &[
                    "fixed",
                    "doca-telemetry-source-id=xplane",
                    "xplane-entered",
                    "--may-exist add-br br-pf3",
                    "post",
                ],
            },
            // An internal fixed-child failure must stop before telemetry and xplane setup.
            FailureCase {
                name: "fixed child failure",
                failure: "fixed",
                expected_success: false,
                required: &["injected:fixed"],
                forbidden: &[
                    "fixed-complete",
                    "doca-telemetry-source-id=xplane",
                    "xplane-entered",
                    "post",
                ],
            },
            // Telemetry failure must stop before entering xplane or creating topology bridges.
            FailureCase {
                name: "telemetry failure",
                failure: "telemetry",
                expected_success: false,
                required: &["fixed-complete", "injected:telemetry"],
                forbidden: &["xplane-entered", "--may-exist add-br br-pf3", "post"],
            },
            // Xplane failure must stop before any topology bridge or the post hook.
            FailureCase {
                name: "xplane failure",
                failure: "xplane",
                expected_success: false,
                required: &["fixed-complete", "injected:xplane"],
                forbidden: &["xplane-complete", "--may-exist add-br br-pf3", "post"],
            },
            // Required bridge creation must fail the run rather than invoke the post hook.
            FailureCase {
                name: "required topology failure",
                failure: "required-topology",
                expected_success: false,
                required: &["xplane-complete", "injected:required-topology"],
                forbidden: &["post"],
            },
            // Representor attachment is explicitly best-effort and must still reach the post hook.
            FailureCase {
                name: "tolerated representor failure",
                failure: "tolerated-representor",
                expected_success: true,
                required: &["injected:tolerated-representor", "post"],
                forbidden: &[],
            },
            // A post-hook failure occurs last but must remain the raw script's final status.
            FailureCase {
                name: "post hook failure",
                failure: "post",
                expected_success: false,
                required: &["injected:post"],
                forbidden: &[],
            },
        ];
        // Exercise each boundary and verify its status plus the downstream stages it permits or blocks.
        for case in cases {
            let (output, stages) = run_astra_topology_bootstrap(case.failure);
            assert_eq!(
                output.status.success(),
                case.expected_success,
                "{}: unexpected status with stages {stages:?} and output {output:?}",
                case.name,
            );
            for marker in case.required {
                assert!(
                    stages.iter().any(|stage| stage.contains(marker)),
                    "{}: missing {marker:?} in {stages:?}",
                    case.name,
                );
            }
            for marker in case.forbidden {
                assert!(
                    stages.iter().all(|stage| !stage.contains(marker)),
                    "{}: unexpectedly reached {marker:?} in {stages:?}",
                    case.name,
                );
            }
        }
    }

    /// Verifies a failed rail lookup preserves the installed netplan and skips applying a partial
    /// candidate because incomplete xplane addressing must not replace working configuration.
    #[test]
    fn xplane_interface_mapping_uses_declared_logical_names() {
        use carbide_test_support::{Check, check_values};
        let fixture =
            std::env::temp_dir().join(format!("carbide-dpf-mapping-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&fixture).unwrap();
        let mapping = fixture.join("mapping.csv");
        // CSV ordering and unused entries must not determine the declared interface list.
        fs::write(&mapping, "pci_address,phys_port_name,cerebro_interface_name\n0005:03:00.1,p1,C1-1-L2\n0005:03:00.2,p2,C1-1-L3\n0005:03:00.0,p0,C1-1-L1\n").unwrap();
        check_values([
            Check {
                scenario: "join unaddressed ports by Cerebro name, keyed by logical name",
                input: r#""C1-1-L1||||||eth_r1_p0" "C1-1-L2||||||eth_r1_p1""#,
                expect: (true, "eth_r1_p0|0005:03:00.0|p0|C1-1-L1|eth_r1_p0\neth_r1_p1|0005:03:00.1|p1|C1-1-L2|eth_r1_p1\n".to_string()),
            },
            Check {
                scenario: "a declared Cerebro name must have a fixed mapping",
                input: r#""C9-1-L1||||||eth_r1_p0""#,
                expect: (false, String::new()),
            },
        ], |rows| {
            let script = [
                "set -euo pipefail\n".to_string(),
                format!("DPU_DEVICE_UNDERLAY_ROWS=({rows})\n"),
                "declare -a INTERFACES=()\ndeclare -A PCI_BY_INTERFACE=() PHYS_PORT_BY_INTERFACE=() CEREBRO_BY_INTERFACE=() INTERFACE_BY_CEREBRO=()\n".to_string(),
                "configure_mapping() {\n".to_string(),
                xplane_build_interface_named_mappings_script_raw().to_string(),
                "}\nconfigure_mapping\n".to_string(),
                r#"for logical in "${INTERFACES[@]}"; do
    cerebro="${CEREBRO_BY_INTERFACE[$logical]}"
    printf '%s|%s|%s|%s|%s\n' "$logical" "${PCI_BY_INTERFACE[$logical]}" "${PHYS_PORT_BY_INTERFACE[$logical]}" "$cerebro" "${INTERFACE_BY_CEREBRO[$cerebro]}"
done
"#.to_string(),
            ].concat();
            let output = Command::new("bash").arg("-c").arg(script)
                .env("MAPPING_FILE", &mapping)
                .output().unwrap();
            (output.status.success(), String::from_utf8(output.stdout).unwrap())
        });
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn xplane_altname_application_rechecks_ownership_and_stops_on_failure() {
        let fixture =
            std::env::temp_dir().join(format!("carbide-dpf-altnames-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&fixture).unwrap();
        let calls = fixture.join("calls");
        for (scenario, expected_calls) in [
            ("unassigned", "add target\nadd second\nnetplan\n"),
            ("new_owner", "del wrong\nadd target\nadd second\nnetplan\n"),
            ("now_correct", "add second\nnetplan\n"),
            ("delete_failure", "del wrong\n"),
            ("add_failure", "del wrong\nadd target\n"),
        ] {
            fs::write(&calls, "").unwrap();
            let setup = indoc::indoc! {r#"
                        set -euo pipefail
                        ALTNAMES_TO_ADD=(eth_r0_p0 eth_r0_p1)
                        declare -A NETDEV_BY_INTERFACES=([eth_r0_p0]=target [eth_r0_p1]=second)
                        OVS_CONFIG_SCRIPT=/dev/null
                        LLDP_TMP_FILE=lldp.tmp
                        LLDP_FILE=lldp.conf
                        netplan_tmp=netplan.tmp
                        NETPLAN_FILE=netplan.yaml
                        mv() { :; }
                        systemctl() { :; }
                        netplan() { builtin printf 'netplan\n' >> "$TEST_CALLS"; }
                        ip() {
                            if [ "$1" = -o ]; then
                                if [ "${@: -1}" = eth_r0_p1 ]; then return 1; fi
                                if [ "$TEST_CASE" = unassigned ]; then return 1; fi
                                if [ "$TEST_CASE" = now_correct ]; then
                                    builtin printf '1: target@peer: mtu 9216\n'
                                else
                                    builtin printf '2: wrong@peer: mtu 9216\n'
                                fi
                            else
                                builtin printf '%s %s\n' "$3" "$5" >> "$TEST_CALLS"
                                if [ "$TEST_CASE" = delete_failure ] && [ "$3" = del ]; then return 1; fi
                                if [ "$TEST_CASE" = add_failure ] && [ "$3" = add ] && [ "$5" = target ]; then return 1; fi
                            fi
                        }
                    "#};
            let script = format!(
                "{setup}{}",
                xplane_execute_verified_network_configuration_script_raw()
            );
            let output = Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("TEST_CALLS", &calls)
                .env("TEST_CASE", scenario)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                !scenario.ends_with("failure"),
                "{scenario}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                fs::read_to_string(&calls).unwrap(),
                expected_calls,
                "{scenario}"
            );
            if scenario.ends_with("failure") {
                assert!(String::from_utf8_lossy(&output.stderr).contains("eth_r0_p0"));
            }
        }
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn xplane_cerebro_checks_and_stages_files_before_changing_network() {
        for (scenario, succeeds) in [
            ("success", true),
            ("shared_group_ip", true),
            ("existing_altname", true),
            ("missing_netdev", false),
            ("conflicting_altname", true),
            ("missing_ovs_port", false),
            ("shared_ip_across_bridges", false),
            ("conflicting_group_ip", false),
            ("invalid_route", false),
            ("lldp_write", false),
            ("netplan_write", false),
        ] {
            let fixture = std::env::temp_dir()
                .join(format!("carbide-dpf-preflight-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&fixture).unwrap();
            let sysfs = fixture.join("pci");
            let mut mapping = "pci_address,phys_port_name,cerebro_interface_name\n".to_string();
            let mut rows = Vec::new();
            for index in 0..32 {
                let pci = format!("0005:03:00.{:x}", index / 4);
                let port = format!("p{}", index % 4);
                let netdev_name = format!("A{}p{}", index / 4, index % 4);
                let netdev = sysfs.join(&pci).join("net").join(netdev_name);
                fs::create_dir_all(&netdev).unwrap();
                if scenario != "missing_netdev" || index != 31 {
                    fs::write(netdev.join("phys_port_name"), format!("{port}\n")).unwrap();
                }
                mapping.push_str(&format!("{pci},{port},C{index}\n"));
                let logical = if scenario == "missing_ovs_port" && index == 31 {
                    "other_port".to_string()
                } else {
                    format!("eth_r{}_p{}", index / 8, index % 8)
                };
                let address = if index == 0
                    || (scenario == "shared_group_ip" && index < 4)
                    || (scenario == "shared_ip_across_bridges" && index == 31)
                {
                    "100.96.0.0/31"
                } else if scenario == "conflicting_group_ip" && index == 1 {
                    "100.96.0.2/31"
                } else {
                    ""
                };
                let route = if scenario == "invalid_route" {
                    "100.999.0.0/16"
                } else {
                    "100.96.0.0/16"
                };
                rows.push(format!(
                    "\"C{index}||{address}|100.96.0.1|{route}|100.96.0.0/13|{logical}\""
                ));
            }
            let mapping_file = fixture.join("mapping.csv");
            fs::write(&mapping_file, mapping).unwrap();
            let lldp = fixture.join("lldp.conf");
            let netplan = fixture.join("netplan.yaml");
            fs::write(&lldp, "original LLDP\n").unwrap();
            fs::write(&netplan, "original netplan\n").unwrap();
            let ovs = fixture.join("ovs.sh");
            fs::write(&ovs, "printf 'ovs\\n' >> \"$TEST_CALLS\"\n").unwrap();
            let calls = fixture.join("calls");
            let mut preamble = xplane_network_preamble_script_raw().to_string();
            let start = preamble
                .find("{{- range .dpu_device_underlay_rows }}")
                .unwrap();
            let end = start + preamble[start..].find("{{- end }}").unwrap() + "{{- end }}".len();
            preamble.replace_range(start..end, &rows.join("\n"));
            let stubs = indoc::indoc! {r#"
                        ip() {
                            if [ "$1" = -o ]; then
                                dev="${@: -1}"
                                if [[ "$dev" == eth_* || "$dev" == other_port ]]; then
                                    if [ "$TEST_CASE" = conflicting_altname ] && [ "$dev" = eth_r3_p7 ]; then
                                        builtin printf '99: wrong_netdev: mtu 9216\n'
                                    elif [ "$TEST_CASE" = existing_altname ] && [ "$dev" = eth_r0_p0 ]; then
                                        builtin printf '1: A0p0: mtu 9216\n'
                                    else
                                        return 1
                                    fi
                                else
                                    builtin printf '1: %s: mtu 9216\n' "$dev"
                                fi
                            elif [ "$1" = link ]; then
                                builtin printf 'altname %s\n' "$*" >> "$TEST_CALLS"
                            else
                                builtin printf 'inet 192.0.2.1/24\n'
                            fi
                        }
                        printf() {
                            if { [ "$TEST_CASE" = lldp_write ] && [[ "$1" == 'configure ports '* ]]; } ||
                                { [ "$TEST_CASE" = netplan_write ] && [ "${2:-}" = network: ]; }; then
                                builtin printf 'partial write\n'
                                return 1
                            fi
                            builtin printf "$@"
                        }
                        systemctl() { builtin printf 'systemctl\n' >> "$TEST_CALLS"; }
                        netplan() { builtin printf 'netplan\n' >> "$TEST_CALLS"; }
                    "#};
            let script = format!("{preamble}{stubs}{}", xplane_cerebro_network_script_raw());
            let output = Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("MAPPING_FILE", &mapping_file)
                .env(
                    "NETDEV_MAPPING_FILE",
                    fixture.join("logical-to-netdev-mapping"),
                )
                .env("SYSFS_PCI_DEVICES_DIR", &sysfs)
                .env("LLDP_FILE", &lldp)
                .env("NETPLAN_FILE", &netplan)
                .env("OVS_CONFIG_SCRIPT", &ovs)
                .env("TEST_CALLS", &calls)
                .env("TEST_CASE", scenario)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                succeeds,
                "{scenario}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let events = fs::read_to_string(&calls).unwrap_or_default();
            let published_mapping = fixture.join("logical-to-netdev-mapping");
            assert_eq!(published_mapping.exists(), succeeds, "{scenario}");
            if succeeds {
                let expected_mapping: String = (0..32)
                    .map(|index| {
                        format!(
                            "eth_r{}_p{}\tA{}p{}\n",
                            index / 8,
                            index % 8,
                            index / 4,
                            index % 4
                        )
                    })
                    .collect();
                assert_eq!(
                    fs::read_to_string(published_mapping).unwrap(),
                    expected_mapping,
                    "{scenario}"
                );
                assert_eq!(
                    events
                        .lines()
                        .filter(|line| line.starts_with("altname link property add "))
                        .count(),
                    if scenario == "existing_altname" {
                        31
                    } else {
                        32
                    }
                );
                if scenario == "conflicting_altname" {
                    let delete = events
                        .find("property del dev wrong_netdev altname eth_r3_p7")
                        .unwrap();
                    let add = events
                        .find("property add dev A7p3 altname eth_r3_p7")
                        .unwrap();
                    assert!(delete < add);
                    assert_eq!(events.matches("property del").count(), 1);
                } else {
                    assert!(!events.contains("property del"));
                }
                assert!(events.rfind("altname ").unwrap() < events.find("ovs\n").unwrap());
                assert!(events.find("ovs\n").unwrap() < events.find("netplan\n").unwrap());
                assert!(fs::read_to_string(&netplan).unwrap().contains("brcx-A0:"));
                if scenario == "shared_group_ip" {
                    let configuration: serde_yaml::Value =
                        serde_yaml::from_str(&fs::read_to_string(&netplan).unwrap()).unwrap();
                    assert_eq!(
                        configuration["network"]["ethernets"]
                            .as_mapping()
                            .unwrap()
                            .len(),
                        1
                    );
                    assert_eq!(
                        configuration["network"]["ethernets"]["brcx-A0"]["addresses"][0],
                        "100.96.0.0/31"
                    );
                }
            } else {
                assert!(
                    events.is_empty(),
                    "{scenario}: network changed before validation completed"
                );
                assert_eq!(fs::read_to_string(&lldp).unwrap(), "original LLDP\n");
                assert_eq!(fs::read_to_string(&netplan).unwrap(), "original netplan\n");
                if scenario.ends_with("write") {
                    assert!(
                        String::from_utf8_lossy(&output.stderr)
                            .contains("retained failed candidate")
                    );
                }
            }
            fs::remove_dir_all(fixture).unwrap();
        }
    }

    #[test]
    fn xplane_netdev_mapping_publishes_atomically_and_preserves_previous_on_failure() {
        let fixture = std::env::temp_dir().join(format!(
            "carbide-dpf-netdev-mapping-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&fixture).unwrap();
        let mapping = fixture.join("logical-to-netdev-mapping");
        for failure in ["none", "write", "rename"] {
            fs::write(&mapping, "original\n").unwrap();
            let setup = indoc::indoc! {r#"
                set -euo pipefail
                INTERFACES=(eth_r0_p0 eth_r2_p6)
                declare -A NETDEV_BY_INTERFACES=([eth_r0_p0]=A53p0 [eth_r2_p6]=A56p2)
                printf() {
                    if [ "$TEST_FAILURE" = write ]; then
                        builtin printf 'partial write\n'
                        return 1
                    fi
                    builtin printf "$@"
                }
                mv() {
                    # The active file must remain untouched until the final rename.
                    [ "$(cat "$NETDEV_MAPPING_FILE")" = original ] || exit 99
                    [ "$TEST_FAILURE" != rename ] || return 1
                    command mv "$@"
                }
            "#};
            let output = Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "{setup}{}",
                    xplane_write_netdev_logical_ifname_mapping_script_raw()
                ))
                .env("NETDEV_MAPPING_FILE", &mapping)
                .env("TEST_FAILURE", failure)
                .output()
                .unwrap();
            assert_eq!(
                output.status.success(),
                failure == "none",
                "{failure}: {output:?}"
            );
            assert_eq!(
                fs::read_to_string(&mapping).unwrap(),
                if failure == "none" {
                    "eth_r0_p0\tA53p0\neth_r2_p6\tA56p2\n"
                } else {
                    "original\n"
                },
                "{failure}"
            );
            assert_eq!(fs::read_dir(&fixture).unwrap().count(), 1, "{failure}");
        }
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn xplane_lldp_port_description_includes_server_name() {
        let fixture =
            std::env::temp_dir().join(format!("carbide-dpf-lldp-name-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&fixture).unwrap();
        for (server_name, expected_description) in [
            ("server-01", "server-01 C1-1-L1"),
            ("", "C1-1-L1"),
            (r#"server "one"\rack"#, r#"server \"one\"\\rack C1-1-L1"#),
        ] {
            let setup = indoc::indoc! {r#"
                set -euo pipefail
                INTERFACES=(eth_r0_p0)
                declare -A NETDEV_BY_INTERFACES=([eth_r0_p0]=A53p0)
                declare -A CEREBRO_BY_INTERFACE=([eth_r0_p0]=C1-1-L1)
            "#};
            let script = format!(
                "{setup}{}\ncat -- \"$LLDP_TMP_FILE\"\n",
                xplane_build_lldp_tmp_configuration_script_raw()
            );
            let output = Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("SERVER_NAME", server_name)
                .env("LLDP_FILE", fixture.join("lldp.conf"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{server_name:?}: {output:?}");
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "configure system interface pattern *\nconfigure lldp portidsubtype macaddress\nconfigure ports A53p0 lldp portdescription \"{expected_description}\"\n"
                )
            );
        }
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn xplane_cerebro_files_preserve_existing_contents_on_failure() {
        let fixture =
            std::env::temp_dir().join(format!("carbide-dpf-file-writes-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&fixture).unwrap();
        let active = fixture.join("active.conf");
        for (snippet, failure) in [
            (xplane_build_lldp_tmp_configuration_script_raw(), "write"),
            (xplane_build_lldp_tmp_configuration_script_raw(), "chmod"),
            (xplane_build_lldp_tmp_configuration_script_raw(), "rename"),
            (xplane_build_netplan_tmp_configuration_script_raw(), "write"),
            (
                xplane_build_netplan_tmp_configuration_script_raw(),
                "rename",
            ),
            (
                xplane_build_netplan_tmp_configuration_script_raw(),
                "lookup",
            ),
        ] {
            fs::write(&active, "original configuration\n").unwrap();
            let setup = r#"set -eu
    LLDP_FILE="$TEST_ACTIVE"
    NETPLAN_FILE="$TEST_ACTIVE"
    INTERFACES=(eth_r0_p0)
    declare -A NETDEV_BY_INTERFACES=([eth_r0_p0]=cx9_0)
    declare -A CEREBRO_BY_INTERFACE=([eth_r0_p0]=C1)
    declare -A BRIDGE_BY_CEREBRO=([C1]=brcx-A56)
    DPU_DEVICE_UNDERLAY_ROWS=("C1||100.96.0.0/31|100.96.0.1|100.96.0.0/16|100.96.0.0/13|eth_r0_p0")
    write_count=0
    printf() {
        if [[ "$1" == brcx-* ]]; then builtin printf "$@"; return; fi
        write_count=$((write_count + 1))
        if [ "$TEST_FAILURE" = write ] && [ "$write_count" -eq 2 ]; then
            builtin printf 'partial write\n'
            return 1
        fi
        builtin printf "$@"
    }
    chmod() {
        [ "$TEST_FAILURE" != chmod ] || return 1
        command chmod "$@"
    }
    mv() {
        [ "$TEST_FAILURE" != rename ] || return 1
        command mv "$@"
    }
    if [ "$TEST_FAILURE" = lookup ]; then BRIDGE_BY_CEREBRO=(); fi
    write_configuration() {
    "#;
            // A conditional call suppresses Bash errexit inside the function:
            // failures must be checked explicitly before replacing the active file.
            let preamble = xplane_network_preamble_script_raw();
            let preamble = preamble
                .split("# Build DPU_DEVICE_UNDERLAY_ROWS")
                .next()
                .unwrap();
            let candidate = if snippet == xplane_build_lldp_tmp_configuration_script_raw() {
                "$LLDP_TMP_FILE"
            } else {
                "$netplan_tmp"
            };
            let script = format!(
                "{preamble}{setup}{snippet}\nmv -fT -- \"{candidate}\" \"$TEST_ACTIVE\" || exit 1\n}}\nif write_configuration; then exit 0; else exit 1; fi\n"
            );
            let output = Command::new("bash")
                .arg("-c")
                .arg(script)
                .env("TEST_ACTIVE", &active)
                .env("TEST_FAILURE", failure)
                .output()
                .unwrap();
            assert!(!output.status.success(), "failure={failure}");
            assert_eq!(
                fs::read_to_string(&active).unwrap(),
                "original configuration\n",
                "failure={failure}"
            );
            let stderr = String::from_utf8(output.stderr).unwrap();
            let candidate = stderr
                .lines()
                .find(|line| line.contains("retained failed"))
                .and_then(|line| line.rsplit_once(" at "))
                .map(|(_, path)| path)
                .expect("failure must report the retained candidate path");
            let contents = fs::read_to_string(candidate).unwrap();
            assert!(!contents.is_empty(), "failure={failure}");
            if failure == "write" {
                assert!(contents.contains("partial write"));
            }
        }
        fs::remove_dir_all(&fixture).unwrap();
    }

    #[test]
    fn xplane_legacy_netplan_preserves_existing_file_on_failed_bridge_lookup() {
        let fixture =
            std::env::temp_dir().join(format!("carbide-dpf-netplan-{}", uuid::Uuid::new_v4()));
        let config = fixture.join("0005:03:00.0/net/A53p0/smart_nic/pf/config");
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        // Only the first MAC resolves, so generation fails after writing one bridge.
        fs::write(config, "00:00:00:00:00:00").unwrap();
        let netplan = fixture.join("99-cx9-rails.yaml");
        fs::write(&netplan, "original configuration\n").unwrap();
        let applied = fixture.join("applied");
        let legacy = xplane_legacy_network_script_raw();
        let mut script = legacy[legacy.find("NETPLAN_FILE=").unwrap()..]
            .replace(
                "NETPLAN_FILE=\"/etc/netplan/99-cx9-rails.yaml\"",
                "NETPLAN_FILE=\"$TEST_NETPLAN\"",
            )
            .replace("/sys/bus/pci/devices/", "$TEST_SYSFS/");
        let rows_start = script
            .find("{{- range .dpu_device_underlay_rows }}")
            .unwrap();
        let rows_end = script[rows_start..].find("\n)\n").unwrap() + rows_start;
        script.replace_range(
            rows_start..rows_end,
            r#"    "00:00:00:00:00:00|100.96.0.0/31|100.96.0.1|100.96.0.0/16|100.96.0.0/13"
    "00:00:00:00:00:01|100.96.0.2/31|100.96.0.3|100.96.0.0/16|100.96.0.0/13""#,
        );
        let output = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "netplan() {{ touch \"$TEST_APPLIED\"; }}\n{script}"
            ))
            .env("TEST_NETPLAN", &netplan)
            .env("TEST_SYSFS", &fixture)
            .env("TEST_APPLIED", &applied)
            .output()
            .unwrap();

        // Failure must leave the installed file byte-identical and never invoke `netplan apply`.
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(&netplan).unwrap(),
            "original configuration\n"
        );
        assert!(!applied.exists());
        let candidate = fs::read_dir(&fixture)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .contains(".tmp.")
            })
            .unwrap();
        // Prove the lookup failed after a partial write, and that the candidate was retained.
        assert!(
            fs::read_to_string(&candidate)
                .unwrap()
                .contains("brcx-A53:")
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(candidate.to_string_lossy().as_ref())
        );
        fs::remove_dir_all(&fixture).unwrap();
    }

    /// Executes the flavor-provided OVN address script with synthetic `ip` output.
    fn run_ovn_encap_script(addresses: &[&str]) -> Output {
        let ip_output = addresses
            .iter()
            .enumerate()
            .map(|(index, address)| {
                format!(
                    "{}: oob_net0 inet {address}/24 brd 10.0.0.255 scope global oob_net0",
                    index + 1
                )
            })
            .collect::<Vec<_>>()
            .join("\\n");
        let raw = ovn_encap_config_files()[0]
            .raw
            .clone()
            .expect("OVN script file must have inline content");
        let script = format!(
            "ip() {{ printf '%b' '{ip_output}'; }}\novs-vsctl() {{ printf '%s\\n' \"$*\"; }}\n{raw}"
        );
        Command::new("bash")
            .arg("-c")
            .arg(script)
            .output()
            .expect("bash must execute synthetic OVN address script")
    }

    /// Verifies BF3 peer bootstrap uses typed PF/VF names and never owns patch pairs.
    #[test]
    fn bf3_intercept_bridging_bootstrap_renders_expected_raw_representors() {
        // Render BF3 bootstrap for one configured PF and VF.
        let topology = intercept_bridging();
        let script =
            get_default_ovs_defaults_with_topology(Some(&topology), ServiceVpcSlots::default());

        // BF3 drops controller only from its platform raw-netdev convention.
        assert!(script.contains("host_representor='pf3hpf'"));
        assert!(script.contains("host_representor='pf3vf4'"));
        assert!(script.contains("_ovs-vsctl --may-exist add-br 'br-pf3'"));
        assert!(script.contains(
            "_ovs-vsctl --if-exists del-port \"$host_representor\" -- --may-exist add-port"
        ));
        assert!(script.contains("bridge-uplink 'p-vf4'"));
        assert!(script.contains("external_ids='{}' || true"));

        // DPF owns both patch ports; flavor bootstrap neither creates them nor edits legacy SFC.
        assert!(!script.contains("sfc.conf"));
        assert!(!script.contains("type=patch"));
        assert!(!script.contains("add-port 'br-pf3' 'p-pf3'"));
    }

    /// Verifies generic-BF4 exact semantic discovery and VF-name derivation.
    #[test]
    fn bf4_intercept_bridging_preflight_resolves_exact_pf_and_derives_vf_suffix() {
        // Render and execute preflight for the selected semantic PF identity.
        let topology = intercept_bridging();

        // Exact semantic names resolve independently of unrelated sysfs entries.
        let output = run_bf4_preflight(
            &topology,
            &[("en8f2", "c2pf3"), ("noise", "c2pf30")],
            "",
            "printf '%s' \"$dpf_c2p3_netdev\"",
        );
        assert!(output.status.success(), "preflight failed: {output:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "en8f2");

        // VF discovery is intentionally absent; the expected VF is the PF netdev plus its suffix.
        let script =
            get_bf4_ovs_defaults_with_topology(Some(&topology), ServiceVpcSlots::default());
        assert!(script.contains("host_representor=\"${dpf_c2p3_netdev}vf4\""));
        assert!(script.contains("external_ids='{}' || true"));
        assert!(!script.contains("phys_port_name 'c2pf3vf4'"));
    }

    /// Verifies generic-BF4 preflight fails before OVS mutation for missing or ambiguous PFs.
    #[test]
    fn bf4_intercept_bridging_preflight_rejects_missing_and_ambiguous_pf_matches() {
        // Include the required PF and one VF whose parent is the only discovery target.
        let topology = DpfInterceptBridging::new(
            vec![
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: None,
                    },
                    "br-pf3",
                    "p-pf3",
                ),
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: Some(4),
                    },
                    "br-vf4",
                    "p-vf4",
                ),
            ],
            16,
        )
        .unwrap();

        value_scenarios!(
            run = |matches: Vec<(&str, &str)>| {
                let output = run_bf4_preflight(&topology, &matches, "", "");
                (
                    output.status.success(),
                    String::from_utf8_lossy(&output.stderr).into_owned(),
                )
            };
            "missing exact PF" {
                // A similar semantic name must not satisfy exact matching.
                vec![("en8f2", "c2pf30")] => (
                    false,
                    "expected exactly one BF4 PF netdev with phys_port_name c2pf3; found 0\n".to_string(),
                ),
            }

            "ambiguous exact PF" {
                // Multiple exact matches are unsafe and must fail before bridge creation.
                vec![("en8f2", "c2pf3"), ("en9f2", "c2pf3")] => (
                    false,
                    "expected exactly one BF4 PF netdev with phys_port_name c2pf3; found 2\n".to_string(),
                ),
            }
        );
    }

    /// Verifies every generic-BF4 PF resolves before the first OVS mutation.
    #[test]
    fn bf4_intercept_bridging_preflight_precedes_all_ovs_mutation() {
        // Locate the final preflight call and the first inherited OVS cleanup operation.
        let topology = intercept_bridging();
        let script =
            get_bf4_ovs_defaults_with_topology(Some(&topology), ServiceVpcSlots::default());

        let final_resolution = script
            .find("resolve_dpf_pf 'c2pf3'")
            .expect("last configured PF must be resolved");
        let first_mutation = script
            .find("ovs-vsctl --if-exists del-br")
            .expect("base OVS cleanup must remain present");
        assert!(final_resolution < first_mutation);
    }

    /// Verifies each platform emits its provisioned VF count and resolved SF capacity because a
    /// wrong NVConfig value can overcommit hardware or unnecessarily reprovision its DPU.
    #[test]
    fn flavor_nvconfig_uses_platform_appropriate_vf_and_sf_counts() {
        // Extract nvconfig parameters uniformly across all flavor variants.
        let parameters =
            |flavor: DPUFlavor| flavor.spec.nvconfig.unwrap()[0].parameters.clone().unwrap();

        // BF3 and generic BF4 consume the validated site value.
        let bf3 = parameters(
            default_flavor_with_topology(
                "ns",
                &None,
                DpuDeploymentType::Bf3,
                3,
                61,
                None,
                None,
                ServiceVpcSlots::default(),
                &[],
                true,
            )
            .unwrap(),
        );
        assert!(bf3.contains(&"NUM_OF_VFS=3".to_string()));
        assert!(bf3.contains(&"PF_TOTAL_SF=61".to_string()));
        let generic_bf4 = parameters(
            flavor_bf4_with_topology(
                "ns",
                &None,
                5,
                63,
                None,
                None,
                ServiceVpcSlots::default(),
                &[],
                true,
            )
            .unwrap(),
        );
        assert!(generic_bf4.contains(&"NUM_OF_VFS=5".to_string()));
        assert!(generic_bf4.contains(&"PF_TOTAL_SF=63".to_string()));

        // Topology-free Astra retains 46 hardware VFs and uses 28 static service endpoints, eight
        // DOCA Weave DHCP Agent SFs, and four SFs of fixed headroom.
        let astra = parameters(DPUFlavor {
            metadata: ObjectMeta::default(),
            spec: astra_flavor_spec(&None, true),
        });
        assert!(astra.contains(&"NUM_OF_VFS=46".to_string()));
        assert!(astra.contains(&"PF_TOTAL_SF=40".to_string()));
    }

    #[test]
    fn host_boot_hold_is_configurable_for_zero_trust_flavors() {
        let default_spec = |deployment_type, enable_delay_host_init| {
            default_flavor_with_topology(
                "ns",
                &None,
                deployment_type,
                DEFAULT_DPU_NUM_OF_VFS,
                DEFAULT_PF_TOTAL_SF_RESERVED,
                None,
                None,
                ServiceVpcSlots::default(),
                &[],
                enable_delay_host_init,
            )
            .unwrap()
            .spec
        };
        let generic_bf4 = |enable_delay_host_init| {
            flavor_bf4_with_topology(
                "ns",
                &None,
                DEFAULT_DPU_NUM_OF_VFS,
                DEFAULT_PF_TOTAL_SF_RESERVED,
                None,
                None,
                ServiceVpcSlots::default(),
                &[],
                enable_delay_host_init,
            )
            .unwrap()
            .spec
        };

        check_cases(
            [
                Case {
                    scenario: "BF3",
                    input: default_spec(DpuDeploymentType::Bf3, true),
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "GB200 BF3",
                    input: default_spec(DpuDeploymentType::Bf3Gb200, true),
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "generic BF4",
                    input: generic_bf4(true),
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "disabled for BF3",
                    input: default_spec(DpuDeploymentType::Bf3, false),
                    expect: Yields((false, false)),
                },
                Case {
                    scenario: "disabled for generic BF4",
                    input: generic_bf4(false),
                    expect: Yields((false, false)),
                },
                Case {
                    scenario: "Astra",
                    input: astra_flavor_spec(&None, true),
                    expect: Yields((true, true)),
                },
                Case {
                    scenario: "disabled for Astra",
                    input: astra_flavor_spec(&None, false),
                    expect: Yields((false, false)),
                },
            ],
            |spec: DpuFlavorSpec| {
                let host_hold_enabled = spec.nvconfig.is_some_and(|entries| {
                    entries.iter().any(|entry| {
                        entry.parameters.as_ref().is_some_and(|parameters| {
                            parameters
                                .iter()
                                .any(|parameter| parameter == DELAY_HOST_OS_INIT_PARAMETER)
                        })
                    })
                });
                let waits_for_critical_pods = matches!(
                    spec.service_readiness.and_then(|readiness| readiness.gate),
                    Some(DpuFlavorServiceReadinessGate::DpuServiceCriticalPodsReady)
                );
                Ok::<_, ()>((host_hold_enabled, waits_for_critical_pods))
            },
        );
    }

    #[test]
    fn gb200_bf3_nvconfig_appends_the_bounded_profile_in_order() {
        let parameters = |deployment_type| {
            get_bf3_nvconfig(16, DEFAULT_PF_TOTAL_SF_RESERVED, deployment_type, true)
                .parameters
                .unwrap()
        };
        let bf3 = parameters(DpuDeploymentType::Bf3);
        let gb200 = parameters(DpuDeploymentType::Bf3Gb200);
        let expected_gb200_base = bf3
            .iter()
            .map(|parameter| match parameter.as_str() {
                "PF_TOTAL_SF=30" => "PF_TOTAL_SF=128".to_string(),
                _ => parameter.clone(),
            })
            .collect::<Vec<_>>();

        assert_eq!(bf3.len(), 17);
        assert_eq!(gb200.len(), 33);
        assert_eq!(&gb200[..bf3.len()], expected_gb200_base.as_slice());
        assert_eq!(
            &gb200[bf3.len()..],
            [
                "OFF_BOARD_SERIALIZER=1",
                "PCI_BUS00_HIERARCHY_TYPE=1",
                "PCI_BUS00_SPEED=5",
                "PCI_BUS00_WIDTH=5",
                "PCI_BUS10_HIERARCHY_TYPE=1",
                "PCI_BUS10_SPEED=4",
                "PCI_BUS10_WIDTH=3",
                "PCI_BUS12_HIERARCHY_TYPE=1",
                "PCI_BUS12_SPEED=4",
                "PCI_BUS12_WIDTH=3",
                "PCI_BUS14_HIERARCHY_TYPE=1",
                "PCI_BUS14_SPEED=4",
                "PCI_BUS14_WIDTH=3",
                "PCI_BUS16_HIERARCHY_TYPE=1",
                "PCI_BUS16_SPEED=4",
                "PCI_BUS16_WIDTH=3",
            ]
        );
        assert!(!gb200.contains(&"PCI_SWITCH0_UPSTREAM_PORT_BUS=0".to_string()));
        assert!(!gb200.contains(&"PCI_SWITCH0_UPSTREAM_PORT_PEX=0".to_string()));
    }

    /// Builds a flavor from operator-supplied bf.cfg parameters and returns the field they land in.
    fn bfcfg_parameters(deployment_type: DpuDeploymentType, extra: &[&str]) -> Option<Vec<String>> {
        let extra: Vec<String> = extra.iter().map(|p| (*p).to_string()).collect();
        // Astra is a DPUFlavorTemplate, so its spec comes back through the rendered body.
        if matches!(deployment_type, DpuDeploymentType::Bf4Astra) {
            let interfaces = crate::sdk::build_astra_dpu_interfaces_vec();
            let pf_total_sf =
                crate::sdk::calculate_astra_pf_total_sf(interfaces.as_slice()).unwrap();
            let template = flavor_bf4_astra("ns", &None, pf_total_sf, &extra, false).unwrap();
            return flavor_spec_from_template(&template).bfcfg_parameters;
        }
        default_flavor_for_with_topology(
            "ns",
            &None,
            deployment_type,
            DEFAULT_DPU_NUM_OF_VFS,
            DEFAULT_PF_TOTAL_SF_RESERVED,
            None,
            None,
            ServiceVpcSlots::default(),
            &extra,
            false,
        )
        .unwrap()
        .spec
        .bfcfg_parameters
    }

    /// A password line: a crypt(3) hash with `$` sections and operator-supplied shell quoting,
    /// both of which must survive unchanged.
    const PASSWORD_PARAMETER: &str = "ubuntu_PASSWORD='$6$rounds=5000$sa.lt$h/a.sh'";

    #[test]
    fn extra_bfcfg_parameters_are_appended_verbatim() {
        // BF3 and generic BF4 append operator input after their built-ins. Astra has no built-ins
        // and carries the operator's entries alone, leaving the field absent when there are none.
        // Spelled out rather than read back from the builders, so a changed built-in fails here.
        let built_ins = || {
            vec![
                "UPDATE_ATF_UEFI=yes".to_string(),
                "UPDATE_DPU_OS=yes".to_string(),
                "WITH_NIC_FW_UPDATE=yes".to_string(),
            ]
        };
        let with_extras = |extras: &[&str]| {
            let mut parameters = built_ins();
            parameters.extend(extras.iter().map(|p| (*p).to_string()));
            Some(parameters)
        };

        value_scenarios!(
            run = |(deployment_type, extra): (DpuDeploymentType, &[&str])| {
                bfcfg_parameters(deployment_type, extra)
            };

            "BF3 without extras keeps only its built-ins" {
                (DpuDeploymentType::Bf3, &[][..]) => Some(built_ins()),
            }

            "BF3 appends the password after its built-ins" {
                (DpuDeploymentType::Bf3, &[PASSWORD_PARAMETER][..])
                    => with_extras(&[PASSWORD_PARAMETER]),
            }

            "generic BF4 appends the password after its built-ins" {
                (DpuDeploymentType::Bf4Generic, &[PASSWORD_PARAMETER][..])
                    => with_extras(&[PASSWORD_PARAMETER]),
            }

            "multiple parameters retain configured order" {
                (DpuDeploymentType::Bf3, &["FIRST=1", "SECOND=2"][..])
                    => with_extras(&["FIRST=1", "SECOND=2"]),
            }

            "Astra without extras leaves bfcfgParameters absent" {
                // Absent, not empty: Astra has no built-ins, and emitting a list would change its
                // template hash and reprovision every Astra DPU with no operator change.
                (DpuDeploymentType::Bf4Astra, &[][..]) => None,
            }

            "Astra carries the operator's parameters alone" {
                (DpuDeploymentType::Bf4Astra, &[PASSWORD_PARAMETER][..])
                    => Some(vec![PASSWORD_PARAMETER.to_string()]),
            }

            "a value NICo would have to quote is passed through byte for byte" {
                (DpuDeploymentType::Bf3, &["RAW=\"$un touched'\\\""][..])
                    => with_extras(&["RAW=\"$un touched'\\\""]),
            }
        );
    }

    /// Builds a flavor for `deployment_type`, discarding the value, so error cases can be checked
    /// uniformly across the DPUFlavor and DPUFlavorTemplate variants.
    fn build_flavor(deployment_type: DpuDeploymentType, extra: &[&str]) -> Result<(), ()> {
        let extra: Vec<String> = extra.iter().map(|p| (*p).to_string()).collect();
        if matches!(deployment_type, DpuDeploymentType::Bf4Astra) {
            let interfaces = crate::sdk::build_astra_dpu_interfaces_vec();
            let pf_total_sf =
                crate::sdk::calculate_astra_pf_total_sf(interfaces.as_slice()).unwrap();
            return flavor_bf4_astra("ns", &None, pf_total_sf, &extra, true)
                .map(drop)
                .map_err(drop);
        }
        default_flavor_for_with_topology(
            "ns",
            &None,
            deployment_type,
            DEFAULT_DPU_NUM_OF_VFS,
            DEFAULT_PF_TOTAL_SF_RESERVED,
            None,
            None,
            ServiceVpcSlots::default(),
            &extra,
            false,
        )
        .map(drop)
        .map_err(drop)
    }

    #[test]
    fn go_template_delimiters_are_rejected_for_every_deployment_type() {
        // Astra's spec becomes a Go-rendered DPUFlavorTemplate body, so `{{` cannot survive there
        // as literal text. BF3 and generic BF4 would pass it through, but are held to the same
        // rule so the configuration contract does not vary by deployment type.
        scenarios!(
            run = |(deployment_type, parameter): (DpuDeploymentType, &str)| {
                build_flavor(deployment_type, &[parameter])
            };

            "BF3 rejects a Go action" {
                (DpuDeploymentType::Bf3, "SOME_KEY={{ .underlayIp }}") => Fails,
            }

            "generic BF4 rejects a Go action" {
                (DpuDeploymentType::Bf4Generic, "SOME_KEY={{ .underlayIp }}") => Fails,
            }

            "Astra rejects a Go action" {
                (DpuDeploymentType::Bf4Astra, "SOME_KEY={{ .underlayIp }}") => Fails,
            }

            "an unspaced Go action is rejected too" {
                (DpuDeploymentType::Bf4Astra, "SOME_KEY={{.underlayIp}}") => Fails,
            }

            "a password hash is accepted" {
                (DpuDeploymentType::Bf4Astra, PASSWORD_PARAMETER) => Yields(()),
            }

            "a lone closing delimiter is literal to the renderer and stays allowed" {
                (DpuDeploymentType::Bf4Astra, "SOME_KEY=a}}b") => Yields(()),
            }

            "a single brace is not a delimiter" {
                (DpuDeploymentType::Bf3, "SOME_KEY={value}") => Yields(()),
            }
        );
    }

    #[test]
    fn extra_bfcfg_parameters_change_the_flavor_name() {
        // The parameters are part of the hashed spec. A rename is what makes MachineUpdateManager
        // reprovision, which is the only path by which a changed value reaches an installed DPU.
        let name = |extra: &[&str]| {
            let extra: Vec<String> = extra.iter().map(|p| (*p).to_string()).collect();
            default_flavor_for_with_topology(
                "ns",
                &None,
                DpuDeploymentType::Bf3,
                DEFAULT_DPU_NUM_OF_VFS,
                DEFAULT_PF_TOTAL_SF_RESERVED,
                None,
                None,
                ServiceVpcSlots::default(),
                &extra,
                true,
            )
            .unwrap()
            .unique_name(DEFAULT_FLAVOR_NAME)
            .unwrap()
        };

        value_scenarios!(
            run = |(left, right): (&[&str], &[&str])| name(left) == name(right);

            "identical parameters keep the name stable" {
                (&[PASSWORD_PARAMETER][..], &[PASSWORD_PARAMETER][..]) => true,
            }

            "setting a parameter renames the flavor" {
                (&[][..], &[PASSWORD_PARAMETER][..]) => false,
            }

            "rotating the password renames the flavor" {
                (&[PASSWORD_PARAMETER][..], &["ubuntu_PASSWORD='$6$other'"][..]) => false,
            }

            "reordering renames the flavor, since order is preserved not normalized" {
                (&["FIRST=1", "SECOND=2"][..], &["SECOND=2", "FIRST=1"][..]) => false,
            }
        );
    }

    /// Verifies normalized input order cannot change rendered flavor identity.
    #[test]
    fn intercept_bridging_input_order_does_not_change_flavor_hash() {
        // Rebuild the same logical topology from reverse input order.
        let topology = intercept_bridging();
        let reversed =
            DpfInterceptBridging::new(topology.interfaces().iter().cloned().rev().collect(), 16)
                .expect("reordered topology fixture must remain valid");

        // Flavor hashing must depend on typed topology content, not legacy map iteration order.
        let flavor_name = |topology: &DpfInterceptBridging| {
            let interfaces = crate::sdk::build_effective_dpu_interfaces(16, Some(topology));
            default_flavor_with_topology(
                "ns",
                &None,
                DpuDeploymentType::Bf3,
                16,
                DEFAULT_PF_TOTAL_SF_RESERVED + 7,
                Some(topology),
                Some(&interfaces),
                ServiceVpcSlots::default(),
                &[],
                true,
            )
            .unwrap()
            .unique_name(DEFAULT_FLAVOR_NAME)
            .unwrap()
        };
        assert_eq!(flavor_name(&topology), flavor_name(&reversed));
    }

    /// Verifies the shared OVN address action enforces exact address cardinality.
    #[test]
    fn ovn_encap_script_requires_one_global_ipv4_address() {
        // Exercise the installed script with controlled management-address output.
        value_scenarios!(
            run = |addresses: &[&str]| {
                let output = run_ovn_encap_script(addresses);
                (
                    output.status.success(),
                    String::from_utf8_lossy(&output.stdout).into_owned(),
                )
            };
            "no global address" {
                // Provisioning must fail instead of leaving an empty OVN tunnel address.
                &[] as &[&str] => (false, String::new()),
            }

            "one global address" {
                // The sole address is persisted directly in Open_vSwitch external IDs.
                &["10.0.0.4"] as &[&str] => (
                    true,
                    "--timeout 15 set Open_vSwitch . external_ids:ovn-encap-ip=10.0.0.4\n".to_string(),
                ),
            }

            "multiple global addresses" {
                // Ambiguous management addressing must fail rather than select arbitrarily.
                &["10.0.0.4", "10.0.0.5"] as &[&str] => (false, String::new()),
            }
        );
    }

    /// Verifies BF3 and generic BF4 set the per-DPU OVN address during OVS provisioning.
    #[test]
    fn ovs_bootstrap_ends_with_ovn_encap_ip_configuration() {
        // Both rawConfigScript variants execute the strict oneshot body in a best-effort subshell.
        let expected = format!("(\n{}) || true\n", ovn_encap_ip_commands());
        value_scenarios!(
            run = |script: String| script.ends_with(&expected);
            "BF3 provisioning" {
                get_default_ovs_defaults_with_topology(None, ServiceVpcSlots::default()) => true,
            }

            // BF4 runs the operator's post-OVS hook last, so the encap-IP block is
            // the final NICo-authored step rather than the final line.
            "generic BF4 provisioning" {
                get_bf4_ovs_defaults_with_topology(None, ServiceVpcSlots::default()) => false,
            }
        );
        assert!(
            get_bf4_ovs_defaults_with_topology(None, ServiceVpcSlots::default())
                .contains(&expected)
        );
    }

    #[test]
    fn ovs_bootstrap_creates_service_vpc_slot_bridges() {
        let expected = concat!(
            "_ovs-vsctl --may-exist add-br br-svc-0\n",
            "_ovs-vsctl set bridge br-svc-0 datapath_type=netdev\n",
            "_ovs-vsctl set bridge br-svc-0 fail_mode=standalone\n",
        );

        let slots = ServiceVpcSlots::new(1).unwrap();
        assert!(get_default_ovs_defaults_with_topology(None, slots).contains(expected));
        assert!(get_bf4_ovs_defaults_with_topology(None, slots).contains(expected));
        assert!(
            !get_default_ovs_defaults_with_topology(None, ServiceVpcSlots::default())
                .contains("br-svc-")
        );
    }

    /// Every BF4 script must run the pre hook before any OVS work and the post
    /// hook after all of it. The post hook is appended separately and is easy to
    /// drop or misplace.
    #[test]
    fn bf4_scripts_run_pre_hook_first_and_post_hook_last() {
        // Matched against a real OVS operation, not the substring "ovs", which
        // also occurs inside the pre-hook's own filename.
        for (script, first_ovs_operation) in [
            // Topology-free generic BF4 still brackets its built-in OVS setup.
            (
                get_bf4_ovs_defaults_with_topology(None, ServiceVpcSlots::default()),
                "ovs-vsctl --if-exists del-br",
            ),
            // Configured generic BF4 brackets preflight and inherited OVS setup alike.
            (
                get_bf4_ovs_defaults_with_topology(
                    Some(&intercept_bridging()),
                    ServiceVpcSlots::default(),
                ),
                "ovs-vsctl --if-exists del-br",
            ),
            // Topology-free Astra brackets its unchanged fixed OVS child.
            (
                get_bf4_astra_ovs_defaults(None, true),
                "/etc/mellanox/ovs-script.sh",
            ),
            // Topology-backed Astra must enable child errexit before creating topology state.
            (
                get_bf4_astra_ovs_defaults(Some(&intercept_bridging()), true),
                "bash -e /etc/mellanox/ovs-script.sh",
            ),
        ] {
            let guard = |hook: &str| {
                let path = format!("/opt/dpf/extra-script-{hook}.sh");
                let line = format!("if [ -x {path} ]; then {path}; fi");
                let at = script
                    .find(&line)
                    .unwrap_or_else(|| panic!("missing guarded {hook} hook"));
                (at, line)
            };
            let (pre, _) = guard("pre-ovs");
            let (_, post_line) = guard("post-ovs");

            let first_ovs = script
                .find(first_ovs_operation)
                .expect("script must contain an OVS operation");
            assert!(pre < first_ovs, "pre-ovs hook must precede all OVS work");
            assert_eq!(
                script.trim_end().lines().last(),
                Some(post_line.as_str()),
                "post-ovs hook must be the final line"
            );
        }
    }

    #[test]
    fn astra_bootstrap_stops_before_post_hook_when_network_setup_fails() {
        let template = get_bf4_astra_ovs_defaults(None, true);
        let (prefix, branches) = template.split_once("{{ if ").unwrap();
        let (_, branches) = branches.split_once(" }}\n").unwrap();
        let (cerebro, legacy) = branches.split_once("{{ else }}\n").unwrap();
        let (_, suffix) = legacy.split_once("{{ end }}\n").unwrap();
        let script = format!("{prefix}{cerebro}{suffix}")
            .replace(
                "if [ -x /opt/dpf/extra-script-pre-ovs.sh ]; then /opt/dpf/extra-script-pre-ovs.sh; fi",
                "printf 'pre\n'",
            )
            .replace(
                "if [ -x /opt/dpf/extra-script-post-ovs.sh ]; then /opt/dpf/extra-script-post-ovs.sh; fi",
                "printf 'post\n'",
            )
            .replace(
                "/usr/local/sbin/configure-xplane-network.sh",
                "bash -c 'declare -F _ovs-vsctl >/dev/null || exit 99; printf \"network\\n\"; exit 7'",
            );
        let output = Command::new("bash").arg("-c").arg(script).output().unwrap();
        assert_eq!(output.status.code(), Some(7));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "pre\nnetwork\n");
    }

    /// The hook files must keep referencing the ConfigMaps the SDK seeds, under
    /// the key it writes, and stay executable agent-applied files.
    #[test]
    fn bf4_hook_config_files_reference_their_configmaps() {
        for (files, suffix) in [
            (
                get_config_files(&None, DpuDeploymentType::Bf4Generic, None).unwrap(),
                "bf4-generic",
            ),
            (
                get_bf4_astra_config_files(&None, None, true).unwrap(),
                "bf4-astra",
            ),
        ] {
            for hook in ["pre-ovs", "post-ovs"] {
                let path = format!("/opt/dpf/extra-script-{hook}.sh");
                let file = files
                    .iter()
                    .find(|f| f.path == path)
                    .unwrap_or_else(|| panic!("{suffix}: no config file for {path}"));
                let key_ref = file
                    .content_from
                    .as_ref()
                    .and_then(|c| c.config_map_key_ref.as_ref())
                    .unwrap_or_else(|| panic!("{suffix}: {path} must use configMapKeyRef"));

                assert_eq!(
                    key_ref.name.as_deref(),
                    Some(&*format!("extra-script-{hook}-{suffix}"))
                );
                assert_eq!(key_ref.key, "script");
                assert_eq!(file.permissions.as_deref(), Some("0755"));
                assert!(matches!(
                    file.r#type,
                    Some(DpuFlavorConfigFilesType::AgentApplied)
                ));
                assert!(file.raw.is_none(), "{suffix}: raw and contentFrom conflict");
            }
        }
    }

    /// Verifies the retained OVN oneshot is installed after network readiness and either OVS unit.
    #[test]
    fn ovn_encap_oneshot_has_required_systemd_ordering() {
        // Read the rendered unit and its DPUFlavor enablement request.
        let files = ovn_encap_config_files();
        let unit = files
            .iter()
            .find(|file| file.path.ends_with(OVN_ENCAP_SERVICE_NAME))
            .and_then(|file| file.raw.as_ref())
            .expect("OVN systemd unit must be rendered");
        let service = ovn_encap_systemd_service();

        // Both supported OVS unit names are ordered before the exact address action.
        assert!(unit.contains(
            "After=network-online.target openvswitch-switch.service openvswitch.service"
        ));
        assert!(unit.contains(&format!("ExecStart={OVN_ENCAP_SCRIPT_PATH}")));
        assert_eq!(service.name, OVN_ENCAP_SERVICE_NAME);
        assert!(matches!(
            service.operation,
            DpuFlavorSystemdServicesOperation::EnableAndStart
        ));
    }

    // ── validate_proxy_string ──────────────────────────────────────────────
    //
    // The pure validator at the heart of the proxy path. `DpfError` is not
    // `PartialEq`, so error rows use `Fails` (with `.map_err(drop)`).

    #[test]
    fn validate_proxy_string_accepts_and_rejects() {
        scenarios!(
            run = |value| validate_proxy_string(value, "field").map_err(drop);
            "typical proxy url" {
                "http://proxy.corp.example.com:3128" => Yields(()),
            }

            "empty string" {
                "" => Yields(()),
            }

            "cidr no_proxy entry" {
                "10.0.0.0/8" => Yields(()),
            }

            "hostname no_proxy entry" {
                "localhost" => Yields(()),
            }

            "dns suffix no_proxy entry" {
                ".svc.cluster.local" => Yields(()),
            }

            "high ascii printable is allowed" {
                "host~name" => Yields(()),
            }

            "space is allowed (>= 0x20, not quote/control)" {
                "has space" => Yields(()),
            }

            "tilde 0x7e is the last printable allowed" {
                "~" => Yields(()),
            }

            "double quote rejected" {
                "http://proxy:3128/\"evil" => Fails,
            }

            "newline rejected" {
                "http://proxy:3128\nEvil: injected" => Fails,
            }

            "carriage return rejected" {
                "http://proxy:3128\rinjected" => Fails,
            }

            "tab (control char) rejected" {
                "http://proxy:3128\tx" => Fails,
            }

            "null byte rejected" {
                "10.0.0.0/8\x00bad" => Fails,
            }

            "0x01 control char rejected" {
                "10.0.0.0/8\x01bad" => Fails,
            }

            "0x1f (last control below 0x20) rejected" {
                "x\x1fy" => Fails,
            }

            "DEL 0x7f rejected" {
                "x\x7fy" => Fails,
            }
        );
    }

    #[test]
    fn validate_proxy_string_error_names_the_field() {
        // The rejected-string error message mentions the field name passed in.
        scenarios!(
            run = |(value, field, tokens): (&str, &str, &[&str])| {
                let msg = match validate_proxy_string(value, field) {
                    Err(crate::error::DpfError::ConfigError(m)) => m,
                    other => return Err(format!("expected ConfigError, got {other:?}")),
                };
                Ok(tokens.iter().all(|t| msg.contains(t)))
            };
            "field name appears in the error" {
                ("\"", "https_proxy", &["https_proxy", "systemd"][..]) => Yields(true),
            }

            "no_proxy field name appears in the error" {
                ("\n", "no_proxy entry", &["no_proxy entry"][..]) => Yields(true),
            }
        );
    }

    // ── default_flavor: proxy validation flows through ─────────────────────

    #[test]
    fn default_flavor_accepts_or_rejects_proxy() {
        scenarios!(
            run = |p| default_flavor("ns", &p).map(drop).map_err(drop);
            "no proxy" {
                None => Yields(()),
            }

            "typical proxy with no_proxy list" {
                proxy(
                    "http://proxy.corp.example.com:3128",
                    &["10.0.0.0/8", "localhost", ".svc.cluster.local"],
                ) => Yields(()),
            }

            "proxy with empty no_proxy" {
                proxy("http://proxy:3128", &[]) => Yields(()),
            }

            "https_proxy with quote rejected" {
                proxy("http://proxy:3128/\"evil", &[]) => Fails,
            }

            "https_proxy with newline rejected" {
                proxy("http://proxy:3128\nEvil: injected", &[]) => Fails,
            }

            "https_proxy with carriage return rejected" {
                proxy("http://proxy:3128\rx", &[]) => Fails,
            }

            "no_proxy entry with control char rejected" {
                proxy("http://proxy:3128", &["10.0.0.0/8\x01bad"]) => Fails,
            }

            "no_proxy entry with DEL rejected" {
                proxy("http://proxy:3128", &["ok", "bad\x7f"]) => Fails,
            }

            "blank/whitespace-only no_proxy entries are skipped, not rejected" {
                proxy("http://proxy:3128", &["", "  ", "\t"]) => Yields(()),
            }
        );
    }

    // ── default_flavor: structural getters ─────────────────────────────────

    #[test]
    fn default_flavor_namespace_is_passed_through() {
        value_scenarios!(
            run = |ns| default_flavor(ns, &None).unwrap().metadata.namespace;
            "plain namespace" {
                "my-ns" => Some("my-ns".to_string()),
            }

            "empty namespace is still set verbatim" {
                "" => Some(String::new()),
            }

            "namespace with hyphens" {
                "dpf-system-test" => Some("dpf-system-test".to_string()),
            }
        );
    }

    #[test]
    fn default_flavor_metadata_name_is_always_none() {
        // The caller must set the name via unique_name(); the builder leaves it unset.
        value_scenarios!(
            run = |p| default_flavor("ns", &p).unwrap().metadata.name.is_none();
            "no proxy" {
                None => true,
            }

            "with proxy" {
                proxy("http://proxy:3128", &["localhost"]) => true,
            }
        );
    }

    #[test]
    fn default_flavor_spec_invariants() {
        // Structural shape of the default spec that callers depend on.
        let flavor = default_flavor("ns", &None).unwrap();
        value_scenarios!(
            run = |present| present;
            "dpu_mode is ZeroTrust" {
                matches!(flavor.spec.dpu_mode, Some(DpuFlavorDpuMode::ZeroTrust)) => true,
            }

            "bfcfg has three parameters" {
                flavor.spec.bfcfg_parameters.as_ref().map(|v| v.len()) == Some(3) => true,
            }

            "exactly one nvconfig entry" {
                flavor.spec.nvconfig.as_ref().map(|v| v.len()) == Some(1) => true,
            }

            "ovs raw config script is present" {
                flavor
                .spec
                .ovs
                .as_ref()
                .and_then(|o| o.raw_config_script.as_ref())
                .is_some() => true,
            }

            "dpu_resources unset" {
                flavor.spec.dpu_resources.is_none() => true,
            }

            "containerd_config unset" {
                flavor.spec.containerd_config.is_none() => true,
            }
        );
    }

    #[test]
    fn bf4_astra_flavor_spec_invariants() {
        let flavor_template = flavor_bf4_astra(
            "astra-ns",
            &None,
            crate::sdk::calculate_astra_pf_total_sf(
                crate::sdk::build_astra_dpu_interfaces_vec().as_slice(),
            )
            .unwrap(),
            &[],
            true,
        )
        .unwrap();
        let template_body: serde_yaml::Value =
            serde_yaml::from_str(&flavor_template.spec.template).unwrap();
        let template_fields = template_body
            .as_mapping()
            .expect("DPUFlavorTemplate body must be a YAML mapping");
        assert_eq!(template_fields.len(), 1);
        assert!(template_fields.contains_key("spec"));
        let flavor = DPUFlavor {
            metadata: ObjectMeta::default(),
            spec: flavor_spec_from_template(&flavor_template),
        };
        let expected_pf_total_sf = expected_astra_pf_total_sf_parameter();
        let ew_nic = flavor
            .spec
            .ew_nic_configurations
            .as_ref()
            .and_then(|configs| configs.first())
            .unwrap();
        let spectrum_x = ew_nic.spectrum_x_optimized.as_ref().unwrap();
        let grub_parameters = flavor
            .spec
            .grub
            .as_ref()
            .and_then(|grub| grub.kernel_parameters.as_ref())
            .unwrap();
        let nvconfig_parameters = flavor
            .spec
            .nvconfig
            .as_ref()
            .and_then(|configs| configs.first())
            .and_then(|config| config.parameters.as_ref())
            .unwrap();
        let ovs_script = flavor
            .spec
            .ovs
            .as_ref()
            .and_then(|ovs| ovs.raw_config_script.as_ref())
            .unwrap();
        let ovs_setup_script = flavor
            .spec
            .config_files
            .as_ref()
            .and_then(|files| {
                files
                    .iter()
                    .find(|file| file.path == "/etc/mellanox/ovs-script.sh")
            })
            .and_then(|file| file.raw.as_ref())
            .unwrap();

        value_scenarios!(
            run = |valid| valid;
            "namespace is passed through and name is left unset" {
                (
                    flavor_template.metadata.namespace.as_deref() == Some("astra-ns")
                        && flavor_template.metadata.name.is_none()
                ) => true,
            }

            "deprecated generic fields are unset" {
                (
                    flavor.spec.bfcfg_parameters.is_none()
                        && flavor.spec.dpu_mode.is_none()
                ) => true,
            }

            "network bay configuration selects conf1 with one VF" {
                (
                    ew_nic.num_vfs == 1
                        && ew_nic.link_type.is_none()
                        && ew_nic
                            .network_bay
                            .as_ref()
                            .is_some_and(|network_bay| network_bay.conf == "conf1")
                ) => true,
            }

            "Spectrum-X configuration selects the Astra profile" {
                (
                    spectrum_x.enabled
                        && matches!(
                            spectrum_x.multiplane_mode.as_ref(),
                            Some(
                                DpuFlavorEwNicConfigurationsSpectrumXOptimizedMultiplaneMode::Hwplb
                            )
                        )
                        && spectrum_x.number_of_planes == Some(4)
                        && matches!(
                            spectrum_x.overlay.as_ref(),
                            Some(DpuFlavorEwNicConfigurationsSpectrumXOptimizedOverlay::None)
                        )
                        && spectrum_x.version == "RA2.2-runtime"
                ) => true,
            }

            "Astra grub parameters include fixrttc and 8072 huge pages" {
                (
                    grub_parameters.iter().any(|parameter| parameter == "fixrttc")
                        && grub_parameters
                            .iter()
                            .any(|parameter| parameter == "hugepages=8072")
                ) => true,
            }

            "Astra nvconfig requests endpoint-derived total SFs and 46 VFs" {
                (
                    nvconfig_parameters
                        .iter()
                        .any(|parameter| parameter == &expected_pf_total_sf)
                        && nvconfig_parameters
                            .iter()
                            .any(|parameter| parameter == "NUM_OF_VFS=46")
                ) => true,
            }

            "OVS bootstrap invokes the ordered Astra network setup" {
                ovs_script.contains("/usr/local/sbin/configure-xplane-network.sh || exit $?") => true,
            }

            "Astra network setup enables xplane and Weave metrics" {
                {
                    [ovs_script.as_str(), ovs_cerebro_script_raw()].into_iter().all(|script| {
                        script.contains("'other_config:flow-metric-labels=\"to_plane,from_plane,device_name,group,plane\"'")
                            && script.contains("other_config:doca-telemetry-interval=\"1000\"")
                            && script.contains("other_config:doca-telemetry-ipc=\"true\"")
                            && script.contains("other_config:doca-telemetry-source-id=\"xplane\"")
                    })
                } => true,
            }

            "OVS bootstrap recreates the HBN bridge" {
                ovs_setup_script
                    .find("_ovs-vsctl --if-exists del-br br-hbn")
                    .zip(ovs_setup_script.find("_ovs-vsctl --may-exist add-br br-hbn"))
                    .is_some_and(|(delete_bridge, add_bridge)| delete_bridge < add_bridge) => true,
            }

            "xplane network setup uses the addressed DPUDevice interfaces" {
                {
                    let xplane_script = flavor
                        .spec.config_files.as_ref().unwrap().iter()
                        .find(|file| file.path == "/usr/local/sbin/configure-xplane-network.sh")
                        .and_then(|file| file.raw.as_ref()).unwrap();
                    xplane_script.contains("{{- range .dpu_device_underlay_rows }}")
                        && xplane_script.contains("{{ .logical_ifname }}")
                        && xplane_script.contains("{{ .cerebro_ifname }}")
                        && xplane_script.contains("{{ .ip }}")
                        && xplane_script.contains("{{ .gateway }}")
                        && xplane_script.contains("{{ .rail_route }}")
                        && xplane_script.contains("{{ .sw_plane_route }}")
                } => true,
            }

            "xplane port mapping uses the Astra interface, PCI, bridge, and group rows" {
                {
                    let ovs_setup_script = flavor
                        .spec
                        .config_files
                        .as_ref()
                        .unwrap()
                        .iter()
                        .find(|file| file.path == "/etc/mellanox/ovs-script.sh")
                        .and_then(|file| file.raw.as_ref())
                        .unwrap();
                    ovs_setup_script.contains("IFS='|' read -r iface_prefix pci_address bridge group_id")
                        && [
                            "A53|0005:03:00.0|brcx-A53|A53",
                            "A56|0005:06:00.0|brcx-A56|A56",
                            "A43|0004:03:00.0|brcx-A43|A43",
                            "A46|0004:06:00.0|brcx-A46|A46",
                            "A3|0000:03:00.0|brcx-A3|A3",
                            "A6|0000:06:00.0|brcx-A6|A6",
                            "A13|0001:03:00.0|brcx-A13|A13",
                            "A16|0001:06:00.0|brcx-A16|A16",
                        ]
                        .into_iter()
                        .all(|row| ovs_setup_script.contains(row))
                } => true,
            }

            "xplane bridge setup waits for OOB connectivity" {
                {
                    let xplane_script = flavor
                        .spec
                        .config_files
                        .as_ref()
                        .unwrap()
                        .iter()
                        .find(|file| file.path == "/usr/local/sbin/configure-xplane-network.sh")
                        .and_then(|file| file.raw.as_ref())
                        .unwrap();
                    xplane_script.contains("OOB_WAIT_TIMEOUT=120")
                        && xplane_script.contains(
                            "ip -4 -o addr show dev \"$OOB_IFACE\" scope global",
                        )
                        && xplane_script.contains(
                            "timed out after ${OOB_WAIT_TIMEOUT}s waiting for ${OOB_IFACE} to have an IP",
                        )
                } => true,
            }

            "empty containerd/sysctl/packages/systemdServices placeholders are set" {
                (
                    flavor.spec.containerd_config.is_some()
                        && flavor
                            .spec
                            .sysctl
                            .as_ref()
                            .is_some_and(|sysctl| {
                                sysctl.parameters.as_ref().is_some_and(Vec::is_empty)
                            })
                        && flavor
                            .spec
                            .packages
                            .as_ref()
                            .is_some_and(Vec::is_empty)
                        && flavor
                            .spec
                            .systemd_services
                            .as_ref()
                            .is_some_and(Vec::is_empty)
                ) => true,
            }

            "Astra includes the HBN configuration files" {
                {
                    let paths = flavor
                        .spec
                        .config_files
                        .as_ref()
                        .unwrap()
                        .iter()
                        .map(|file| file.path.as_str())
                        .collect::<BTreeSet<_>>();
                    [
                        "/var/lib/hbn/etc/supervisor/conf.d/acltool.conf",
                        "/var/lib/hbn/etc/cumulus/acl/policy.d/10-dhcp.rules",
                        "/etc/lldpd.d/lldp-interfaces.conf",
                        "/etc/default/lldpd",
                    ]
                    .into_iter()
                    .all(|path| paths.contains(path))
                } => true,
            }

            "ewNic rawNvConfig has correct programmable CC and locality mode" {
                {
                    let raw = ew_nic.raw_nv_config.as_ref().unwrap();
                    let programmable_cc = raw.iter().find(|entry| {
                        entry.name == "USER_PROGRAMMABLE_CC"
                    });
                    let locality = raw.iter().find(|entry| {
                        entry.name == "TX_SCHEDULER_LOCALITY_MODE"
                    });
                    raw.iter()
                        .filter(|entry| entry.name == "ROCE_ADAPTIVE_ROUTING_EN")
                        .count()
                        == 1
                        && programmable_cc.is_some_and(|entry| entry.value == "1")
                        && locality.is_some_and(|entry| entry.value == "2")
                } => true,
            }
        );
    }

    #[test]
    fn bf4_astra_proxy_config_file_count() {
        value_scenarios!(
            run = |(p, enabled)| {
                let template = flavor_bf4_astra_with_topology(
                    "astra-ns", &p, 128, None, None, &[], true, enabled,
                ).unwrap();
                let spec = flavor_spec_from_template(&template);
                assert_eq!(
                    spec.ovs.unwrap().raw_config_script.unwrap()
                        .contains("/usr/local/sbin/initialize-spx-maintenance.sh"),
                    enabled,
                );
                let files = spec.config_files.unwrap();
                assert_eq!(
                    files.iter().any(|file| file.path == "/usr/local/sbin/initialize-spx-maintenance.sh"),
                    enabled,
                );
                let proxy_file_count = files
                    .iter()
                    .filter(|file| {
                        file.path
                            == "/etc/systemd/system/containerd.service.d/socks-proxy.conf"
                    })
                    .count();
                (files.len(), proxy_file_count)
            };
            "no proxy keeps the sixteen Astra base files" {
                (None, true) => (16, 0),
            }

            "configured proxy appends exactly one proxy file" {
                (proxy("http://proxy:3128", &["10.0.0.0/8", "localhost"]), true) => (17, 1),
            }
            "disabled maintenance omits its initialization file and call" {
                (None, false) => (15, 0),
            }
        );
    }

    // ── get_config_files: count and trailing-file fields ───────────────────

    #[test]
    fn config_file_count_depends_on_proxy() {
        value_scenarios!(
            run = |p| {
                default_flavor("ns", &p)
                    .unwrap()
                    .spec
                    .config_files
                    .unwrap()
                    .len()
            };
            "no proxy yields eleven base files" {
                None => 11,
            }

            "proxy with empty no_proxy appends a twelfth" {
                proxy("http://proxy:3128", &[]) => 12,
            }

            "proxy with no_proxy list still appends exactly one" {
                proxy("http://proxy:3128", &["10.0.0.0/8", "localhost"]) => 12,
            }
        );
    }

    #[test]
    fn every_deployment_type_includes_hbn_apparmor_extensions() {
        value_scenarios!(
            run = |deployment_type| {
                let files = match deployment_type {
                    DpuDeploymentType::Bf4Astra => {
                        astra_flavor_spec(&None, true).config_files.unwrap()
                    }
                    deployment_type => default_flavor_for("ns", &None, deployment_type)
                        .unwrap()
                        .spec
                        .config_files
                        .unwrap(),
                };
                let has_file = |path: &str, raw: &str| {
                    files.iter().find(|file| file.path == path).is_some_and(|file| {
                        matches!(
                            file.operation,
                            Some(DpuFlavorConfigFilesOperation::Override)
                        ) && file.permissions.as_deref() == Some("0644")
                            && file.raw.as_deref() == Some(raw)
                            && file.content_from.is_none()
                            && file.r#type.is_none()
                    })
                };

                has_file(
                    "/etc/apparmor.d/local/usr.sbin.rsyslogd",
                    concat!(
                        "signal (receive) peer=runc,\n",
                        "capability chown,\n",
                        "/usr/{bin,sbin}/* ixr,\n",
                        "/etc/logrotate.d/* rk,\n",
                        "/var/lib/logrotate/{,**} rwk,\n",
                    ),
                ) && has_file(
                    "/etc/apparmor.d/local/usr.bin.tcpdump",
                    "signal (receive) peer=runc,\n",
                )
            };
            "BF3" {
                DpuDeploymentType::Bf3 => true,
            }

            "GB200 BF3" {
                DpuDeploymentType::Bf3Gb200 => true,
            }

            "generic BF4" {
                DpuDeploymentType::Bf4Generic => true,
            }

            "Astra BF4" {
                DpuDeploymentType::Bf4Astra => true,
            }
        );
    }

    #[test]
    fn proxy_file_fields_are_fixed() {
        // path, permissions, operation of the trailing proxy drop-in.
        let flavor = default_flavor("ns", &proxy("http://proxy:3128", &[])).unwrap();
        let files = flavor.spec.config_files.unwrap();
        let f = files.last().unwrap();
        value_scenarios!(
            run = |ok| ok;
            "path" {
                f.path == "/etc/systemd/system/containerd.service.d/socks-proxy.conf" => true,
            }

            "permissions 0644" {
                f.permissions.as_deref() == Some("0644") => true,
            }

            "override operation" {
                matches!(f.operation, Some(DpuFlavorConfigFilesOperation::Override)) => true,
            }
        );
    }

    #[test]
    fn base_config_file_paths_are_present() {
        // The base files always exist regardless of proxy, with these paths.
        let files = default_flavor("ns", &None)
            .unwrap()
            .spec
            .config_files
            .unwrap();
        let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        value_scenarios!(
            run = |path| paths.contains(&path.to_string());
            "acltool.conf" {
                "/var/lib/hbn/etc/supervisor/conf.d/acltool.conf" => true,
            }

            "10-dhcp.rules" {
                "/var/lib/hbn/etc/cumulus/acl/policy.d/10-dhcp.rules" => true,
            }

            "lldp-interfaces.conf" {
                "/etc/lldpd.d/lldp-interfaces.conf" => true,
            }

            "lldpd defaults" {
                "/etc/default/lldpd" => true,
            }

            "mlnx-bf.conf" {
                "/etc/mellanox/mlnx-bf.conf" => true,
            }

            "mlnx-ovs.conf" {
                "/etc/mellanox/mlnx-ovs.conf" => true,
            }

            "mlnx-sf.conf" {
                "/etc/mellanox/mlnx-sf.conf" => true,
            }
        );
    }

    #[test]
    fn lldp_config_file_contents_are_fixed() {
        let files = default_flavor("ns", &None)
            .unwrap()
            .spec
            .config_files
            .unwrap();
        value_scenarios!(
            run = |(path, expected_raw): (&str, &str)| {
                files.iter().find(|file| file.path == path).is_some_and(|file| {
                    matches!(file.operation, Some(DpuFlavorConfigFilesOperation::Override))
                        && file.permissions.as_deref() == Some("0644")
                        && file.raw.as_deref() == Some(expected_raw)
                        && file.content_from.is_none()
                        && file.r#type.is_none()
                })
            };
            "LLDP interface pattern permits every interface" {
                (
                    "/etc/lldpd.d/lldp-interfaces.conf",
                    "configure system interface pattern *\n",
                ) => true,
            }

            "lldpd enables LLDP-MED inventory" {
                ("/etc/default/lldpd", "DAEMON_ARGS=\"-M 1\"\n") => true,
            }
        );
    }

    // ── proxy drop-in raw body content ─────────────────────────────────────
    //
    // `.contains(...)` substring checks folded into (value, &[tokens]) rows.

    #[test]
    fn proxy_raw_contains_expected_tokens() {
        check_cases(
            [Case {
                scenario: "uppercase and lowercase HTTPS_PROXY env set under [Service]",
                input: (
                    proxy_file_raw("http://proxy.example.com:3128", &[]),
                    &[
                        "[Service]",
                        "HTTPS_PROXY=http://proxy.example.com:3128",
                        "https_proxy=http://proxy.example.com:3128",
                    ][..],
                ),
                expect: Yields(true),
            }],
            |(raw, tokens): (String, &[&str])| Ok::<_, ()>(tokens.iter().all(|t| raw.contains(t))),
        );
    }

    #[test]
    fn proxy_raw_no_proxy_handling() {
        // When no_proxy is empty the NO_PROXY env lines are omitted; when set they
        // appear sorted+deduped. Each row: (raw body, tokens that must all appear).
        check_cases(
            [
                Case {
                    scenario: "no_proxy lines present, sorted and deduped",
                    input: (
                        proxy_file_raw(
                            "http://proxy:3128",
                            &["localhost", "10.0.0.0/8", "10.0.0.0/8"],
                        ),
                        &[
                            "NO_PROXY=10.0.0.0/8,localhost",
                            "no_proxy=10.0.0.0/8,localhost",
                        ][..],
                    ),
                    expect: Yields(true),
                },
                Case {
                    scenario: "single no_proxy entry",
                    input: (
                        proxy_file_raw("http://proxy:3128", &["10.0.0.0/8"]),
                        &["NO_PROXY=10.0.0.0/8", "no_proxy=10.0.0.0/8"][..],
                    ),
                    expect: Yields(true),
                },
                Case {
                    scenario: "whitespace around entries is trimmed",
                    input: (
                        proxy_file_raw("http://proxy:3128", &["  localhost  ", " 10.0.0.0/8 "]),
                        &["NO_PROXY=10.0.0.0/8,localhost"][..],
                    ),
                    expect: Yields(true),
                },
            ],
            |(raw, tokens): (String, &[&str])| Ok::<_, ()>(tokens.iter().all(|t| raw.contains(t))),
        );
    }

    #[test]
    fn proxy_raw_omits_no_proxy_when_effectively_empty() {
        // Empty or blank-only no_proxy lists produce no NO_PROXY env lines at all.
        value_scenarios!(
            run = |raw| raw.contains("NO_PROXY") || raw.contains("no_proxy");
            "empty list" {
                proxy_file_raw("http://proxy:3128", &[]) => false,
            }

            "blank and whitespace-only entries are filtered out" {
                proxy_file_raw("http://proxy:3128", &["", "   ", "\t"]) => false,
            }
        );
    }

    // ── unique_name ────────────────────────────────────────────────────────

    #[test]
    fn unique_name_has_expected_format() {
        // "<prefix>-<16 lowercase hex chars>" for several prefixes.
        scenarios!(
            run = |prefix: &str| {
                let flavor = default_flavor("ns", &None).map_err(drop)?;
                let name = flavor.unique_name(prefix).map_err(drop)?;
                let (got_prefix, hash) = name.rsplit_once('-').ok_or(())?;
                Ok::<bool, ()>(
                    got_prefix == prefix
                        && hash.len() == 16
                        && hash
                            .chars()
                            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                )
            };
            "standard prefix" {
                "dpu-flavor" => Yields(true),
            }

            "empty prefix still yields prefix-<hash>" {
                "" => Yields(true),
            }

            "prefix containing hyphens" {
                "a-b-c" => Yields(true),
            }
        );
    }

    #[test]
    fn unique_name_equality_across_specs() {
        // true  => the two specs hash to the same name (stable / order- & dup-insensitive)
        // false => the specs differ, so the names must differ
        value_scenarios!(
            run = |(a, b)| a == b;
            "deterministic for identical specs" {
                (name_for(&None), name_for(&None)) => true,
            }

            "no_proxy order does not affect the name" {
                (
                    name_for(&proxy("http://proxy:3128", &["localhost", "10.0.0.0/8"])),
                    name_for(&proxy("http://proxy:3128", &["10.0.0.0/8", "localhost"])),
                ) => true,
            }

            "duplicate no_proxy entries do not affect the name" {
                (
                    name_for(&proxy("http://proxy:3128", &["10.0.0.0/8"])),
                    name_for(&proxy("http://proxy:3128", &["10.0.0.0/8", "10.0.0.0/8"])),
                ) => true,
            }

            "adding a proxy changes the name" {
                (name_for(&None), name_for(&proxy("http://proxy:3128", &[]))) => false,
            }

            "extending the no_proxy list changes the name" {
                (
                    name_for(&proxy("http://proxy:3128", &["10.0.0.0/8"])),
                    name_for(&proxy("http://proxy:3128", &["10.0.0.0/8", "localhost"])),
                ) => false,
            }

            "changing the https_proxy url changes the name" {
                (
                    name_for(&proxy("http://a:3128", &[])),
                    name_for(&proxy("http://b:3128", &[])),
                ) => false,
            }
        );
    }

    #[test]
    fn unique_name_prefix_changes_the_output() {
        // The same spec under different prefixes yields different names.
        let flavor = default_flavor("ns", &None).unwrap();
        value_scenarios!(
            run = |(a, b)| a == b;
            "different prefixes differ" {
                (
                    flavor.unique_name("a").unwrap(),
                    flavor.unique_name("b").unwrap(),
                ) => false,
            }

            "same prefix matches" {
                (
                    flavor.unique_name("x").unwrap(),
                    flavor.unique_name("x").unwrap(),
                ) => true,
            }
        );
    }

    // ── dhcp_acl_rules (pure formatter) ────────────────────────────────────

    #[test]
    fn dhcp_acl_rules_shape() {
        let rules = dhcp_acl_rules(None);
        value_scenarios!(
            run = |v| v;
            "starts with the iptables header" {
                rules.starts_with("[iptables]\n") => true,
            }

            "covers the host-facing pf0hpf interface" {
                rules.contains("--physdev-in pf0hpf_if ") => true,
            }

            "covers vf0" {
                rules.contains("--physdev-in pf0vf0_if ") => true,
            }

            "covers vf15 (last in range)" {
                rules.contains("--physdev-in pf0vf15_if ") => true,
            }

            "does not over-run to vf16" {
                rules.contains("pf0vf16_if") => false,
            }

            "header line plus 17 rule lines (hpf + vf0..15)" {
                rules.lines().count() == 18 => true,
            }

            "every rule drops DHCP broadcast to .255" {
                rules.matches("-d 255.255.255.255").count() == 17 => true,
            }
        );
    }

    /// Verifies configured ACLs follow the exact sparse PF/VF inventory.
    #[test]
    fn configured_dhcp_acl_rules_follow_effective_inventory() {
        // Build one PF and the highest VMaaS-addressable VF as a sparse inventory.
        let topology = DpfInterceptBridging::new(
            vec![
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: None,
                    },
                    "br-pf3",
                    "p-pf3",
                ),
                DpfInterceptBridge::new(
                    DpfInterfaceIdentity {
                        controller_id: 2,
                        pf_id: 3,
                        vf_id: Some(15),
                    },
                    "br-vf15",
                    "p-vf15",
                ),
            ],
            16,
        )
        .expect("sparse VF15 topology must be valid");
        let interfaces = crate::sdk::build_effective_dpu_interfaces(16, Some(&topology));
        let rules = dhcp_acl_rules(Some(&interfaces));

        // Only the configured host-facing HBN endpoints should receive ACL rules.
        value_scenarios!(
            run = |value| value;
            "selected PF is covered" {
                // The selected hardware PF is exposed to HBN through canonical logical PF0.
                rules.contains("--physdev-in pf0hpf_if ") => true,
            }

            "sparse VF15 is covered" {
                // The highest VMaaS-addressable VF retains the broadcast policy.
                rules.contains("--physdev-in pf0vf15_if ") => true,
            }

            "unconfigured VF14 is absent" {
                // Exact inventory generation must not restore the old unconditional range.
                rules.contains("pf0vf14_if") => false,
            }

            "physical uplinks are absent" {
                // Fixed physical endpoints are not host-facing DHCP ingress interfaces.
                rules.contains("--physdev-in p0_if ") => false,
            }

            "header plus selected PF and VF rules" {
                // The sparse inventory contains exactly two host-facing endpoints.
                rules.lines().count() == 3 => true,
            }
        );
    }

    // ── get_default_ovs_defaults (pure formatter) ──────────────────────────

    #[test]
    fn ovs_defaults_contains_key_lines() {
        check_cases(
            [Case {
                scenario: "doca/offload/br-sfc setup lines present",
                input: (
                    get_default_ovs_defaults_with_topology(None, ServiceVpcSlots::default()),
                    &[
                        "other_config:doca-init=true",
                        "other_config:hw-offload=true",
                        "add-br br-sfc",
                        "datapath_type=netdev",
                        "type=dpdk",
                        "mtu_request=9216",
                        "_ovs-vsctl --if-exists del-br br-hbn\n_ovs-vsctl --may-exist add-br br-hbn",
                    ][..],
                ),
                expect: Yields(true),
            }],
            |(raw, tokens): (String, &[&str])| Ok::<_, ()>(tokens.iter().all(|t| raw.contains(t))),
        );
    }

    // ── get_default_nvconfig (pure constructor) ────────────────────────────

    #[test]
    fn default_nvconfig_shape() {
        let nv = get_bf3_nvconfig(
            16,
            DEFAULT_PF_TOTAL_SF_RESERVED,
            DpuDeploymentType::Bf3,
            false,
        );
        value_scenarios!(
            run = |v| v;
            "device is the only allowed wildcard variant" {
                matches!(nv.device, Some(DpuFlavorNvconfigDevice::KopiumVariant0)) => true,
            }

            "parameter count" {
                nv.parameters.as_ref().map(|p| p.len()) == Some(16) => true,
            }

            "carries the SRIOV enable flag" {
                nv
                .parameters
                .as_ref()
                .map(|p| p.iter().any(|s| s == "SRIOV_EN=1"))
                == Some(true) => true,
            }

            "carries NUM_OF_VFS=16" {
                nv
                .parameters
                .as_ref()
                .map(|p| p.iter().any(|s| s == "NUM_OF_VFS=16"))
                == Some(true) => true,
            }

            "carries the legacy default PF_TOTAL_SF" {
                nv
                    .parameters
                    .as_ref()
                    .map(|p| p.iter().any(|s| s == "PF_TOTAL_SF=30"))
                    == Some(true) => true,
            }
        );
    }
}
