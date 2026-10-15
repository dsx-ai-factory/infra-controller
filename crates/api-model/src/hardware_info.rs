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

//! Describes hardware that is discovered by Forge
use std::fmt;
use std::fmt::{Display, Formatter};
use std::net::IpAddr;
use std::str::FromStr;

use base64::prelude::*;
use carbide_utils::arch::CpuArchitecture;
use carbide_uuid::nvlink::NvLinkDomainId;
use mac_address::{MacAddress, MacParseError};
use serde::{Deserialize, Serialize};

use crate::machine::machine_id::MissingHardwareInfo;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareInfo {
    #[serde(default)]
    pub network_interfaces: Vec<NetworkInterface>,
    #[serde(default)]
    pub infiniband_interfaces: Vec<InfinibandInterface>,
    #[serde(default)]
    pub cpu_info: Vec<CpuInfo>,
    #[serde(default)]
    pub block_devices: Vec<BlockDevice>,
    // This should be called machine_arch, but it's serialized directly in/out of a JSONB field in
    // the DB, so renaming it requires a migration or custom Serialize impl.
    pub machine_type: CpuArchitecture,
    #[serde(default)]
    pub nvme_devices: Vec<NvmeDevice>,
    #[serde(default)]
    pub dmi_data: Option<DmiData>,
    pub tpm_ek_certificate: Option<TpmEkCertificate>,
    #[serde(default)]
    pub dpu_info: Option<DpuData>,
    #[serde(default)]
    pub gpus: Vec<Gpu>,
    #[serde(default)]
    pub memory_devices: Vec<MemoryDevice>,
    #[serde(default)]
    pub tpm_description: Option<TpmDescription>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterface {
    #[serde(deserialize_with = "carbide_network::deserialize_mlx_mac")]
    pub mac_address: MacAddress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci_properties: Option<PciDeviceProperties>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfinibandInterface {
    pub guid: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pci_properties: Option<PciDeviceProperties>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuInfo {
    #[serde(default)]
    pub model: String, // CPU model name
    #[serde(default)]
    pub vendor: String, // CPU vendor name
    #[serde(default)]
    pub sockets: u32, // number of sockets
    #[serde(default)]
    pub cores: u32, // cores per socket
    #[serde(default)]
    pub threads: u32, // threads per socket
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockDevice {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub serial: String,
    #[serde(default)]
    pub device_type: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NvmeDevice {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub firmware_rev: String,
    #[serde(default)]
    pub serial: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmiData {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub board_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub board_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bios_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bios_date: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub product_serial: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub board_serial: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chassis_serial: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub product_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sys_vendor: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DpuData {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub part_number: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub part_description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub product_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub factory_mac_address: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub firmware_version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub firmware_date: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub switches: Vec<LldpSwitchData>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LldpSwitchData {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub local_port: String,
    #[serde(
        default,
        deserialize_with = "deserialize_ip_addr_vec_lossy",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub ip_address: Vec<IpAddr>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub remote_port: String,
}

fn deserialize_ip_addr_vec_lossy<'de, D>(deserializer: D) -> Result<Vec<IpAddr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Vec<String>>::deserialize(deserializer)?
        .unwrap_or_default()
        .into_iter()
        .filter_map(|address| address.parse::<IpAddr>().ok())
        .collect())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PciDeviceProperties {
    #[serde(default)]
    pub vendor: String,
    #[serde(default)]
    pub device: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub numa_node: i32,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub slot: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gpu {
    pub name: String,
    pub serial: String,
    pub driver_version: String,
    pub vbios_version: String,
    pub inforom_version: String,
    pub total_memory: String,
    pub frequency: String,
    pub pci_bus_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform_info: Option<GpuPlatformInfo>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuPlatformInfo {
    pub chassis_serial: String,
    pub slot_number: u32,
    pub tray_index: u32,
    pub host_id: u32,
    pub module_id: u32,
    pub fabric_guid: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryDevice {
    pub size_mb: Option<u32>,
    pub mem_type: Option<String>,
}

/// TPM endorsement key certificate
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TpmEkCertificate(Vec<u8>);

impl From<Vec<u8>> for TpmEkCertificate {
    fn from(cert: Vec<u8>) -> Self {
        Self(cert)
    }
}

impl TpmEkCertificate {
    /// Returns the binary content of the certificate
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    /// Converts the certificate into a byte array
    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl Serialize for TpmEkCertificate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&BASE64_STANDARD.encode(self.as_bytes()))
    }
}

impl<'de> Deserialize<'de> for TpmEkCertificate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let str_value = String::deserialize(deserializer)?;
        let bytes = BASE64_STANDARD
            .decode(str_value)
            .map_err(|err| Error::custom(err.to_string()))?;
        Ok(Self(bytes))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TpmDescription {
    pub vendor: String,
    pub firmware_version: String,
    pub tpm_spec: String,
}

#[derive(thiserror::Error, Debug)]
pub enum HardwareInfoError {
    #[error("DPU Info is missing.")]
    MissingDpuInfo,

    #[error("Mac address conversion error: {0}")]
    MacAddressConversionError(#[from] MacParseError),

    #[error("Missing hardware info: {0}")]
    MissingHardwareInfo(#[from] MissingHardwareInfo),
}

impl HardwareInfo {
    /// Returns whether the machine is deemed to be a DPU based on some properties
    pub fn is_dpu(&self) -> bool {
        if self.machine_type != CpuArchitecture::Aarch64 {
            return false;
        }
        self.dmi_data
            .as_ref()
            .is_some_and(|dmi| dmi.board_name.to_lowercase().contains("bluefield"))
    }

    /// This function returns factory_mac_address from dpu_info.
    pub fn factory_mac_address(&self) -> Result<MacAddress, HardwareInfoError> {
        let Some(ref dpu_info) = self.dpu_info else {
            return Err(HardwareInfoError::MissingDpuInfo);
        };

        Ok(MacAddress::from_str(&dpu_info.factory_mac_address)?)
    }

    /// Is this a Dell, Lenovo, etc machine?
    pub fn bmc_vendor(&self) -> bmc_vendor::BMCVendor {
        match self.dmi_data.as_ref() {
            Some(dmi_info) => bmc_vendor::BMCVendor::from_udev_dmi(dmi_info.sys_vendor.as_ref()),
            None => bmc_vendor::BMCVendor::Unknown,
        }
    }

    pub fn all_mac_addresses(&self) -> Vec<MacAddress> {
        self.network_interfaces
            .iter()
            .map(|i| i.mac_address)
            .collect()
    }

    /// Returns true when hardware reports at least one GPU with NVLink platform metadata
    /// and an MNNVL family name on the GPU or, when absent there, in DMI `product_name`.
    pub fn is_mnnvl_capable(&self) -> bool {
        let dmi_product_name = self.dmi_data.as_ref().map(|dmi| dmi.product_name.as_str());
        self.gpus
            .iter()
            .any(|gpu| is_mnnvl_capable_gpu(gpu, dmi_product_name))
    }

    pub fn is_dgx_h100(&self) -> bool {
        self.dmi_data
            .as_ref()
            .is_some_and(|dmi| dmi.sys_vendor == "NVIDIA" && dmi.product_name == "DGXH100")
    }
}

/// Substrings matched against GPU `name` or DMI `product_name` to identify MNNVL-capable hardware.
pub const MNNVL_KNOWN_GPU_NAMES: &[&str] = &["GB200", "GB300", "VR NVL"];

/// Returns true when `name` contains any [`MNNVL_KNOWN_GPU_NAMES`] entry.
pub fn gpu_name_indicates_mnnvl(name: &str) -> bool {
    MNNVL_KNOWN_GPU_NAMES
        .iter()
        .any(|marker| name.contains(marker))
}

/// Returns true when a GPU has `platform_info` and its name matches an MNNVL marker, or when
/// the GPU name does not match and DMI `product_name` does.
pub fn is_mnnvl_capable_gpu(gpu: &Gpu, dmi_product_name: Option<&str>) -> bool {
    if gpu.platform_info.is_none() {
        return false;
    }
    if gpu_name_indicates_mnnvl(&gpu.name) {
        return true;
    }
    dmi_product_name.is_some_and(gpu_name_indicates_mnnvl)
}

/// Builds SQL `LIKE` conditions matching GPU `name` or DMI `product_name` against
/// [`MNNVL_KNOWN_GPU_NAMES`].
pub fn mnnvl_gpu_name_sql_like_conditions() -> String {
    let gpu_name_conditions = MNNVL_KNOWN_GPU_NAMES
        .iter()
        .map(|marker| format!("gpu->>'name' LIKE '%{marker}%'"))
        .collect::<Vec<_>>()
        .join("\n                      OR ");
    let dmi_product_name_conditions = MNNVL_KNOWN_GPU_NAMES
        .iter()
        .map(|marker| {
            format!(
                "mt.topology->'discovery_data'->'Info'->'dmi_data'->>'product_name' LIKE '%{marker}%'"
            )
        })
        .collect::<Vec<_>>()
        .join("\n                      OR ");
    format!("{gpu_name_conditions}\n                      OR {dmi_product_name_conditions}")
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineInventory {
    pub components: Vec<MachineInventorySoftwareComponent>,
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct MachineInventorySoftwareComponent {
    pub name: String,
    pub version: String,
    pub url: String,
}

impl Display for MachineInventorySoftwareComponent {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}:{}", self.url, self.name, self.version)
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineNvLinkInfo {
    pub domain_uuid: NvLinkDomainId,
    /// Chassis serial from the first GPU `GpuPlatformInfo` at discovery (or operator RPC).
    #[serde(default)]
    pub chassis_serial: String,
    pub gpus: Vec<NvLinkGpu>,
}

#[derive(Debug, Default, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct NvLinkGpu {
    pub tray_index: i32,
    pub slot_id: i32,
    pub device_id: i32, // For GB200s, 1-based index of GPU in compute tray.
    pub guid: u64,
}

impl From<libnmxm::nmxm_model::Gpu> for NvLinkGpu {
    fn from(gpu: libnmxm::nmxm_model::Gpu) -> Self {
        NvLinkGpu {
            tray_index: gpu
                .location_info
                .as_ref()
                .and_then(|info| info.tray_index)
                .unwrap_or_default(),
            slot_id: gpu
                .location_info
                .as_ref()
                .and_then(|info| info.slot_id)
                .unwrap_or_default(),
            device_id: gpu.device_id,
            guid: gpu.device_uid,
        }
    }
}

#[cfg(test)]
mod tests {

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    // Build a `HardwareInfo` carrying only the architecture and `DmiData` fields
    // the classification predicates look at, leaving everything else defaulted.
    fn info_with_dmi(machine_type: CpuArchitecture, dmi: DmiData) -> HardwareInfo {
        HardwareInfo {
            machine_type,
            dmi_data: Some(dmi),
            ..Default::default()
        }
    }

    const DPU_INFO_JSON: &[u8] = include_bytes!("hardware_info/test_data/dpu_info.json");
    const DPU_BF3_INFO_JSON: &[u8] = include_bytes!("hardware_info/test_data/dpu_bf3_info.json");
    const X86_INFO_JSON: &[u8] = include_bytes!("hardware_info/test_data/x86_info.json");

    /// Pre-NMX-C rows stored `nvlink_info` without `chassis_serial` (and GPUs carried `nmx_m_id`).
    #[test]
    fn machine_nvlink_info_deserializes_legacy_json_without_chassis_serial() {
        let domain_uuid: NvLinkDomainId = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".parse().unwrap();
        let legacy_json = r#"{
            "domain_uuid": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "gpus": [{
                "nmx_m_id": "legacy-partition-id",
                "tray_index": 0,
                "slot_id": 1,
                "device_id": 1,
                "guid": 12345
            }]
        }"#;

        let info: MachineNvLinkInfo = serde_json::from_str(legacy_json).unwrap();

        assert_eq!(info.domain_uuid, domain_uuid);
        assert_eq!(info.chassis_serial, "");
        assert_eq!(
            info.gpus,
            vec![NvLinkGpu {
                tray_index: 0,
                slot_id: 1,
                device_id: 1,
                guid: 12345,
            }]
        );
    }

    #[test]
    fn test_machine_inventory_json_representation() {
        let inventory = MachineInventory {
            components: vec![
                MachineInventorySoftwareComponent {
                    name: "foo".to_string(),
                    version: "1.0".to_string(),
                    url: "".to_string(),
                },
                MachineInventorySoftwareComponent {
                    name: "bar".to_string(),
                    version: "2.0".to_string(),
                    url: "nvidia.com".to_string(),
                },
            ],
        };
        let json = serde_json::to_string(&inventory).unwrap();
        assert_eq!(
            json,
            r#"{"components":[{"name":"foo","version":"1.0","url":""},{"name":"bar","version":"2.0","url":"nvidia.com"}]}"#
        );
    }

    // Deserialize an LLDP switch entry and project to the management `ip_address`
    // list: invalid entries are dropped lossily and a null list defaults to empty.
    #[test]
    fn lldp_switch_data_management_addresses() {
        scenarios!(
            // serde_json::Error is not PartialEq, so deserialization failure would
            // be Fails; here every row parses, so the error type is irrelevant.
            run = |json| {
                serde_json::from_str::<LldpSwitchData>(json)
                    .map(|switch| switch.ip_address)
                    .map_err(drop)
            };
            "filters invalid management addresses" {
                r#"{
                            "ip_address": [
                                "192.0.2.10",
                                "not-an-ip",
                                "2001:db8::1"
                            ]
                        }"# => Yields(vec![
                    "192.0.2.10".parse::<IpAddr>().unwrap(),
                    "2001:db8::1".parse::<IpAddr>().unwrap(),
                ]),
            }

            "defaults null management addresses to empty" {
                r#"{"ip_address":null}"# => Yields(vec![]),
            }
        );
    }

    #[test]
    fn serialize_blockdev() {
        let dev: BlockDevice = serde_json::from_str("{}").unwrap();
        assert_eq!(
            dev,
            BlockDevice {
                model: "".to_string(),
                revision: "".to_string(),
                serial: "".to_string(),
                device_type: "".to_string(),
            }
        );

        let dev1 = BlockDevice {
            model: "disk".to_string(),
            revision: "rev1".to_string(),
            serial: "001".to_string(),
            device_type: "device_type".to_string(),
        };

        let serialized = serde_json::to_string(&dev1).unwrap();
        assert_eq!(
            serialized,
            r#"{"model":"disk","revision":"rev1","serial":"001","device_type":"device_type"}"#
        );
        assert_eq!(
            serde_json::from_str::<BlockDevice>(&serialized).unwrap(),
            dev1
        );
    }

    #[test]
    fn serialize_cpu_info() {
        let cpu_info: CpuInfo = serde_json::from_str("{}").unwrap();
        assert_eq!(
            cpu_info,
            CpuInfo {
                model: "".to_string(),
                vendor: "".to_string(),
                sockets: 0,
                cores: 0,
                threads: 0,
            }
        );

        let cpu_info1 = CpuInfo {
            model: "m1".to_string(),
            vendor: "v1".to_string(),
            sockets: 2,
            cores: 32,
            threads: 64,
        };

        let serialized = serde_json::to_string(&cpu_info1).unwrap();
        assert_eq!(
            serialized,
            "{\"model\":\"m1\",\"vendor\":\"v1\",\"sockets\":2,\"cores\":32,\"threads\":64}"
        );
        assert_eq!(
            serde_json::from_str::<CpuInfo>(&serialized).unwrap(),
            cpu_info1
        );
    }

    #[test]
    fn serialize_pci_dev_properties() {
        let props: PciDeviceProperties = serde_json::from_str("{}").unwrap();
        assert_eq!(
            props,
            PciDeviceProperties {
                vendor: "".to_string(),
                device: "".to_string(),
                path: "".to_string(),
                numa_node: 0,
                description: None,
                slot: None,
            }
        );

        let props1 = PciDeviceProperties {
            vendor: "v1".to_string(),
            device: "d1".to_string(),
            path: "p1".to_string(),
            numa_node: 3,
            description: Some("desc1".to_string()),
            slot: Some("0000:4b:00.0".to_string()),
        };

        let serialized = serde_json::to_string(&props1).unwrap();
        assert_eq!(
            serialized,
            "{\"vendor\":\"v1\",\"device\":\"d1\",\"path\":\"p1\",\"numa_node\":3,\"description\":\"desc1\",\"slot\":\"0000:4b:00.0\"}"
        );
        assert_eq!(
            serde_json::from_str::<PciDeviceProperties>(&serialized).unwrap(),
            props1
        );
    }

    // Deserialize a HardwareInfo fixture and project to whether it is classified as
    // a DPU: x86 hardware is not, both BlueField fixtures are.
    #[test]
    fn deserialize_info_is_dpu() {
        scenarios!(
            // serde_json::Error is not PartialEq; every fixture parses, so the error
            // type is irrelevant here.
            run = |bytes| {
                serde_json::from_slice::<HardwareInfo>(bytes)
                    .map(|info| info.is_dpu())
                    .map_err(drop)
            };
            "x86 host is not a DPU" {
                X86_INFO_JSON => Yields(false),
            }

            "dpu info is a DPU" {
                DPU_INFO_JSON => Yields(true),
            }

            "bf3 dpu info is a DPU" {
                DPU_BF3_INFO_JSON => Yields(true),
            }
        );
    }

    #[test]
    fn deserialize_dpu_info_decodes_ch_64_mac() {
        let info = serde_json::from_slice::<HardwareInfo>(DPU_INFO_JSON).unwrap();

        // Make sure deserialize_ch_64 works as expected, where
        // the source dpu_info.json file for this has ch:64 as
        // the mac_address.
        assert_eq!(
            info.network_interfaces[1].mac_address.to_string(),
            "00:00:00:00:00:64"
        );
    }

    #[test]
    fn serialize_tpm_ek_certificate() {
        let cert_data = b"This is not really a certificate".to_vec();
        let cert = TpmEkCertificate::from(cert_data.clone());

        let serialized = serde_json::to_string(&cert).unwrap();
        assert_eq!(
            serialized,
            format!("\"{}\"", BASE64_STANDARD.encode(&cert_data))
        );

        // Test also how that the certificate looks right within a Json structure
        #[derive(Serialize)]
        struct OptionalCert {
            cert: Option<TpmEkCertificate>,
        }

        let serialized = serde_json::to_string(&OptionalCert { cert: Some(cert) }).unwrap();
        assert_eq!(
            serialized,
            format!("{{\"cert\":\"{}\"}}", BASE64_STANDARD.encode(&cert_data))
        );
    }

    #[test]
    fn deserialize_tpm_ek_certificate() {
        let cert_data = b"This is not really a certificate".to_vec();
        let encoded = BASE64_STANDARD.encode(&cert_data);

        let json = format!("\"{encoded}\"");
        let deserialized: TpmEkCertificate = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.as_bytes(), &cert_data);

        // Test also how that the certificate looks right within a Json structure
        #[derive(Deserialize)]
        struct OptionalCert {
            cert: Option<TpmEkCertificate>,
        }

        let json = format!("{{\"cert\":\"{encoded}\"}}");
        let deserialized: OptionalCert = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.cert.as_ref().map(|cert| cert.as_bytes()),
            Some(cert_data.as_slice())
        );
    }

    // `is_dpu()` is true only when the architecture is Aarch64 *and* the DMI board
    // name contains "bluefield" (case-insensitively). Both conditions must hold.
    #[test]
    fn hardware_info_is_dpu() {
        value_scenarios!(
            run = |info| info.is_dpu();
            "aarch64 with bluefield board is a DPU" {
                info_with_dmi(
                    CpuArchitecture::Aarch64,
                    DmiData {
                        board_name: "BlueField-3".to_string(),
                        ..Default::default()
                    },
                ) => true,
            }

            "board name match is case-insensitive" {
                info_with_dmi(
                    CpuArchitecture::Aarch64,
                    DmiData {
                        board_name: "MY-BLUEFIELD-CARD".to_string(),
                        ..Default::default()
                    },
                ) => true,
            }

            "bluefield as a lowercase substring still matches" {
                info_with_dmi(
                    CpuArchitecture::Aarch64,
                    DmiData {
                        board_name: "prefix-bluefield-suffix".to_string(),
                        ..Default::default()
                    },
                ) => true,
            }

            "aarch64 without bluefield board is not a DPU" {
                info_with_dmi(
                    CpuArchitecture::Aarch64,
                    DmiData {
                        board_name: "GenericBoard".to_string(),
                        ..Default::default()
                    },
                ) => false,
            }

            "aarch64 with empty board name is not a DPU" {
                info_with_dmi(CpuArchitecture::Aarch64, DmiData::default()) => false,
            }

            "x86_64 with bluefield board is not a DPU" {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        board_name: "BlueField-3".to_string(),
                        ..Default::default()
                    },
                ) => false,
            }

            "unknown arch with bluefield board is not a DPU" {
                info_with_dmi(
                    CpuArchitecture::Unknown,
                    DmiData {
                        board_name: "BlueField-3".to_string(),
                        ..Default::default()
                    },
                ) => false,
            }

            "aarch64 with no dmi data at all is not a DPU" {
                HardwareInfo {
                    machine_type: CpuArchitecture::Aarch64,
                    dmi_data: None,
                    ..Default::default()
                } => false,
            }
        );
    }

    // `factory_mac_address()` requires `dpu_info` and a parseable MAC string within
    // it; its error type is not PartialEq, so failures are asserted as `Fails`.
    #[test]
    fn hardware_info_factory_mac_address() {
        scenarios!(
            // HardwareInfoError is not PartialEq, so drop the error to make the run
            // closure's error type `()`.
            run = |info| info.factory_mac_address().map_err(drop);
            "valid mac in dpu info yields the address" {
                HardwareInfo {
                    dpu_info: Some(DpuData {
                        factory_mac_address: "00:11:22:33:44:55".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                } => Yields(MacAddress::from_str("00:11:22:33:44:55").unwrap()),
            }

            "missing dpu info fails" {
                HardwareInfo::default() => Fails,
            }

            "empty mac string fails to parse" {
                HardwareInfo {
                    dpu_info: Some(DpuData {
                        factory_mac_address: String::new(),
                        ..Default::default()
                    }),
                    ..Default::default()
                } => Fails,
            }

            "malformed mac string fails to parse" {
                HardwareInfo {
                    dpu_info: Some(DpuData {
                        factory_mac_address: "not-a-mac".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                } => Fails,
            }

            "too-short mac string fails to parse" {
                HardwareInfo {
                    dpu_info: Some(DpuData {
                        factory_mac_address: "00:11:22".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                } => Fails,
            }
        );
    }

    // `bmc_vendor()` maps the DMI `sys_vendor` string through `from_udev_dmi`, and
    // falls back to `Unknown` when there is no DMI data at all.
    #[test]
    fn hardware_info_bmc_vendor() {
        value_scenarios!(
            run = |info| info.bmc_vendor();
            "lenovo sys vendor" {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        sys_vendor: "Lenovo".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Lenovo,
            }

            "dell sys vendor" {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        sys_vendor: "Dell Inc.".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Dell,
            }

            "nvidia sys vendor" {
                info_with_dmi(
                    CpuArchitecture::Aarch64,
                    DmiData {
                        sys_vendor: "NVIDIA".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Nvidia,
            }

            "mellanox url maps to nvidia" {
                info_with_dmi(
                    CpuArchitecture::Aarch64,
                    DmiData {
                        sys_vendor: "https://www.mellanox.com".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Nvidia,
            }

            "supermicro sys vendor" {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        sys_vendor: "Supermicro".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Supermicro,
            }

            "hpe sys vendor" {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        sys_vendor: "HPE".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Hpe,
            }

            "unrecognized sys vendor is unknown" {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        sys_vendor: "Acme Corp".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Unknown,
            }

            "case-sensitive: lowercase dell is unknown" {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        sys_vendor: "dell inc.".to_string(),
                        ..Default::default()
                    },
                ) => bmc_vendor::BMCVendor::Unknown,
            }

            "no dmi data is unknown" {
                HardwareInfo::default() => bmc_vendor::BMCVendor::Unknown,
            }
        );
    }

    fn gpu_with_platform_info(name: &str) -> Gpu {
        Gpu {
            name: name.to_string(),
            serial: String::new(),
            driver_version: String::new(),
            vbios_version: String::new(),
            inforom_version: String::new(),
            total_memory: String::new(),
            frequency: String::new(),
            pci_bus_id: String::new(),
            platform_info: Some(GpuPlatformInfo {
                chassis_serial: "chassis-1".to_string(),
                slot_number: 1,
                tray_index: 1,
                host_id: 1,
                module_id: 1,
                fabric_guid: "0x1".to_string(),
            }),
        }
    }

    #[test]
    fn hardware_info_is_mnnvl_capable() {
        value_scenarios!(
            run = |(gpu_name, has_platform_info)| {
                let mut info = HardwareInfo::default();
                if has_platform_info {
                    info.gpus.push(gpu_with_platform_info(gpu_name));
                } else {
                    info.gpus.push(Gpu {
                        name: gpu_name.to_string(),
                        serial: String::new(),
                        driver_version: String::new(),
                        vbios_version: String::new(),
                        inforom_version: String::new(),
                        total_memory: String::new(),
                        frequency: String::new(),
                        pci_bus_id: String::new(),
                        platform_info: None,
                    });
                }
                info.is_mnnvl_capable()
            };
            "GB200 GPU with platform_info is MNNVL capable" {
                ("NVIDIA GB200", true) => true,
            }

            "GB300 GPU with platform_info is MNNVL capable" {
                ("NVIDIA GB300", true) => true,
            }

            "VR GPU with platform_info is MNNVL capable" {
                ("NVIDIA VR NVL72 ES", true) => true,
            }

            "GPU name without platform_info is not MNNVL capable" {
                ("NVIDIA GB200", false) => false,
            }

            "platform_info without MNNVL GPU name is not MNNVL capable" {
                ("NVIDIA H100 PCIe", true) => false,
            }
        );
    }

    #[test]
    fn hardware_info_is_mnnvl_capable_falls_back_to_dmi_product_name() {
        let mut info = info_with_dmi(
            CpuArchitecture::Aarch64,
            DmiData {
                product_name: "GB200 NVL".to_string(),
                ..Default::default()
            },
        );
        info.gpus.push(gpu_with_platform_info("NVIDIA H100 PCIe"));
        assert!(info.is_mnnvl_capable());
    }

    #[test]
    fn mnnvl_gpu_name_sql_like_conditions_match_markers() {
        let sql = mnnvl_gpu_name_sql_like_conditions();
        for marker in MNNVL_KNOWN_GPU_NAMES {
            assert!(
                sql.contains(&format!("gpu->>'name' LIKE '%{marker}%'")),
                "expected SQL filter to include GPU name marker {marker:?}, got: {sql}"
            );
            assert!(
                sql.contains(&format!("dmi_data'->>'product_name' LIKE '%{marker}%'")),
                "expected SQL filter to include DMI product_name marker {marker:?}, got: {sql}"
            );
        }
    }

    // `is_dgx_h100()` requires both sys_vendor == "NVIDIA" and product_name == "DGXH100".
    #[test]
    fn hardware_info_is_dgx_h100() {
        value_scenarios!(
            run = |(sys_vendor, product_name)| {
                info_with_dmi(
                    CpuArchitecture::X86_64,
                    DmiData {
                        sys_vendor: sys_vendor.to_string(),
                        product_name: product_name.to_string(),
                        ..Default::default()
                    },
                )
                .is_dgx_h100()
            };
            "nvidia vendor and dgxh100 product" {
                ("NVIDIA", "DGXH100") => true,
            }

            "wrong product is not a dgx h100" {
                ("NVIDIA", "DGXH200") => false,
            }

            "wrong vendor is not a dgx h100" {
                ("Supermicro", "DGXH100") => false,
            }

            "both empty is not a dgx h100" {
                ("", "") => false,
            }

            "product as substring is rejected (exact match required)" {
                ("NVIDIA", "DGXH100-rev2") => false,
            }
        );
    }

    // `all_mac_addresses()` projects each network interface's MAC, in order.
    #[test]
    fn hardware_info_all_mac_addresses() {
        let iface = |mac: &str| NetworkInterface {
            mac_address: MacAddress::from_str(mac).unwrap(),
            pci_properties: None,
        };
        value_scenarios!(
            run = |network_interfaces| {
                HardwareInfo {
                    network_interfaces,
                    ..Default::default()
                }
                .all_mac_addresses()
            };
            "no interfaces yields an empty list" {
                vec![] => vec![],
            }

            "one interface yields its mac" {
                vec![iface("00:11:22:33:44:55")] => vec![MacAddress::from_str("00:11:22:33:44:55").unwrap()],
            }

            "several interfaces preserve order" {
                vec![iface("00:11:22:33:44:55"), iface("aa:bb:cc:dd:ee:ff")] => vec![
                    MacAddress::from_str("00:11:22:33:44:55").unwrap(),
                    MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap(),
                ],
            }
        );
    }

    // `Display` for a software component renders as `url/name:version`, including
    // the empty-url case.
    #[test]
    fn machine_inventory_component_display() {
        value_scenarios!(
            run = |(url, name, version)| {
                MachineInventorySoftwareComponent {
                    name: name.to_string(),
                    version: version.to_string(),
                    url: url.to_string(),
                }
                .to_string()
            };
            "all fields populated" {
                ("nvidia.com", "bar", "2.0") => "nvidia.com/bar:2.0".to_string(),
            }

            "empty url keeps the leading slash" {
                ("", "foo", "1.0") => "/foo:1.0".to_string(),
            }

            "all fields empty" {
                ("", "", "") => "/:".to_string(),
            }
        );
    }

    // `TpmEkCertificate` round-trips its bytes through `From`, `as_bytes`, and
    // `into_bytes`, including the empty-certificate case.
    #[test]
    fn tpm_ek_certificate_byte_accessors() {
        value_scenarios!(
            run = |bytes: Vec<u8>| {
                let cert = TpmEkCertificate::from(bytes.clone());
                assert_eq!(cert.as_bytes(), bytes.as_slice());
                cert.into_bytes()
            };
            "non-empty certificate round-trips" {
                vec![1u8, 2, 3, 4] => vec![1u8, 2, 3, 4],
            }

            "empty certificate round-trips" {
                vec![] => vec![],
            }
        );
    }

    // `NvLinkGpu::from` pulls tray/slot from `location_info` (defaulting to 0 when
    // absent or unset) and copies device_id / device_uid straight across.
    #[test]
    fn nvlink_gpu_from_nmxm_gpu() {
        value_scenarios!(
            run = |json| {
                let gpu = serde_json::from_str::<libnmxm::nmxm_model::Gpu>(json).unwrap();
                NvLinkGpu::from(gpu)
            };
            "full location info is carried through" {
                r#"{
                            "LocationInfo": {"TrayIndex": 3, "SlotID": 7},
                            "DeviceUID": 42,
                            "DeviceID": 5,
                            "DevicePcieID": 0,
                            "SystemUID": 0,
                            "VendorID": 0,
                            "ALIDList": []
                        }"# => NvLinkGpu {
                    tray_index: 3,
                    slot_id: 7,
                    device_id: 5,
                    guid: 42,
                },
            }

            "absent location info defaults tray and slot to zero" {
                r#"{
                            "DeviceUID": 99,
                            "DeviceID": 1,
                            "DevicePcieID": 0,
                            "SystemUID": 0,
                            "VendorID": 0,
                            "ALIDList": []
                        }"# => NvLinkGpu {
                    tray_index: 0,
                    slot_id: 0,
                    device_id: 1,
                    guid: 99,
                },
            }

            "partial location info defaults the missing field" {
                r#"{
                            "LocationInfo": {"TrayIndex": 2},
                            "DeviceUID": 0,
                            "DeviceID": 0,
                            "DevicePcieID": 0,
                            "SystemUID": 0,
                            "VendorID": 0,
                            "ALIDList": []
                        }"# => NvLinkGpu {
                    tray_index: 2,
                    slot_id: 0,
                    device_id: 0,
                    guid: 0,
                },
            }
        );
    }
}
