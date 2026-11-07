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

use ::rpc::MachineDiscoveryReporter;
use carbide_host_support::hardware_enumeration::enumerate_hardware;
use carbide_host_support::registration;
use carbide_host_support::registration::RegistrationError;
use carbide_uuid::machine::MachineId;
use tracing::info;
use tss_esapi::Context;
use tss_esapi::handles::KeyHandle;

use crate::{CarbideClientError, attestation as attest, platform, tpm};

// Sentinel MAC reported by udev for Mellanox SFs and VFs that have no real MAC
// address. Mirrors carbide_network::MELLANOX_SF_VF_MAC_ADDRESS_IN without
// adding that crate as a dependency.
const MELLANOX_SF_VF_MAC_ADDRESS_IN: &str = "ch:64";

// Mellanox SFs and VFs that have no real MAC address report the sentinel above from udev.
// VFs additionally appear under a "virtfn" sysfs path segment.
// Neither is useful to the host inventory model.
fn is_auxiliary_interface(nic: &::rpc::machine_discovery::NetworkInterface) -> bool {
    let is_sf_or_vf_mac = nic.mac_address == MELLANOX_SF_VF_MAC_ADDRESS_IN;
    let is_vf_path = nic
        .pci_properties
        .as_ref()
        .map(|p| {
            let pci_portion = p.path.split("/net/").next().unwrap_or("");
            pci_portion.contains("virtfn")
        })
        .unwrap_or(false);
    is_sf_or_vf_mac || is_vf_path
}

pub(super) async fn run(
    forge_api: &str,
    root_ca: String,
    machine_interface_id: Option<uuid::Uuid>,
    retry: &registration::DiscoveryRetry,
    tpm_path: &str,
) -> Result<(MachineId, Option<uuid::Uuid>), CarbideClientError> {
    let mut hardware_info = enumerate_hardware()?;
    hardware_info
        .network_interfaces
        .retain(|nic| !is_auxiliary_interface(nic));
    info!("Successfully enumerated hardware");

    // Missing TPM EK material must not be treated as DPU detection. DPUs are
    // identified from platform SMBIOS data, not from TPM availability.
    let is_dpu = !platform::is_host();

    if machine_interface_id.is_none() && !is_dpu {
        return Err(CarbideClientError::GenericError(
            "--machine-interface-id=<uuid> is required for this subcommand.".to_string(),
        ));
    };

    // if we are not on dpu, obtain attestation key (AK) and send it to carbide
    let mut endorsement_key_handle_opt: Option<KeyHandle> = None;
    let mut att_key_handle_opt: Option<KeyHandle> = None;
    let mut tss_ctx_opt: Option<Context> = None;

    // A host with no TPM cannot attest, so gate on actual TPM presence (not just is_dpu) and skip
    // the flow rather than hard failing in create_context_from_path.
    let do_attestation = !is_dpu && tpm::tpm_present(tpm_path);
    if !is_dpu && !do_attestation {
        tracing::warn!(
            tpm_path = ?tpm_path,
            "Host has no TPM device; skipping attestation key setup"
        );
    }

    if do_attestation {
        // set the max auth fail to 256 as a stop gap measure to prevent machines from failing during
        // repeated reingestion cycle
        crate::tpm::set_tpm_max_auth_fail()?;

        // create tss context
        let mut tss_ctx = attest::create_context_from_path(tpm_path)
            .map_err(|e| CarbideClientError::TpmError(format!("Could not create context: {e}")))?;

        // CHANGETO - supply context externally
        hardware_info.tpm_description = attest::get_tpm_description(&mut tss_ctx);

        let result = match attest::create_attest_key_info(&mut tss_ctx) {
            Ok(result) => result,
            Err(e) => {
                if tpm::should_attempt_tpm_recovery_for_attest_key_failure(&*e) {
                    tpm::recover_tpm_and_reboot(tpm_path)?;
                }
                return Err(CarbideClientError::TpmError(format!(
                    "Could not create AttestKeyInfo: {e}"
                )));
            }
        };

        hardware_info.attest_key_info = Some(result.0);
        endorsement_key_handle_opt = Some(result.1);
        att_key_handle_opt = Some(result.2);
        tss_ctx_opt = Some(tss_ctx);
    }

    let (registration_data, attest_key_challenge_opt, interface_id) =
        registration::register_machine(
            forge_api,
            root_ca.clone(),
            machine_interface_id,
            hardware_info,
            false,
            retry.clone(),
            true,
            is_dpu,
            MachineDiscoveryReporter::Scout,
            Some(carbide_version::v!(build_version).to_string()),
        )
        .await?;
    let machine_id = registration_data.machine_id;
    info!(
        %machine_id,
        ?machine_interface_id,
        "successfully discovered machine",
    );

    // If we are not on a DPU and have some post-registration things to do,
    // we do them here.
    if do_attestation {
        // If we have received back an attestation key challenge, this means
        // that Carbide has requested an attestation, so do it!
        //
        // This will perform:
        // -> activate_credential() - to obtain nonce
        // -> get_pcr_quote() - to obtain pcr values
        // -> get_eventlog() - to obtain eventlog
        // -> and, finally, create_quote_request() to create the actual quote
        if let Some(attest_key_challenge) = attest_key_challenge_opt {
            tracing::info!(
                "Sent AttestKeyInfo and received AttestKeyBindChallenge, starting measurements ..."
            );
            tracing::info!(
                credential_blob_bytes = attest_key_challenge.cred_blob.len(),
                encrypted_secret_bytes = attest_key_challenge.encrypted_secret.len(),
                "Received attestation key challenge credential sizes",
            );

            let Some(ek_handle) = endorsement_key_handle_opt else {
                return Err(CarbideClientError::TpmError(
                    "InternalError: EK is None".to_string(),
                ));
            };

            let Some(ak_handle) = att_key_handle_opt else {
                return Err(CarbideClientError::TpmError(
                    "InternalError: AK is None".to_string(),
                ));
            };

            let Some(mut tss_ctx) = tss_ctx_opt else {
                return Err(CarbideClientError::TpmError(
                    "InternalError: TSS_CTX is None".to_string(),
                ));
            };

            // retrieve credential (kind of AuthToken) from the bind_response
            let cred = attest::activate_credential(
                &attest_key_challenge.cred_blob,
                &attest_key_challenge.encrypted_secret,
                &mut tss_ctx,
                &ek_handle,
                &ak_handle,
            )
            .map_err(|e| {
                CarbideClientError::TpmError(format!("Could not activate credential: {e}"))
            })?;

            // obtain signed attestation (a hash of pcr values) and actual pcr values
            let (attest, signature, pcr_values) = attest::get_pcr_quote(&mut tss_ctx, &ak_handle)
                .map_err(|e| {
                CarbideClientError::TpmError(format!("Could not get PCR Quote: {e}"))
            })?;

            tracing::info!("Obtained PCR quote");

            let tpm_eventlog = attest::get_tpm_eventlog();

            // create Quote Request message
            let quote_request = attest::create_quote_request(
                attest,
                signature,
                pcr_values,
                &cred,
                &machine_id,
                &tpm_eventlog,
            )
            .map_err(|e| {
                CarbideClientError::TpmError(format!("Could not create quote request: {e}"))
            })?;
            // send to server
            if !registration::attest_quote(
                forge_api,
                root_ca.clone(),
                false,
                retry.clone(),
                &quote_request,
            )
            .await?
            {
                return Err(RegistrationError::AttestationFailed.into());
            }
        }
    }

    Ok((machine_id, interface_id))
}

#[cfg(test)]
mod tests {
    use ::rpc::machine_discovery::{NetworkInterface, PciDeviceProperties};

    use super::*;

    fn pci(path: &str) -> Option<PciDeviceProperties> {
        Some(PciDeviceProperties {
            vendor: "Mellanox Technologies".to_string(),
            device: "MT42822 BlueField-2 integrated ConnectX-6 Dx network controller".to_string(),
            path: path.to_string(),
            numa_node: 0,
            description: None,
            slot: Some("0000:08:00.0".to_string()),
        })
    }

    fn nic(mac: &str, path: &str) -> NetworkInterface {
        NetworkInterface {
            mac_address: mac.to_string(),
            pci_properties: pci(path),
        }
    }

    // DPU physical functions exposed to the host have real MACs and a direct sysfs
    // path (no "virtfn" segment). They must survive the filter so the inventory model
    // retains evidence that the machine has a DPU.
    #[test]
    fn dpu_pf_is_kept() {
        let iface = nic(
            "a0:88:c2:00:11:22",
            "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/net/enp8s0f0np0",
        );
        assert!(!is_auxiliary_interface(&iface));
    }

    #[test]
    fn second_dpu_pf_port_is_kept() {
        let iface = nic(
            "a0:88:c2:00:11:23",
            "/devices/pci0000:00/0000:00:1c.4/0000:08:00.1/net/enp8s0f1np1",
        );
        assert!(!is_auxiliary_interface(&iface));
    }

    // Regular host NICs (non-DPU Ethernet controllers) must also be kept.
    #[test]
    fn regular_host_nic_is_kept() {
        let iface = nic(
            "b4:96:91:aa:bb:cc",
            "/devices/pci0000:00/0000:00:01.0/0000:01:00.0/net/eth0",
        );
        assert!(!is_auxiliary_interface(&iface));
    }

    // SFs (scalable functions) report the sentinel MAC from udev when they have no real MAC.
    #[test]
    fn sf_with_sentinel_mac_is_filtered() {
        let iface = nic(
            MELLANOX_SF_VF_MAC_ADDRESS_IN,
            "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/net/en_sf0",
        );
        assert!(is_auxiliary_interface(&iface));
    }

    // VFs appear under a "virtfn" sysfs path segment regardless of their MAC.
    #[test]
    fn vf_with_virtfn_path_is_filtered() {
        let iface = nic(
            "a0:88:c2:00:11:30",
            "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/virtfn0/net/eth1",
        );
        assert!(is_auxiliary_interface(&iface));
    }

    // VFs that also carry the sentinel MAC are caught by either predicate.
    #[test]
    fn vf_with_sentinel_mac_and_virtfn_path_is_filtered() {
        let iface = nic(
            MELLANOX_SF_VF_MAC_ADDRESS_IN,
            "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/virtfn3/net/eth3",
        );
        assert!(is_auxiliary_interface(&iface));
    }

    // An interface whose name happens to contain "virtfn" (after /net/) is not a VF
    // and must not be filtered — only a "virtfn" sysfs segment before /net/ indicates a VF.
    #[test]
    fn interface_named_virtfn_with_real_mac_is_kept() {
        let iface = nic(
            "b4:96:91:aa:bb:cc",
            "/devices/pci0000:00/0000:00:01.0/0000:01:00.0/net/virtfn0",
        );
        assert!(!is_auxiliary_interface(&iface));
    }

    // Exercises the unwrap_or(false) in the virtfn path check: absent pci_properties
    // does not cause a false positive when the interface does not also carry
    // the sentinel MAC.
    #[test]
    fn no_pci_properties_with_real_mac_is_kept() {
        let iface = NetworkInterface {
            mac_address: "de:ad:be:ef:00:01".to_string(),
            pci_properties: None,
        };
        assert!(!is_auxiliary_interface(&iface));
    }

    // Verify end-to-end that retain keeps exactly the expected interfaces when a
    // mixed list is filtered — DPU PFs survive, SFs and VFs are removed.
    #[test]
    fn retain_keeps_pfs_removes_sfs_and_vfs() {
        let mut nics = vec![
            nic(
                "a0:88:c2:00:11:22",
                "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/net/enp8s0f0np0",
            ),
            nic(
                MELLANOX_SF_VF_MAC_ADDRESS_IN,
                "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/net/en_sf0",
            ),
            nic(
                MELLANOX_SF_VF_MAC_ADDRESS_IN,
                "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/net/en_sf1",
            ),
            nic(
                "a0:88:c2:00:11:23",
                "/devices/pci0000:00/0000:00:1c.4/0000:08:00.0/virtfn0/net/eth1",
            ),
            nic(
                "a0:88:c2:00:11:24",
                "/devices/pci0000:00/0000:00:1c.4/0000:08:00.1/net/enp8s0f1np1",
            ),
        ];

        nics.retain(|nic| !is_auxiliary_interface(nic));

        assert_eq!(nics.len(), 2);
        assert_eq!(nics[0].mac_address, "a0:88:c2:00:11:22");
        assert_eq!(nics[1].mac_address, "a0:88:c2:00:11:24");
    }
}
