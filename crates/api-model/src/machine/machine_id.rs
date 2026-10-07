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

use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use sha2::{Digest, Sha256};

use crate::hardware_info::HardwareInfo;

/// Generates a temporary Machine ID for a host from the hardware fingerprint
/// of the attached DPU
///
/// Returns `None` if no sufficient data is available
///
/// Panics of the Machine is not a DPU
pub fn host_id_from_dpu_hardware_info(
    hardware_info: &HardwareInfo,
) -> Result<MachineId, MissingHardwareInfo> {
    assert!(hardware_info.is_dpu(), "Method can only be called on a DPU");
    from_hardware_info_with_type(hardware_info, MachineType::PredictedHost)
}

/// Generates a Machine ID from a hardware fingerprint
///
/// Returns `None` if no sufficient data is available
pub fn from_hardware_info_with_type(
    hardware_info: &HardwareInfo,
    machine_type: MachineType,
) -> Result<MachineId, MissingHardwareInfo> {
    let bytes;
    let source;
    let all_serials;

    if let Some(cert) = &hardware_info.tpm_ek_certificate {
        bytes = cert.as_bytes();
        if bytes.is_empty() {
            return Err(MissingHardwareInfo::TPMCertEmpty);
        }
        source = MachineIdSource::Tpm;
    } else if let Some(dmi_data) = &hardware_info.dmi_data {
        // We need at least 1 valid serial number
        if dmi_data.product_serial.is_empty()
            && dmi_data.board_serial.is_empty()
            && dmi_data.chassis_serial.is_empty()
        {
            return Err(MissingHardwareInfo::Serial);
        }

        all_serials = format!(
            "p{}-b{}-c{}",
            dmi_data.product_serial, dmi_data.board_serial, dmi_data.chassis_serial
        );
        bytes = all_serials.as_bytes();
        source = MachineIdSource::ProductBoardChassisSerial;
    } else {
        return Err(MissingHardwareInfo::All);
    }

    let mut hasher = Sha256::new();
    hasher.update(bytes);

    Ok(MachineId::new(
        source,
        hasher.finalize().into(),
        machine_type,
    ))
}

/// Generates a Machine ID from a hardware fingerprint
///
/// Returns `None` if no sufficient data is available
pub fn from_hardware_info(hardware_info: &HardwareInfo) -> Result<MachineId, MissingHardwareInfo> {
    let machine_type = if hardware_info.is_dpu() {
        MachineType::Dpu
    } else {
        MachineType::Host
    };

    from_hardware_info_with_type(hardware_info, machine_type)
}

#[derive(Debug, Copy, Clone, PartialEq, thiserror::Error)]
pub enum MissingHardwareInfo {
    #[error("the TPM certificate has no bytes")]
    TPMCertEmpty,
    #[error("serial number missing (product, board and chassis)")]
    Serial,
    #[error("TPM and DMI data are both missing")]
    All,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases, scenarios, value_scenarios};
    use carbide_uuid::machine::MACHINE_ID_LENGTH;

    use super::*;
    use crate::hardware_info::{DmiData, TpmEkCertificate};

    // Build a `HardwareInfo` carrying only the two fields the ID derivation looks
    // at — an optional TPM certificate and optional DMI serials — leaving every
    // other field defaulted. `tpm` is the certificate bytes (when present) and
    // `serials` is the (product, board, chassis) triple folded into `DmiData`.
    fn info_for_id(tpm: Option<Vec<u8>>, serials: Option<(&str, &str, &str)>) -> HardwareInfo {
        HardwareInfo {
            tpm_ek_certificate: tpm.map(TpmEkCertificate::from),
            dmi_data: serials.map(|(product, board, chassis)| DmiData {
                product_serial: product.to_string(),
                board_serial: board.to_string(),
                chassis_serial: chassis.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    const TEST_DATA_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/hardware_info/test_data");

    lazy_static::lazy_static! {
        /// A valid DNS domain name. Regex is copied from a k8s error message for DNS name validation
        static ref DOMAIN_NAME_RE: regex::Regex = regex::Regex::new(r"^[a-z0-9]([-a-z0-9]*[a-z0-9])?(\\.[a-z0-9]([-a-z0-9]*[a-z0-9])?)*$").unwrap();
    }

    fn test_derive_machine_id(
        fingerprint: &mut HardwareInfo,
        expected_type: MachineType,
        constructor: fn(&HardwareInfo) -> Result<MachineId, MissingHardwareInfo>,
    ) {
        fingerprint.tpm_ek_certificate = Some(TpmEkCertificate::from(vec![1, 2, 3, 4]));

        fn validate_id(
            machine_id: MachineId,
            expected_source: MachineIdSource,
            expected_type: MachineType,
        ) {
            let serialized = machine_id.to_string();
            println!("Serialized: {serialized}");
            assert!(
                DOMAIN_NAME_RE.is_match(&serialized),
                "{serialized} is not a valid DNS name"
            );

            let expected_prefix =
                format!("{}{}", expected_type.id_prefix(), expected_source.id_char());

            assert!(serialized.starts_with(&expected_prefix));
            assert_eq!(serialized.len(), MACHINE_ID_LENGTH);
            let parsed: MachineId = serialized.parse().unwrap();
            assert_eq!(parsed, machine_id);
            assert_eq!(parsed.source(), expected_source);
            assert_eq!(parsed.machine_type(), expected_type);
        }

        let machine_id_tpm = constructor(fingerprint).unwrap();
        validate_id(machine_id_tpm, MachineIdSource::Tpm, expected_type);

        fingerprint.tpm_ek_certificate = None;
        let machine_id_product_serial = constructor(fingerprint).unwrap();
        validate_id(
            machine_id_product_serial,
            MachineIdSource::ProductBoardChassisSerial,
            expected_type,
        );

        fingerprint
            .dmi_data
            .as_mut()
            .unwrap()
            .product_serial
            .clear();
        let machine_id_product_serial = constructor(fingerprint).unwrap();
        validate_id(
            machine_id_product_serial,
            MachineIdSource::ProductBoardChassisSerial,
            expected_type,
        );

        fingerprint.dmi_data.as_mut().unwrap().board_serial.clear();
        let machine_id_product_serial = constructor(fingerprint).unwrap();
        validate_id(
            machine_id_product_serial,
            MachineIdSource::ProductBoardChassisSerial,
            expected_type,
        );

        fingerprint
            .dmi_data
            .as_mut()
            .unwrap()
            .chassis_serial
            .clear();
        assert!(constructor(fingerprint).is_err());
    }

    // Each row loads a hardware-info fixture and derives a Machine ID through one
    // constructor, expecting a given MachineType. `test_derive_machine_id` does all
    // the assertions internally (and panics on mismatch), so each row just expects
    // the run to complete, i.e. `Yields(())`.
    #[test]
    fn derive_machine_id() {
        type Constructor = fn(&HardwareInfo) -> Result<MachineId, MissingHardwareInfo>;

        check_cases(
            [
                Case {
                    scenario: "host machine id from x86 fingerprint",
                    input: (
                        "x86_info.json",
                        MachineType::Host,
                        from_hardware_info as Constructor,
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "dpu machine id from dpu fingerprint",
                    input: (
                        "dpu_info.json",
                        MachineType::Dpu,
                        from_hardware_info as Constructor,
                    ),
                    expect: Yields(()),
                },
                Case {
                    scenario: "predicted-host machine id from dpu fingerprint",
                    input: (
                        "dpu_info.json",
                        MachineType::PredictedHost,
                        host_id_from_dpu_hardware_info as Constructor,
                    ),
                    expect: Yields(()),
                },
            ],
            |(fixture, expected_type, constructor)| -> Result<(), ()> {
                let path = format!("{TEST_DATA_DIR}/{fixture}");
                let data = std::fs::read(path).unwrap();
                let mut fingerprint = serde_json::from_slice::<HardwareInfo>(&data).unwrap();

                test_derive_machine_id(&mut fingerprint, expected_type, constructor);
                Ok(())
            },
        );
    }

    // The error paths of `from_hardware_info_with_type`: a present-but-empty TPM
    // certificate, DMI data with every serial blank, and neither TPM nor DMI
    // present each map to a distinct `MissingHardwareInfo`. A non-empty TPM cert,
    // or DMI data with at least one serial, derives an ID and so `Yields`.
    #[test]
    fn from_hardware_info_with_type_error_paths() {
        scenarios!(
            // Drop the derived ID so a success is `Ok(())`: the `Yields(())` rows
            // assert an ID was derived, while the error rows keep their exact
            // `MissingHardwareInfo` for the `FailsWith` checks.
            run = |info| from_hardware_info_with_type(&info, MachineType::Host).map(drop);
            "present but empty TPM cert is rejected" {
                info_for_id(Some(vec![]), None) => FailsWith(MissingHardwareInfo::TPMCertEmpty),
            }

            "empty TPM cert is rejected even with valid serials present" {
                info_for_id(Some(vec![]), Some(("p1", "b1", "c1"))) => FailsWith(MissingHardwareInfo::TPMCertEmpty),
            }

            "DMI data with all serials blank is rejected" {
                info_for_id(None, Some(("", "", ""))) => FailsWith(MissingHardwareInfo::Serial),
            }

            "neither TPM nor DMI present" {
                info_for_id(None, None) => FailsWith(MissingHardwareInfo::All),
            }

            "non-empty TPM cert derives an ID" {
                info_for_id(Some(vec![1, 2, 3, 4]), None) => Yields(()),
            }

            "product serial alone derives an ID" {
                info_for_id(None, Some(("p1", "", ""))) => Yields(()),
            }

            "board serial alone derives an ID" {
                info_for_id(None, Some(("", "b1", ""))) => Yields(()),
            }

            "chassis serial alone derives an ID" {
                info_for_id(None, Some(("", "", "c1"))) => Yields(()),
            }

            "all three serials present derives an ID" {
                info_for_id(None, Some(("p1", "b1", "c1"))) => Yields(()),
            }
        );
    }

    // Which `MachineIdSource` the derivation selects: a present TPM certificate
    // wins outright, and falls through to the product/board/chassis serial source
    // only when no TPM certificate is present.
    #[test]
    fn from_hardware_info_with_type_selects_source() {
        scenarios!(
            run = |info| {
                from_hardware_info_with_type(&info, MachineType::Host)
                    .map(|id| id.source())
                    .map_err(drop)
            };
            "TPM certificate selects the Tpm source" {
                info_for_id(Some(vec![9]), None) => Yields(MachineIdSource::Tpm),
            }

            "TPM certificate wins even when serials are present" {
                info_for_id(Some(vec![9]), Some(("p1", "b1", "c1"))) => Yields(MachineIdSource::Tpm),
            }

            "serials select the ProductBoardChassisSerial source" {
                info_for_id(None, Some(("p1", "", ""))) => Yields(MachineIdSource::ProductBoardChassisSerial),
            }
        );
    }

    // The requested `MachineType` is carried onto the derived ID unchanged,
    // independent of which hardware source produced the fingerprint.
    #[test]
    fn from_hardware_info_with_type_carries_machine_type() {
        scenarios!(
            run = |(info, ty)| {
                from_hardware_info_with_type(&info, ty)
                    .map(|id| id.machine_type())
                    .map_err(drop)
            };
            "Host onto a TPM-derived ID" {
                (info_for_id(Some(vec![7]), None), MachineType::Host) => Yields(MachineType::Host),
            }

            "Dpu onto a TPM-derived ID" {
                (info_for_id(Some(vec![7]), None), MachineType::Dpu) => Yields(MachineType::Dpu),
            }

            "PredictedHost onto a TPM-derived ID" {
                (info_for_id(Some(vec![7]), None), MachineType::PredictedHost) => Yields(MachineType::PredictedHost),
            }

            "Host onto a serial-derived ID" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    MachineType::Host,
                ) => Yields(MachineType::Host),
            }

            "Dpu onto a serial-derived ID" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    MachineType::Dpu,
                ) => Yields(MachineType::Dpu),
            }

            "PredictedHost onto a serial-derived ID" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    MachineType::PredictedHost,
                ) => Yields(MachineType::PredictedHost),
            }
        );
    }

    // The derivation is a deterministic hash: the same fingerprint and type
    // produce the same ID string, and the string fields differing on type/source
    // change the rendered prefix.
    #[test]
    fn from_hardware_info_with_type_is_deterministic() {
        value_scenarios!(
            run = |(left, right)| {
                let left = from_hardware_info_with_type(&left, MachineType::Host).unwrap();
                let right = from_hardware_info_with_type(&right, MachineType::Host).unwrap();
                left.to_string() == right.to_string()
            };
            "same TPM cert and type yields the same id string" {
                (
                    info_for_id(Some(vec![1, 2, 3]), None),
                    info_for_id(Some(vec![1, 2, 3]), None),
                ) => true,
            }

            "different TPM certs yield different id strings" {
                (
                    info_for_id(Some(vec![1, 2, 3]), None),
                    info_for_id(Some(vec![4, 5, 6]), None),
                ) => false,
            }

            "different serials yield different id strings" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    info_for_id(None, Some(("p2", "b1", "c1"))),
                ) => false,
            }

            "same serials yield the same id string" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                ) => true,
            }
        );
    }

    // The rendered ID string opens with the type+source prefix the constructed
    // fingerprint and requested type imply (see `MachineType::id_prefix` and
    // `MachineIdSource::id_char`).
    #[test]
    fn from_hardware_info_with_type_renders_expected_prefix() {
        scenarios!(
            run = |(info, ty, prefix)| {
                from_hardware_info_with_type(&info, ty)
                    .map(|id| id.to_string().starts_with(prefix))
                    .map_err(drop)
            };
            "host + TPM renders fm100ht" {
                (
                    info_for_id(Some(vec![1]), None),
                    MachineType::Host,
                    "fm100ht",
                ) => Yields(true),
            }

            "dpu + TPM renders fm100dt" {
                (
                    info_for_id(Some(vec![1]), None),
                    MachineType::Dpu,
                    "fm100dt",
                ) => Yields(true),
            }

            "predicted host + serial renders fm100ps" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    MachineType::PredictedHost,
                    "fm100ps",
                ) => Yields(true),
            }

            "host + serial renders fm100hs" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    MachineType::Host,
                    "fm100hs",
                ) => Yields(true),
            }
        );
    }

    // The rendered ID string is exactly `MACHINE_ID_LENGTH` characters regardless
    // of which source or type produced it.
    #[test]
    fn from_hardware_info_with_type_renders_fixed_length() {
        value_scenarios!(
            run = |(info, ty)| {
                from_hardware_info_with_type(&info, ty)
                    .unwrap()
                    .to_string()
                    .len()
            };
            "TPM-derived host id length" {
                (info_for_id(Some(vec![1, 2]), None), MachineType::Host) => MACHINE_ID_LENGTH,
            }

            "serial-derived dpu id length" {
                (
                    info_for_id(None, Some(("p1", "b1", "c1"))),
                    MachineType::Dpu,
                ) => MACHINE_ID_LENGTH,
            }

            "serial-derived predicted-host id length" {
                (
                    info_for_id(None, Some(("", "b1", ""))),
                    MachineType::PredictedHost,
                ) => MACHINE_ID_LENGTH,
            }
        );
    }
}
