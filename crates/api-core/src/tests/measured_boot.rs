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

pub(in crate::tests) mod tests {
    use std::str::FromStr;

    use carbide_uuid::machine::MachineId;
    use common::api_fixtures::tpm_attestation::{
        AK_NAME, AK_NAME_SERIALIZED, AK_PUB_SERIALIZED, AK_PUB_SERIALIZED_2, ATTEST_SERIALIZED,
        ATTEST_SERIALIZED_2, ATTEST_SERIALIZED_SHORT, CRED_SERIALIZED, EK_CERT_SERIALIZED,
        EK_PUB_SERIALIZED, PCR_VALUES, PCR_VALUES_SHORT, SESSION_KEY, SIGNATURE_SERIALIZED,
        SIGNATURE_SERIALIZED_2, SIGNATURE_SERIALIZED_INVALID,
    };
    use common::api_fixtures::{
        TestEnvOverrides, create_test_env, create_test_env_with_overrides, get_config,
    };
    use model::hardware_info::{HardwareInfo, TpmEkCertificate};
    use rpc::forge::AttestQuoteRequest;
    use rpc::forge::forge_server::Forge;
    use rpc::machine_discovery::AttestKeyInfo;
    use rpc::{DiscoveryData, DiscoveryInfo, MachineDiscoveryInfo};
    use tonic::Code;

    use crate::attestation::cli_make_cred;
    use crate::attestation::linux_build::do_compare_pub_key_against_cert;
    use crate::tests::common;
    use crate::tests::common::api_fixtures::dpu::create_dpu_machine;
    use crate::tests::common::api_fixtures::host::host_discover_dhcp;

    #[crate::sqlx_test]
    async fn test_discover_machine_key_ek_unmarshall_returns_error(pool: sqlx::PgPool) {
        let mut config = get_config();
        config.attestation_enabled = true;
        let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

        let host_config = env.managed_host_config();
        let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

        let host_machine_interface_id =
            host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

        // ek_pub is corrupted on purpose
        let ek_pub_corrupted = [
            0, 44, 204, 141, 70, 165, 215, 36, 253, 82, 215, 110, 6, 82, 11, 100, 242, 161, 218,
            27, 51, 20, 105, 170, 0, 6, 0, 128, 0, 67, 0, 16, 8, 0, 0, 0, 0, 0, 1, 0, 161, 6, 212,
            135, 171, 109, 37, 41, 140, 162, 195, 208, 28, 179, 230, 10, 240, 68, 50, 63, 156, 87,
            145, 116, 187, 226, 155, 98, 39, 45, 151, 92, 237, 12, 163, 23, 222, 219, 192, 54, 202,
            86, 88, 126, 33, 221, 129, 226, 234, 88, 157, 181, 78, 232, 181, 248, 75, 150, 214, 90,
            154, 231, 177, 168, 97, 214, 69, 237, 147, 77, 89, 191, 188, 209, 36, 87, 92, 145, 236,
            231, 206, 100, 177, 159, 40, 65, 177, 177, 91, 116, 173, 114, 128, 82, 70, 2, 225, 214,
            11, 241, 253, 134, 12, 160, 205, 34, 148, 77, 77, 114, 165, 237, 25, 36, 65, 183, 193,
            35, 138, 64, 183, 59, 240, 142, 126, 67, 81, 15, 120, 9, 13, 94, 220, 12, 99, 225, 130,
            91, 81, 223, 183, 122, 0, 224, 243, 84, 239, 188, 147, 44, 149, 78, 90, 246, 180, 255,
            71, 44, 4, 20, 114, 46, 234, 213, 115, 123, 21, 3, 29, 161, 52, 203, 172, 186, 8, 84,
            2, 127, 252, 152, 219, 56, 144, 177, 9, 125, 234, 93, 78, 118, 126, 101, 38, 59, 174,
            103, 249, 86, 7, 2, 97, 246, 117, 79, 1, 222, 12, 64, 167, 15, 41, 67, 140, 66, 124,
            100, 236, 245, 2, 227, 26, 68, 132, 104, 156, 96, 53, 225, 169, 180, 84, 182, 67, 143,
            162, 63, 156, 13, 6, 118, 37, 35, 105, 163, 200, 56, 233, 254, 7, 165, 40, 33, 189,
            226, 206, 145,
        ];

        let mut discovery_info = DiscoveryInfo::try_from(HardwareInfo::from(&host_config)).unwrap();

        discovery_info.attest_key_info = Some(AttestKeyInfo {
            ek_pub: ek_pub_corrupted.to_vec(),
            ak_pub: AK_PUB_SERIALIZED.to_vec(),
            ak_name: AK_NAME_SERIALIZED.to_vec(),
        });

        let response = env
            .api
            .discover_machine(Request::new(MachineDiscoveryInfo {
                machine_interface_id: Some(host_machine_interface_id),
                discovery_data: Some(DiscoveryData::Info(discovery_info)),
                create_machine: true,
                ..Default::default()
            }))
            .await;

        match response {
            Ok(_) => panic!("Unexpected OK value returned"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest bind key error: TPM EK is not in RSA format"
                );
            }
        }
    }

    #[crate::sqlx_test]
    async fn test_discover_machine_key_pub_key_does_not_match_cert_returns_error(
        pool: sqlx::PgPool,
    ) {
        let mut config = get_config();
        config.attestation_enabled = true;
        let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

        let host_config = env.managed_host_config();
        let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

        let host_machine_interface_id =
            host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

        // ek_pub is corrupted on purpose
        let ek_pub_different = [
            0, 1, 0, 11, 0, 3, 0, 178, 0, 32, 131, 113, 151, 103, 68, 132, 179, 248, 26, 144, 204,
            141, 70, 165, 215, 36, 253, 82, 215, 110, 6, 82, 11, 100, 242, 161, 218, 27, 51, 20,
            105, 170, 0, 6, 0, 128, 0, 67, 0, 16, 8, 0, 0, 1, 0, 1, 1, 0, 135, 228, 64, 171, 148,
            185, 68, 17, 77, 214, 165, 176, 125, 193, 241, 181, 132, 157, 253, 181, 98, 160, 38,
            214, 165, 113, 149, 222, 176, 36, 56, 123, 88, 22, 152, 21, 177, 124, 128, 76, 104,
            248, 33, 175, 221, 182, 76, 17, 65, 47, 221, 100, 177, 122, 55, 129, 126, 189, 43, 225,
            152, 93, 47, 196, 77, 122, 180, 51, 80, 38, 54, 106, 87, 47, 155, 185, 110, 149, 85,
            161, 139, 145, 103, 233, 206, 198, 212, 57, 42, 142, 96, 179, 179, 139, 162, 199, 45,
            99, 52, 14, 181, 111, 96, 211, 166, 107, 22, 90, 149, 208, 240, 145, 94, 9, 186, 164,
            93, 117, 223, 216, 196, 142, 247, 223, 214, 167, 219, 141, 165, 253, 78, 192, 69, 1,
            108, 104, 99, 6, 104, 162, 63, 6, 190, 239, 217, 76, 86, 229, 135, 140, 145, 6, 216,
            125, 16, 62, 233, 62, 224, 148, 40, 63, 86, 181, 247, 73, 3, 37, 202, 123, 19, 240,
            125, 131, 116, 59, 113, 18, 31, 209, 21, 99, 216, 235, 105, 21, 23, 8, 69, 254, 114,
            63, 57, 134, 83, 242, 153, 24, 198, 161, 135, 181, 251, 14, 145, 178, 52, 25, 14, 39,
            254, 248, 180, 67, 208, 143, 134, 225, 142, 127, 207, 205, 150, 250, 174, 141, 195,
            112, 114, 249, 229, 99, 103, 153, 190, 199, 253, 117, 29, 147, 19, 157, 8, 7, 158, 213,
            231, 204, 186, 242, 24, 202, 209, 210, 121, 23,
        ];

        let mut discovery_info = DiscoveryInfo::try_from(HardwareInfo::from(&host_config)).unwrap();

        discovery_info.attest_key_info = Some(AttestKeyInfo {
            ek_pub: ek_pub_different.to_vec(),
            ak_pub: AK_PUB_SERIALIZED.to_vec(),
            ak_name: AK_NAME_SERIALIZED.to_vec(),
        });

        let response = env
            .api
            .discover_machine(Request::new(MachineDiscoveryInfo {
                machine_interface_id: Some(host_machine_interface_id),
                discovery_data: Some(DiscoveryData::Info(discovery_info)),
                create_machine: true,
                ..Default::default()
            }))
            .await;

        match response {
            Ok(_) => panic!("Unexpected OK value returned"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest bind key error: TPM EK is not in RSA format"
                );
            }
        }
    }
    //
    // TODO: test_bind_attest_key_get_insert_pubkey_fails_returns_error - not clear how to simulate db failure atm

    #[crate::sqlx_test(fixtures("create_cred_pub_key"))]
    async fn test_verify_quote_no_secret_in_db_returns_error(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut cred_serialized_invalid = CRED_SERIALIZED;
        cred_serialized_invalid[3] = 8; // corrupt the db key

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: ATTEST_SERIALIZED.to_vec(),
            signature: SIGNATURE_SERIALIZED.to_vec(),
            credential: Vec::from(cred_serialized_invalid),
            pcr_values: PCR_VALUES.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest quote error: could not form SQL query to fetch AK pub"
                );
            }
        }
    }

    #[crate::sqlx_test(fixtures("create_cred_pub_key_invalid"))]
    async fn test_verify_quote_invalid_ak_pub_in_db_returns_error(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: ATTEST_SERIALIZED.to_vec(),
            signature: SIGNATURE_SERIALIZED.to_vec(),
            credential: Vec::from(CRED_SERIALIZED),
            pcr_values: PCR_VALUES.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest quote error: could not unmarshal AK pub: response code not recognized"
                );
            }
        }
    }

    #[crate::sqlx_test(fixtures("create_cred_pub_key"))]
    async fn test_verify_quote_cannot_unmarshall_attest_returns_error(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut attest_invalid = ATTEST_SERIALIZED;
        attest_invalid[5] = 54;

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: attest_invalid.to_vec(),
            signature: SIGNATURE_SERIALIZED.to_vec(),
            credential: Vec::from(CRED_SERIALIZED),
            pcr_values: PCR_VALUES.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest quote error: could not unmarshall attest struct: not currently used"
                );
            }
        }
    }

    #[crate::sqlx_test(fixtures("create_cred_pub_key"))]
    async fn test_verify_quote_cannot_unmarshall_signature_returns_error(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut signature_invalid = SIGNATURE_SERIALIZED;
        signature_invalid[5] = 15;

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: ATTEST_SERIALIZED.to_vec(),
            signature: signature_invalid.to_vec(),
            credential: Vec::from(CRED_SERIALIZED),
            pcr_values: PCR_VALUES.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest quote error: could not unmarshall signature struct: response code not recognized"
                );
            }
        }
    }

    #[crate::sqlx_test(fixtures("create_cred_pub_key"))]
    async fn test_verify_quote_cannot_verify_signature_fails_returns_error(pool: sqlx::PgPool) {
        use tss_esapi::structures::Signature;
        use tss_esapi::structures::Signature::{RsaPss, RsaSsa};
        use tss_esapi::traits::{Marshall, UnMarshall};

        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let signature = Signature::unmarshall(&SIGNATURE_SERIALIZED).unwrap();

        let rsa_signature = match signature {
            RsaPss(rsa_signature) => rsa_signature,
            _ => panic!("Failed: Unexepected signarue type in test"),
        };

        let signature_invalid = RsaSsa(rsa_signature);

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: ATTEST_SERIALIZED.to_vec(),
            signature: Signature::marshall(&signature_invalid).unwrap(),
            credential: Vec::from(CRED_SERIALIZED),
            pcr_values: PCR_VALUES.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(e.message(), "attest quote error: unknown signature type");
            }
        }
    }

    // test_verify_quote_cannot_verify_pcr_hash_fails_returns_error - currently impossible to do since attest fields are private

    #[crate::sqlx_test(fixtures("create_cred_pub_key"))]
    async fn test_verify_quote_signature_mismatch_returns_false(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: ATTEST_SERIALIZED.to_vec(),
            signature: SIGNATURE_SERIALIZED_INVALID.to_vec(), // invalid signature
            credential: Vec::from(CRED_SERIALIZED),
            pcr_values: PCR_VALUES.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest quote error: PCR signature invalid (see logs for full event log)"
                );
            }
        }
    }

    #[crate::sqlx_test(fixtures("create_cred_pub_key"))]
    async fn test_verify_quote_pcr_hash_mismatch_returns_false(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut pcr_values_invalid = PCR_VALUES;

        pcr_values_invalid[0][3] = 88; // corrupt the pcr values

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: ATTEST_SERIALIZED.to_vec(),
            signature: SIGNATURE_SERIALIZED.to_vec(),
            credential: Vec::from(CRED_SERIALIZED),
            pcr_values: pcr_values_invalid.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest quote error: PCR hash does not match (see logs for full event log)"
                );
            }
        }
    }

    #[crate::sqlx_test(fixtures("create_cred_pub_key"))]
    async fn test_verify_quote_signature_and_pcr_hash_mismatch_returns_false(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut pcr_values_invalid = PCR_VALUES;

        pcr_values_invalid[0][3] = 88; // corrupt the pcr values

        let request = tonic::Request::new(AttestQuoteRequest {
            attestation: ATTEST_SERIALIZED.to_vec(),
            signature: SIGNATURE_SERIALIZED_INVALID.to_vec(), // invalid signature
            credential: Vec::from(CRED_SERIALIZED),
            pcr_values: pcr_values_invalid.iter().map(|x| x.to_vec()).collect(),
            machine_id: Some(host_id),
            event_log: None,
        });

        let res = env.api.attest_quote(request).await;

        match res {
            Ok(..) => panic!("Failed: should have returned an error"),
            Err(e) => {
                assert_eq!(e.code(), Code::Internal);
                assert_eq!(
                    e.message(),
                    "attest quote error: PCR signature invalid and PCR hash mismatch (see logs for full event log)"
                );
            }
        }
    }

    // carbide/api/attestation.rs tests

    use rsa::RsaPublicKey;
    use tonic::Request;
    use tss_esapi::structures::Signature::RsaPss;
    use tss_esapi::structures::{EccPoint, Public, Signature};
    use tss_esapi::traits::{Marshall, UnMarshall};

    use crate::CarbideError::AttestBindKeyError;
    use crate::attestation::{verify_pcr_hash, verify_signature};

    #[test]
    fn test_compare_pub_key_against_cert_corrupt_ek_pub_returns_error() {
        let ek_pub = [
            0, 44, 204, 141, 70, 165, 215, 36, 253, 82, 215, 110, 6, 82, 11, 100, 242, 161, 218,
            27, 51, 20, 105, 170, 0, 6, 0, 128, 0, 67, 0, 16, 8, 0, 0, 0, 0, 0, 1, 0, 161, 6, 212,
            135, 171, 109, 37, 41, 140, 162, 195, 208, 28, 179, 230, 10, 240, 68, 50, 63, 156, 87,
            145, 116, 187, 226, 155, 98, 39, 45, 151, 92, 237, 12, 163, 23, 222, 219, 192, 54, 202,
            86, 88, 126, 33, 221, 129, 226, 234, 88, 157, 181, 78, 232, 181, 248, 75, 150, 214, 90,
            154, 231, 177, 168, 97, 214, 69, 237, 147, 77, 89, 191, 188, 209, 36, 87, 92, 145, 236,
            231, 206, 100, 177, 159, 40, 65, 177, 177, 91, 116, 173, 114, 128, 82, 70, 2, 225, 214,
            11, 241, 253, 134, 12, 160, 205, 34, 148, 77, 77, 114, 165, 237, 25, 36, 65, 183, 193,
            35, 138, 64, 183, 59, 240, 142, 126, 67, 81, 15, 120, 9, 13, 94, 220, 12, 99, 225, 130,
            91, 81, 223, 183, 122, 0, 224, 243, 84, 239, 188, 147, 44, 149, 78, 90, 246, 180, 255,
            71, 44, 4, 20, 114, 46, 234, 213, 115, 123, 21, 3, 29, 161, 52, 203, 172, 186, 8, 84,
            2, 127, 252, 152, 219, 56, 144, 177, 9, 125, 234, 93, 78, 118, 126, 101, 38, 59, 174,
            103, 249, 86, 7, 2, 97, 246, 117, 79, 1, 222, 12, 64, 167, 15, 41, 67, 140, 66, 124,
            100, 236, 245, 2, 227, 26, 68, 132, 104, 156, 96, 53, 225, 169, 180, 84, 182, 67, 143,
            162, 63, 156, 13, 6, 118, 37, 35, 105, 163, 200, 56, 233, 254, 7, 165, 40, 33, 189,
            226, 206, 145,
        ];

        let res = do_compare_pub_key_against_cert(
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            &ek_pub,
        );

        match res {
            Ok(..) => {
                panic!("Failed: should have returned error");
            }
            Err(e) => match e {
                AttestBindKeyError(d) => {
                    assert_eq!(d, "could not unmarshall EK: response code not recognized")
                }
                _another_error => panic!("Failed: incorrect error type: {_another_error:?}"),
            },
        }
    }

    #[test]
    fn test_compare_pub_key_against_cert_ek_pub_not_rsa_returns_error() {
        let ek_pub = get_ext_ecc_pub();

        let ek_pub_serialized = ek_pub.marshall().unwrap();

        let res = do_compare_pub_key_against_cert(
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            &ek_pub_serialized,
        );

        match res {
            Ok(..) => {
                panic!("Failed: should have returned error");
            }
            Err(e) => match e {
                AttestBindKeyError(d) => {
                    assert_eq!(d, "EK pub is not in RSA format")
                }
                _another_error => panic!("Failed: incorrect error type: {_another_error:?}"),
            },
        }
    }

    #[test]
    fn test_compare_pub_key_against_cert_invalid_modulus_returns_error() {
        use tss_esapi::structures::Public::Rsa;
        use tss_esapi::structures::PublicKeyRsa;

        let ek_pub = get_ext_rsa_pub();

        let (object_attributes, name_hashing_algo, auth_policy, params) = match ek_pub {
            Rsa {
                object_attributes,
                name_hashing_algorithm,
                auth_policy,
                parameters,
                ..
            } => (
                object_attributes,
                name_hashing_algorithm,
                auth_policy,
                parameters,
            ),
            _ => panic!("Incorrect key type"),
        };

        let ek_pub_copy = Rsa {
            object_attributes,
            name_hashing_algorithm: name_hashing_algo,
            auth_policy,
            parameters: params,
            unique: PublicKeyRsa::try_from([0, 34, 56].to_vec()).unwrap(), // injecting bad value
        };

        let ek_pub_serialized = ek_pub_copy.marshall().unwrap();

        let res = do_compare_pub_key_against_cert(
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            &ek_pub_serialized,
        );

        match res {
            Ok(..) => {
                panic!("Failed: should have returned error");
            }
            Err(e) => match e {
                AttestBindKeyError(d) => {
                    assert_eq!(
                        d,
                        "could not create RsaPublicKey from TPM's EK pub: invalid modulus"
                    )
                }
                _another_error => panic!("Failed: incorrect error type: {_another_error:?}"),
            },
        }
    }

    #[test]
    fn test_compare_pub_key_against_cert_invalid_cert_returns_error() {
        // need to corrupt certificate
        let mut ek_cert_corrupted = EK_CERT_SERIALIZED;

        ek_cert_corrupted[56] = 20;
        ek_cert_corrupted[543] = 92;

        let res = do_compare_pub_key_against_cert(
            &TpmEkCertificate::from(ek_cert_corrupted.to_vec()),
            EK_PUB_SERIALIZED.as_ref(),
        );

        match res {
            Ok(..) => {
                panic!("Failed: should have returned error");
            }
            Err(e) => match e {
                AttestBindKeyError(d) => {
                    assert_eq!(
                        d,
                        "could not unmarshall EK cert: Parsing Error: NomError(Eof)"
                    )
                }
                _another_error => panic!("Failed: incorrect error type: {_another_error:?}"),
            },
        }
    }

    #[test]
    fn test_compare_pub_key_against_cert_different_cert_returns_false() {
        let res = do_compare_pub_key_against_cert(
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            AK_PUB_SERIALIZED.as_ref(), // using AK instad of EK on purpose to make it fail
        );

        match res {
            Ok(val) => assert!(!val.0),
            Err(..) => panic!("Failed: should have returned error"),
        }
    }

    #[test]
    fn test_compare_pub_key_against_cert_success_returns_true() {
        let res = do_compare_pub_key_against_cert(
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            EK_PUB_SERIALIZED.as_ref(),
        );

        match res {
            Ok(val) => assert!(val.0),
            Err(..) => panic!("Failed: should have returned error"),
        }
    }

    #[test]
    fn test_cli_make_cred_success_returns_cred_and_secret() {
        let ek_pub = get_ext_rsa_pub();

        let unique = match ek_pub {
            Public::Rsa { unique, .. } => unique,
            _ => {
                panic!("EK Pub is not in RSA format");
            }
        };

        // now, we construct the actual public key from the modulus and exponent
        let modulus = rsa::BigUint::from_bytes_be(unique.value());
        let exponent = rsa::BigUint::from(65537u32);

        let pub_key_ek =
            RsaPublicKey::new(modulus, exponent).expect("ERROR: could not create RsaPublicKey");

        let res = cli_make_cred(pub_key_ek, AK_NAME.to_vec().as_ref(), &SESSION_KEY);

        match res {
            Ok((v1, v2)) => {
                assert_eq!(v1.len(), 50);
                assert_eq!(v2.len(), 256);
            }
            Err(_) => panic!("Failed: should have returned Ok"),
        }
    }

    use crate::CarbideError::AttestQuoteError;

    /*const ATTEST_SERIALIZED: [u8; 129] = [
        255, 84, 67, 71, 128, 24, 0, 34, 0, 11, 131, 45, 55, 82, 140, 235, 232, 215, 180, 133, 115,
        220, 203, 79, 13, 153, 10, 168, 230, 203, 59, 199, 64, 128, 150, 218, 164, 66, 52, 72, 227,
        197, 0, 16, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
        0, 0, 0, 1, 108, 44, 167, 86, 0, 0, 0, 63, 0, 0, 0, 0, 1, 0, 7, 0, 2, 0, 2, 0, 0, 0, 0, 0,
        1, 0, 11, 3, 255, 15, 0, 0, 32, 69, 159, 141, 33, 201, 110, 233, 102, 224, 171, 155, 67,
        115, 214, 128, 145, 55, 215, 242, 130, 251, 89, 92, 188, 251, 113, 20, 127, 251, 198, 74,
        188,
    ];
    const SIGNATURE_SERIALIZED: [u8; 262] = [
        0, 22, 0, 11, 1, 0, 171, 33, 190, 68, 89, 71, 190, 125, 172, 120, 100, 63, 101, 236, 168,
        171, 90, 209, 161, 89, 156, 193, 87, 74, 57, 203, 179, 84, 240, 213, 128, 158, 39, 132,
        212, 18, 25, 113, 53, 71, 255, 68, 15, 213, 40, 25, 118, 180, 156, 67, 63, 153, 150, 17,
        64, 74, 68, 242, 195, 11, 53, 92, 103, 222, 109, 66, 104, 115, 86, 243, 49, 31, 229, 160,
        71, 213, 45, 119, 126, 183, 106, 235, 224, 63, 132, 119, 208, 158, 236, 201, 147, 200, 70,
        166, 175, 20, 239, 145, 228, 215, 233, 184, 111, 54, 134, 133, 28, 171, 118, 94, 99, 43,
        194, 122, 19, 20, 107, 214, 203, 72, 16, 71, 16, 58, 116, 98, 64, 156, 197, 241, 184, 76,
        197, 198, 79, 15, 90, 157, 18, 234, 35, 241, 144, 136, 72, 69, 197, 232, 251, 251, 181,
        190, 64, 191, 130, 160, 76, 253, 179, 172, 12, 7, 213, 245, 140, 109, 97, 222, 164, 233,
        189, 166, 219, 218, 243, 72, 95, 124, 184, 71, 152, 109, 101, 47, 119, 117, 141, 1, 1, 108,
        148, 28, 69, 217, 177, 187, 153, 119, 216, 76, 44, 102, 249, 94, 56, 93, 108, 7, 229, 79,
        75, 47, 82, 82, 159, 202, 238, 240, 176, 99, 123, 61, 186, 28, 149, 166, 124, 62, 176, 84,
        197, 231, 222, 116, 40, 39, 68, 228, 210, 208, 152, 50, 240, 53, 223, 9, 213, 255, 190,
        231, 214, 11, 126, 155, 19, 190,
    ];
    const AK_PUB_SERIALIZED: [u8; 280] = [
        0, 1, 0, 11, 0, 5, 0, 114, 0, 0, 0, 16, 0, 22, 0, 11, 8, 0, 0, 0, 0, 0, 1, 0, 183, 98, 82,
        64, 227, 242, 101, 235, 94, 190, 115, 98, 139, 145, 176, 117, 64, 80, 27, 131, 8, 234, 223,
        32, 34, 225, 126, 76, 88, 171, 97, 120, 111, 22, 89, 174, 189, 113, 255, 8, 67, 184, 206,
        133, 82, 210, 227, 106, 176, 17, 105, 132, 103, 117, 61, 114, 235, 2, 183, 216, 246, 213,
        57, 111, 174, 139, 247, 70, 142, 225, 151, 15, 144, 249, 214, 149, 255, 45, 193, 0, 161,
        109, 251, 69, 246, 78, 116, 230, 2, 18, 229, 211, 74, 98, 18, 174, 104, 227, 162, 237, 72,
        207, 117, 130, 242, 149, 143, 46, 6, 25, 170, 234, 80, 199, 240, 7, 142, 92, 44, 55, 217,
        205, 139, 86, 8, 4, 140, 164, 223, 233, 109, 78, 188, 127, 130, 237, 39, 219, 189, 29, 47,
        111, 145, 114, 92, 32, 24, 186, 135, 193, 176, 52, 138, 18, 232, 54, 104, 56, 13, 219, 90,
        219, 94, 110, 246, 28, 224, 112, 222, 0, 166, 131, 21, 226, 52, 36, 236, 140, 235, 183,
        226, 80, 77, 58, 26, 218, 173, 223, 209, 111, 191, 126, 87, 215, 91, 93, 71, 246, 25, 190,
        91, 62, 244, 53, 61, 149, 148, 197, 219, 230, 18, 10, 206, 183, 208, 22, 106, 242, 174,
        182, 35, 206, 26, 208, 0, 39, 180, 241, 23, 129, 19, 218, 129, 59, 126, 25, 184, 252, 146,
        246, 248, 204, 177, 4, 42, 2, 198, 69, 50, 0, 243, 27, 42, 41, 68, 177,
    ];*/

    #[test]
    fn test_verify_signature_ak_pub_not_rsa_returns_error() {
        let ak_pub = get_ext_ecc_pub().marshall().unwrap();
        let res = verify_signature(&ak_pub, &ATTEST_SERIALIZED, &SIGNATURE_SERIALIZED);

        match res {
            Ok(..) => {
                panic!("Failed: should have returned error");
            }
            Err(e) => match e {
                AttestQuoteError(d) => {
                    assert_eq!(d, "AK pub is not an RSA key")
                }
                _another_error => panic!("Failed: incorrect error type: {_another_error:?}"),
            },
        }
    }

    #[test]
    fn test_verify_signature_ak_pub_invalid_modulus_returns_error() {
        use tss_esapi::structures::Public::Rsa;
        use tss_esapi::structures::PublicKeyRsa;

        let ak_pub = get_ext_rsa_pub();

        let (object_attributes, name_hashing_algo, auth_policy, params) = match ak_pub {
            Rsa {
                object_attributes,
                name_hashing_algorithm,
                auth_policy,
                parameters,
                ..
            } => (
                object_attributes,
                name_hashing_algorithm,
                auth_policy,
                parameters,
            ),
            _ => panic!("Incorrect key type"),
        };

        let ak_pub_copy = Rsa {
            object_attributes,
            name_hashing_algorithm: name_hashing_algo,
            auth_policy,
            parameters: params,
            unique: PublicKeyRsa::try_from([0, 34, 56].to_vec()).unwrap(), // injecting invalid value
        };

        let res = verify_signature(
            &ak_pub_copy.marshall().unwrap(),
            &ATTEST_SERIALIZED,
            &SIGNATURE_SERIALIZED,
        );

        match res {
            Ok(..) => {
                panic!("Failed: should have returned error");
            }
            Err(e) => match e {
                AttestQuoteError(d) => {
                    assert_eq!(d, "could not create RsaPublicKey: invalid modulus")
                }
                _another_error => panic!("Failed: incorrect error type: {_another_error:?}"),
            },
        }
    }

    #[test]
    fn test_verify_signature_invalid_signature_type_returns_error() {
        use tss_esapi::structures::Signature::RsaSsa;

        let signature = Signature::unmarshall(&SIGNATURE_SERIALIZED).unwrap();

        let rsa_signature = match signature {
            RsaPss(rsa_signature) => rsa_signature,
            _ => panic!("Failed: Unexepected signature type in test"),
        };

        let signature = RsaSsa(rsa_signature);

        let res = verify_signature(
            &AK_PUB_SERIALIZED,
            &ATTEST_SERIALIZED,
            &signature.marshall().unwrap(),
        );

        match res {
            Ok(..) => {
                panic!("Failed: should have returned error");
            }
            Err(e) => match e {
                AttestQuoteError(d) => {
                    assert_eq!(d, "unknown signature type")
                }
                _another_error => panic!("Failed: incorrect error type: {_another_error:?}"),
            },
        }
    }

    #[test]
    fn test_verify_signature_invalid_attestation_returns_false() {
        let bad_attest = [
            255, 84, 67, 53, 10, 168, 230, 203, 59, 199, 64, 128, 150, 218, 164, 66, 52, 72, 227,
            197, 0, 16, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255,
            255, 0, 0, 0, 1, 108, 44, 167, 86, 0, 0, 0, 63, 0, 0, 0, 0, 1, 0, 7, 0, 2, 0, 2, 0, 0,
            0, 0, 0, 1, 0, 11, 3, 255, 15, 0, 0, 32, 69, 159, 141, 33, 201, 110, 233, 102, 224,
            171, 155, 67, 115, 214, 128, 145, 55, 215, 242, 130, 251, 89, 92, 188, 251, 113, 20,
            127, 251, 198, 74, 188,
        ];

        let res = verify_signature(&AK_PUB_SERIALIZED, &bad_attest, &SIGNATURE_SERIALIZED);

        match res {
            Ok(value) => assert!(!value),
            Err(_) => panic!("Failed: Should have returned Ok"),
        }
    }

    #[test]
    fn test_verify_signature_success_returns_true() {
        let res = verify_signature(
            &AK_PUB_SERIALIZED_2,
            &ATTEST_SERIALIZED_2,
            &SIGNATURE_SERIALIZED_2,
        );

        match res {
            Ok(value) => assert!(value),
            Err(_) => panic!("Failed: Should have returned Ok"),
        }
    }

    #[test]
    fn test_verify_pcr_hash_attest_not_match_returns_false() {
        let mut pcr_values_copy = PCR_VALUES_SHORT;
        pcr_values_copy[0][10] = 255;

        let res = verify_pcr_hash(
            &ATTEST_SERIALIZED_SHORT,
            &[pcr_values_copy[0].to_vec(), pcr_values_copy[1].to_vec()],
        );

        match res {
            Ok(value) => assert!(!value),
            Err(_) => panic!("Failed: Should have returned Ok"),
        }
    }

    #[test]
    fn test_verify_pcr_hash_success_returns_true() {
        let res = verify_pcr_hash(
            &ATTEST_SERIALIZED_SHORT,
            &[PCR_VALUES_SHORT[0].to_vec(), PCR_VALUES_SHORT[1].to_vec()],
        );

        match res {
            Ok(value) => assert!(value),
            Err(_) => panic!("Failed: Should have returned Ok"),
        }
    }

    // test_verify_pcr_hash_invalid_quote_type_returns_error - currently impossible to do since Attest fields are private

    //------------------

    use tss_esapi::interface_types::algorithm::HashingAlgorithm;

    fn get_ext_ecc_pub() -> Public {
        use tss_esapi::attributes::ObjectAttributesBuilder;
        use tss_esapi::interface_types::algorithm::PublicAlgorithm;
        use tss_esapi::interface_types::ecc::EccCurve;
        use tss_esapi::structures::{
            EccScheme, KeyDerivationFunctionScheme, PublicBuilder, PublicEccParametersBuilder,
        };

        let object_attributes = ObjectAttributesBuilder::new()
            .with_user_with_auth(true)
            .with_decrypt(false)
            .with_sign_encrypt(true)
            .with_restricted(false)
            .build()
            .expect("Failed to build object attributes");

        let ecc_parameters = PublicEccParametersBuilder::new()
            .with_ecc_scheme(EccScheme::Null)
            .with_curve(EccCurve::NistP256)
            .with_is_signing_key(false)
            .with_is_decryption_key(true)
            .with_restricted(false)
            .with_key_derivation_function_scheme(KeyDerivationFunctionScheme::Null)
            .build()
            .expect("Failed to build PublicEccParameters");
        PublicBuilder::new()
            .with_public_algorithm(PublicAlgorithm::Ecc)
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(object_attributes)
            .with_ecc_parameters(ecc_parameters)
            .with_ecc_unique_identifier(get_ecc_point())
            .build()
            .expect("Failed to build Public structure")
    }

    const EC_POINT: [u8; 65] = [
        0x04, 0x14, 0xd8, 0x59, 0xec, 0x31, 0xe5, 0x94, 0x0f, 0x2b, 0x3a, 0x08, 0x97, 0x64, 0xc4,
        0xfb, 0xa6, 0xcd, 0xaf, 0x0e, 0xa2, 0x44, 0x7f, 0x30, 0xcf, 0xe8, 0x2e, 0xe5, 0x1b, 0x47,
        0x70, 0x01, 0xc3, 0xd6, 0xb4, 0x69, 0x7e, 0xa1, 0xcf, 0x03, 0xdb, 0x05, 0x9c, 0x62, 0x3e,
        0xc6, 0x15, 0x4f, 0xed, 0xab, 0xa0, 0xa0, 0xab, 0x84, 0x2e, 0x67, 0x0c, 0x98, 0xc7, 0x1e,
        0xef, 0xd2, 0x51, 0x91, 0xce,
    ];

    fn get_ecc_point() -> EccPoint {
        use tss_esapi::structures::EccParameter;

        let x =
            EccParameter::try_from(&EC_POINT[1..33]).expect("Failed to construct x EccParameter");
        let y: EccParameter =
            EccParameter::try_from(&EC_POINT[33..]).expect("Failed to construct y EccParameter");
        EccPoint::new(x, y)
    }

    const RSA_KEY: [u8; 256] = [
        0xc9, 0x75, 0xf8, 0xb2, 0x30, 0xf4, 0x24, 0x6e, 0x95, 0xb1, 0x3c, 0x55, 0x0f, 0xe4, 0x48,
        0xe9, 0xac, 0x06, 0x1f, 0xa8, 0xbe, 0xa4, 0xd7, 0x1c, 0xa5, 0x5e, 0x2a, 0xbf, 0x60, 0xc2,
        0x98, 0x63, 0x6c, 0xb4, 0xe2, 0x61, 0x54, 0x31, 0xc3, 0x3e, 0x9d, 0x1a, 0x83, 0x84, 0x18,
        0x51, 0xe9, 0x8c, 0x24, 0xcf, 0xac, 0xc6, 0x0d, 0x26, 0x2c, 0x9f, 0x2b, 0xd5, 0x91, 0x98,
        0x89, 0xe3, 0x68, 0x97, 0x36, 0x02, 0xec, 0x16, 0x37, 0x24, 0x08, 0xb4, 0x77, 0xd1, 0x56,
        0x10, 0x3e, 0xf0, 0x64, 0xf6, 0x68, 0x50, 0x68, 0x31, 0xf8, 0x9b, 0x88, 0xf2, 0xc5, 0xfb,
        0xc9, 0x21, 0xd2, 0xdf, 0x93, 0x6f, 0x98, 0x94, 0x53, 0x68, 0xe5, 0x25, 0x8d, 0x8a, 0xf1,
        0xd7, 0x5b, 0xf3, 0xf9, 0xdf, 0x8c, 0x77, 0x24, 0x9e, 0x28, 0x09, 0x36, 0xf0, 0xa2, 0x93,
        0x17, 0xad, 0xbb, 0x1a, 0xd7, 0x6f, 0x25, 0x6b, 0x0c, 0xd3, 0x76, 0x7f, 0xcf, 0x3a, 0xe3,
        0x1a, 0x84, 0x57, 0x62, 0x71, 0x8a, 0x6a, 0x42, 0x94, 0x71, 0x21, 0x6a, 0x13, 0x73, 0x17,
        0x56, 0xa2, 0x38, 0xc1, 0x5e, 0x76, 0x0b, 0x67, 0x6b, 0x6e, 0xcd, 0xd3, 0xe2, 0x8a, 0x80,
        0x61, 0x6c, 0x1c, 0x60, 0x9d, 0x65, 0xbd, 0x5a, 0x4e, 0xeb, 0xa2, 0x06, 0xd6, 0xbe, 0xf5,
        0x49, 0xc1, 0x7d, 0xd9, 0x46, 0x3e, 0x9f, 0x2f, 0x92, 0xa4, 0x1a, 0x14, 0x2c, 0x1e, 0xb7,
        0x6d, 0x71, 0x29, 0x92, 0x43, 0x7b, 0x76, 0xa4, 0x8b, 0x33, 0xf3, 0xd0, 0xda, 0x7c, 0x7f,
        0x73, 0x50, 0xe2, 0xc5, 0x30, 0xad, 0x9e, 0x0f, 0x61, 0x73, 0xa0, 0xbb, 0x87, 0x1f, 0x0b,
        0x70, 0xa9, 0xa6, 0xaa, 0x31, 0x2d, 0x62, 0x2c, 0xaf, 0xea, 0x49, 0xb2, 0xce, 0x6c, 0x23,
        0x90, 0xdd, 0x29, 0x37, 0x67, 0xb1, 0xc9, 0x99, 0x3a, 0x3f, 0xa6, 0x69, 0xc9, 0x0d, 0x24,
        0x3f,
    ];

    pub(in crate::tests) fn get_ext_rsa_pub() -> Public {
        use tss_esapi::attributes::ObjectAttributesBuilder;
        use tss_esapi::interface_types::algorithm::{PublicAlgorithm, RsaSchemeAlgorithm};
        use tss_esapi::interface_types::key_bits::RsaKeyBits;
        use tss_esapi::structures::{
            PublicBuilder, PublicKeyRsa, PublicRsaParametersBuilder, RsaScheme,
        };

        let object_attributes = ObjectAttributesBuilder::new()
            .with_user_with_auth(true)
            .with_decrypt(false)
            .with_sign_encrypt(true)
            .with_restricted(false)
            .build()
            .expect("Failed to build object attributes");

        PublicBuilder::new()
            .with_public_algorithm(PublicAlgorithm::Rsa)
            .with_name_hashing_algorithm(HashingAlgorithm::Sha256)
            .with_object_attributes(object_attributes)
            .with_rsa_parameters(
                PublicRsaParametersBuilder::new_unrestricted_signing_key(
                    RsaScheme::create(RsaSchemeAlgorithm::RsaSsa, Some(HashingAlgorithm::Sha256))
                        .expect("Failed to create rsa scheme"),
                    RsaKeyBits::Rsa2048,
                    Default::default(), // Default exponent is 0 but TPM internally this is mapped to 65537
                )
                .build()
                .expect("Failed to create rsa parameters for public structure"),
            )
            .with_rsa_unique_identifier(
                PublicKeyRsa::try_from(&RSA_KEY[..])
                    .expect("Failed to create Public RSA key from buffer"),
            )
            .build()
            .expect("Failed to build Public structure")
    }
}
