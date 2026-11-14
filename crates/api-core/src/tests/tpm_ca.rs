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
    use common::api_fixtures::dpu::create_dpu_machine;
    use common::api_fixtures::host::host_discover_dhcp;
    use common::api_fixtures::tpm_attestation::{
        CA_CERT_SERIALIZED, CA2_CERT_SERIALIZED, EK_CERT_SERIALIZED, EK2_CERT_SERIALIZED,
    };
    use common::api_fixtures::{TestEnv, create_test_env};
    use db::ObjectColumnFilter;
    use model::hardware_info::{HardwareInfo, TpmEkCertificate};
    use model::machine::machine_id::from_hardware_info;
    use model::network_segment;
    use rpc::forge::forge_server::Forge;
    use rpc::forge::{TpmCaCert, TpmCaCertDetail, TpmCaCertId, TpmEkCertStatus};
    use sha2::{Digest, Sha256};

    use crate::attestation::get_ek_cert_by_machine_id;
    use crate::attestation::tpm_ca_cert::match_update_existing_ek_cert_status_against_ca;
    use crate::tests::common;

    #[crate::sqlx_test]
    async fn test_get_ek_cert_by_machine_id_machine_not_found_returns_error(pool: sqlx::PgPool) {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut txn = env.pool.begin().await.unwrap();

        match get_ek_cert_by_machine_id(&mut txn, &host_id).await {
            Err(e) => assert_eq!(
                e.to_string(),
                "internal error: machine with id fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530 not found"
            ),
            _ => panic!("Failed: should have returned an error"),
        }
    }

    #[crate::sqlx_test]
    async fn test_get_ek_cert_by_machine_hw_info_not_found_returns_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // We can't use the fixture created Machine here, since it already has a topology attached
        // therefore we create a new one
        let env = create_test_env(pool).await;
        let host_config = env.managed_host_config();
        let dpu = host_config.get_and_assert_single_dpu();

        let mut txn = env.pool.begin().await?;

        let segment = db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(db::network_segment::IdColumn, env.admin_segment_ref()),
            network_segment::NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap()
        .remove(0);

        let iface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&segment),
            &dpu.host_mac_address,
            true,
            model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await
        .unwrap();

        // hardware_info is never inserted via db::machine_topology::create_or_update thus triggering an error
        let hardware_info = HardwareInfo::from(&host_config);
        let machine_id = from_hardware_info(&hardware_info).unwrap();
        let _machine = db::machine::get_or_create(&mut txn, None, &machine_id, &iface)
            .await
            .unwrap();

        txn.commit().await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await?;

        match get_ek_cert_by_machine_id(&mut txn, &machine_id).await {
            Err(e) => assert_eq!(e.to_string(), "internal error: hardware info not found"),
            _ => panic!("Failed: should have returned an error"),
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_get_ek_cert_by_machine_tpm_ek_cert_not_found_returns_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // We can't use the fixture created Machine here, since it already has a topology attached
        // therefore we create a new one
        let env = create_test_env(pool).await;
        let host_config = env.managed_host_config();
        let dpu = host_config.get_and_assert_single_dpu();

        let mut txn = env.pool.begin().await?;

        let segment = db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(db::network_segment::IdColumn, env.admin_segment_ref()),
            network_segment::NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap()
        .remove(0);

        let iface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&segment),
            &dpu.host_mac_address,
            true,
            model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await
        .unwrap();
        let mut hardware_info = HardwareInfo::from(&host_config);
        let machine_id = from_hardware_info(&hardware_info).unwrap();
        let machine = db::machine::get_or_create(&mut txn, None, &machine_id, &iface)
            .await
            .unwrap();

        txn.commit().await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await?;

        // set ek cert to None to trigger an error
        hardware_info.tpm_ek_certificate = None;
        db::machine_topology::create_or_update(&mut txn, &machine.id, &hardware_info).await?;

        txn.commit().await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await?;

        match get_ek_cert_by_machine_id(&mut txn, &machine_id).await {
            Err(e) => assert_eq!(
                e.to_string(),
                "internal error: TPM EK certificate not found"
            ),
            _ => panic!("Failed: should have returned an error"),
        }

        Ok(())
    }

    use crate::attestation::extract_ca_fields;

    #[test]
    fn test_extract_ca_fields_invalid_cert_returns_error() {
        let random_bytes: &[u8] = &[12, 34, 45];
        match extract_ca_fields(random_bytes) {
            Err(e) => assert_eq!(
                e.to_string(),
                "argument is invalid: could not parse CA cert: Parsing Error: NomError(Eof)"
            ),
            _ => panic!("Failed: expected an error to be returned!"),
        }
    }

    use db::attestation::ek_cert_verification_status;

    use crate::attestation::match_insert_new_ek_cert_status_against_ca;

    #[crate::sqlx_test]
    async fn test_match_insert_new_ek_cert_status_against_ca_invalid_ek_cert_returns_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut txn = env.pool.begin().await.unwrap();

        // corrupt certificate
        let mut ek_cert_corrupted = EK_CERT_SERIALIZED;

        ek_cert_corrupted[56] = 20;
        ek_cert_corrupted[543] = 92;

        let ek_cert = TpmEkCertificate::from(ek_cert_corrupted.to_vec());

        match match_insert_new_ek_cert_status_against_ca(&mut txn, &ek_cert, &host_id).await {
            Err(e) => assert_eq!(
                e.to_string(),
                "argument is invalid: could not parse EK cert: Parsing Error: NomError(Eof)"
            ),
            _ => panic!("Failed: should have rertuned an error"),
        }

        Ok(())
    }

    #[crate::sqlx_test(fixtures("create_tpm_ca_cert_invalid"))]
    async fn test_match_insert_new_ek_cert_status_against_ca_invalid_ca_cert_returns_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut txn = env.pool.begin().await.unwrap();

        let ek_cert = TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec());

        match match_insert_new_ek_cert_status_against_ca(&mut txn, &ek_cert, &host_id).await {
            Err(e) => assert_eq!(
                e.to_string(),
                "argument is invalid: could not parse CA cert: Parsing Error: NomError(Eof)"
            ),
            _ => panic!("Failed: should have rertuned an error"),
        }

        Ok(())
    }

    #[crate::sqlx_test(fixtures("create_tpm_ca_wrong_cert"))]
    async fn test_match_insert_new_ek_cert_status_against_ca_wrong_ca_cert_should_not_match_ek(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut txn = env.pool.begin().await.unwrap();

        let ek_cert = TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec());

        if match_insert_new_ek_cert_status_against_ca(&mut txn, &ek_cert, &host_id)
            .await
            .is_err()
        {
            panic!("Failed: should not have rertuned an error");
        }

        txn.commit().await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await?;

        let ek_cert_status = ek_cert_verification_status::get_by_machine_id(txn.as_mut(), host_id)
            .await
            .expect("Failed: could not make a look up for EkCertVerificationStatus in DB")
            .expect("Failed: could not find EkCertVerificationStatus for given machine in DB");

        assert!(!ek_cert_status.signing_ca_found);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_match_insert_new_ek_cert_status_against_ca_no_ca_found_should_not_match_ek(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let env = create_test_env(pool).await;
        let host_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        let ek_cert = TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec());

        if match_insert_new_ek_cert_status_against_ca(&mut txn, &ek_cert, &host_id)
            .await
            .is_err()
        {
            panic!("Failed: should not have rertuned an error");
        }

        txn.commit().await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await?;

        let ek_cert_status = ek_cert_verification_status::get_by_machine_id(txn.as_mut(), host_id)
            .await
            .expect("Failed: could not make a look up for EkCertVerificationStatus in DB")
            .expect("Failed: could not find EkCertVerificationStatus for given machine in DB");

        assert!(!ek_cert_status.signing_ca_found);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_match_update_existing_ek_cert_against_ca_machine_id_not_found_returns_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up
        let env = create_test_env(pool).await;

        let machine_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
                .unwrap();

        let mut hasher = Sha256::new();
        hasher.update(EK_CERT_SERIALIZED);
        let tpm_ek_cert_sha256 = hasher.finalize();

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        // execute and verify
        match match_update_existing_ek_cert_status_against_ca(
            &mut txn,
            23,
            &CA2_CERT_SERIALIZED,
            &machine_id,
            &tpm_ek_cert_sha256,
        )
        .await
        {
            Ok(_) => panic!("Failed: should have returned an error!"),
            Err(e) => assert_eq!(
                e.to_string(),
                "internal error: machine with id fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530 not found"
            ),
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_match_update_existing_ek_cert_against_ca_ek_cert_invalid_returns_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up
        let env = create_test_env(pool).await;
        // corrupt certificate
        let mut ek_cert_corrupted = EK_CERT_SERIALIZED;

        ek_cert_corrupted[56] = 20;
        ek_cert_corrupted[543] = 92;

        let machine_id = create_machine_with_ek_cert(&ek_cert_corrupted, &env).await?;

        let mut hasher = Sha256::new();
        hasher.update(EK_CERT_SERIALIZED);
        let tpm_ek_cert_sha256 = hasher.finalize();

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        // execute and verify
        match match_update_existing_ek_cert_status_against_ca(
            &mut txn,
            23,
            &CA2_CERT_SERIALIZED,
            &machine_id,
            &tpm_ek_cert_sha256,
        )
        .await
        {
            Ok(_) => panic!("Failed: should have returned an error!"),
            Err(e) => assert_eq!(
                e.to_string(),
                "internal error: could not parse EK cert: Parsing Error: NomError(Eof)"
            ),
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_match_update_existing_ek_cert_against_ca_ca_cert_invalid_returns_error(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up
        let env = create_test_env(pool).await;
        // corrupt certificate
        let mut ca_cert_corrupted = CA2_CERT_SERIALIZED;

        ca_cert_corrupted[0] = 29;
        ca_cert_corrupted[56] = 20;
        ca_cert_corrupted[98] = 92;

        let machine_id = create_machine_with_ek_cert(&EK_CERT_SERIALIZED, &env).await?;

        let mut hasher = Sha256::new();
        hasher.update(EK_CERT_SERIALIZED);
        let tpm_ek_cert_sha256 = hasher.finalize();

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        // execute and verify
        match match_update_existing_ek_cert_status_against_ca(
            &mut txn,
            23,
            &ca_cert_corrupted,
            &machine_id,
            &tpm_ek_cert_sha256,
        )
        .await
        {
            Ok(_) => panic!("Failed: should have returned an error!"),
            Err(e) => assert_eq!(
                e.to_string(),
                "internal error: could not parse CA cert: Parsing Error: Der(UnexpectedTag { expected: Some(Tag(16)), actual: Tag(29) })"
            ),
        }

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_match_update_existing_ek_cert_status_against_ca_signature_not_verified_should_not_change_record(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - create a machine with ek cert
        //        - call match_insert to add the cert status
        let env = create_test_env(pool).await;

        let machine_id = create_machine_with_ek_cert(&EK_CERT_SERIALIZED, &env).await?;

        let ek_cert = TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec());

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        if match_insert_new_ek_cert_status_against_ca(&mut txn, &ek_cert, &machine_id)
            .await
            .is_err()
        {
            panic!("Failed: should not have rertuned an error");
        }

        txn.commit().await?;

        // execute - call with unmatching ek
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        let mut hasher = Sha256::new();
        hasher.update(EK_CERT_SERIALIZED);
        let tpm_ek_cert_sha256 = hasher.finalize();

        match_update_existing_ek_cert_status_against_ca(
            &mut txn,
            23,
            &CA2_CERT_SERIALIZED,
            &machine_id,
            &tpm_ek_cert_sha256,
        )
        .await?;

        txn.commit().await?;

        // verify - make sql query, ek status should remain unmatched
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        let query = "SELECT * from ek_cert_verification_status;";
        let all_ek_cert_statuses =
            sqlx::query_as::<_, model::attestation::EkCertVerificationStatus>(query)
                .fetch_all(&mut *txn)
                .await?;

        assert_eq!(all_ek_cert_statuses.len(), 1);
        assert!(!all_ek_cert_statuses[0].signing_ca_found);

        Ok(())
    }

    //---------

    #[crate::sqlx_test]
    async fn test_tpm_add_ca_cert_no_ek_present_should_insert_ca_in_db(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up
        let ca_cert_subject_expected: [u8; 87] = [
            48, 85, 49, 83, 48, 31, 6, 3, 85, 4, 3, 19, 24, 78, 117, 118, 111, 116, 111, 110, 32,
            84, 80, 77, 32, 82, 111, 111, 116, 32, 67, 65, 32, 50, 49, 49, 49, 48, 37, 6, 3, 85, 4,
            10, 19, 30, 78, 117, 118, 111, 116, 111, 110, 32, 84, 101, 99, 104, 110, 111, 108, 111,
            103, 121, 32, 67, 111, 114, 112, 111, 114, 97, 116, 105, 111, 110, 48, 9, 6, 3, 85, 4,
            6, 19, 2, 84, 87,
        ];

        let env = create_test_env(pool).await;

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        // execute
        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        // verify
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        let query = "SELECT * from tpm_ca_certs;";
        let all_ca_certs = sqlx::query_as::<_, model::attestation::TpmCaCert>(query)
            .fetch_all(&mut *txn)
            .await?;

        assert_eq!(all_ca_certs.len(), 1);

        assert_eq!(all_ca_certs[0].cert_subject, ca_cert_subject_expected);
        assert_eq!(all_ca_certs[0].ca_cert_der, CA_CERT_SERIALIZED);
        assert_eq!(all_ca_certs[0].not_valid_after.timestamp(), 2135920189);
        assert_eq!(all_ca_certs[0].not_valid_before.timestamp(), 1505113789);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_tpm_add_ca_cert_with_two_eks_present_should_update_one_ek(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - create machine and insert to ek statuses into db
        let env = create_test_env(pool).await;

        let machineid_1 = create_machine_with_ek_cert(&EK_CERT_SERIALIZED, &env).await?;
        let machineid_2 = create_machine_with_ek_cert(&EK2_CERT_SERIALIZED, &env).await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        // insert two new ek cert statuses
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            &machineid_1,
        )
        .await?;
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK2_CERT_SERIALIZED.to_vec()),
            &machineid_2,
        )
        .await?;

        txn.commit().await?;

        // execute
        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        // verify
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        let query = "SELECT * from ek_cert_verification_status ORDER BY serial_num;";
        let all_ek_cert_statuses =
            sqlx::query_as::<_, model::attestation::EkCertVerificationStatus>(query)
                .fetch_all(&mut *txn)
                .await?;

        assert_eq!(all_ek_cert_statuses.len(), 2);

        assert!(!all_ek_cert_statuses[0].signing_ca_found);
        assert!(all_ek_cert_statuses[1].signing_ca_found);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_tpm_show_ca_certs_returns_two_ca_certs_from_db(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - insert two ca certs
        let env: TestEnv = create_test_env(pool).await;

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA2_CERT_SERIALIZED.to_vec(),
        });

        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        // execute - try to get those two certs back

        let show_ca_certs_request = tonic::Request::new(());
        let ca_certs = env
            .api
            .tpm_show_ca_certs(show_ca_certs_request)
            .await?
            .into_inner();

        // verify
        assert_eq!(ca_certs.tpm_ca_cert_details.len(), 2);

        let cert_1: &TpmCaCertDetail;
        let cert_2: &TpmCaCertDetail;
        if ca_certs.tpm_ca_cert_details[0]
            .ca_cert_subject
            .contains("CN=NuvotonTPMRootCA1210, O=Nuvoton Technology Corporation, C=TW")
        {
            cert_1 = &ca_certs.tpm_ca_cert_details[0];
            cert_2 = &ca_certs.tpm_ca_cert_details[1];
        } else {
            cert_1 = &ca_certs.tpm_ca_cert_details[1];
            cert_2 = &ca_certs.tpm_ca_cert_details[0];
        }

        assert_eq!(
            cert_1.ca_cert_subject,
            "CN=NuvotonTPMRootCA1210, O=Nuvoton Technology Corporation, C=TW"
        );
        assert_eq!(
            cert_2.ca_cert_subject,
            "CN=Nuvoton TPM Root CA 2111 + O=Nuvoton Technology Corporation + C=TW"
        );

        assert_eq!(cert_1.not_valid_after, "Thu, 27 Jul 2051 02:52:15 +0000");
        assert_eq!(cert_2.not_valid_after, "Mon, 7 Sep 2037 07:09:49 +0000");

        assert_eq!(cert_1.not_valid_before, "Mon, 2 Aug 2021 02:52:15 +0000");
        assert_eq!(cert_2.not_valid_before, "Mon, 11 Sep 2017 07:09:49 +0000");

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_tpm_show_ca_certs_returns_zero_ca_certs_from_db(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let env: TestEnv = create_test_env(pool).await;

        let show_ca_certs_request = tonic::Request::new(());
        let ca_certs = env
            .api
            .tpm_show_ca_certs(show_ca_certs_request)
            .await?
            .into_inner();

        // verify
        assert_eq!(ca_certs.tpm_ca_cert_details.len(), 0);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_tpm_show_unmatched_ek_cert_with_two_unmatched_present_returns_two_eks(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - create machine and insert to ek statuses into db
        let env = create_test_env(pool).await;

        let machineid_1 = create_machine_with_ek_cert(&EK_CERT_SERIALIZED, &env).await?;
        let machineid_2 = create_machine_with_ek_cert(&EK2_CERT_SERIALIZED, &env).await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        // insert two new ek cert statuses
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            &machineid_1,
        )
        .await?;
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK2_CERT_SERIALIZED.to_vec()),
            &machineid_2,
        )
        .await?;

        txn.commit().await?;

        // execute
        let show_ek_certs_request = tonic::Request::new(());
        let ek_certs = env
            .api
            .tpm_show_unmatched_ek_certs(show_ek_certs_request)
            .await?
            .into_inner();

        // verify
        assert_eq!(ek_certs.tpm_ek_cert_statuses.len(), 2);

        let cert_1: &TpmEkCertStatus;
        let cert_2: &TpmEkCertStatus;
        if ek_certs.tpm_ek_cert_statuses[0]
            .serial_num
            .contains("12:f2:03:c8:8e:e1:a2:95:fe:c8")
        {
            cert_1 = &ek_certs.tpm_ek_cert_statuses[0];
            cert_2 = &ek_certs.tpm_ek_cert_statuses[1];
        } else {
            cert_1 = &ek_certs.tpm_ek_cert_statuses[1];
            cert_2 = &ek_certs.tpm_ek_cert_statuses[0];
        }

        assert_eq!(cert_1.serial_num, "12:f2:03:c8:8e:e1:a2:95:fe:c8");
        assert_eq!(cert_2.serial_num, "00:f8:f0:21:90:d3:11:1f:67:2b:a7");

        assert_eq!(
            cert_1.issuer,
            "CN=Nuvoton TPM Root CA 2111 + O=Nuvoton Technology Corporation + C=TW"
        );
        assert_eq!(
            cert_2.issuer,
            "CN=Nuvoton TPM Root CA 2112 + O=Nuvoton Technology Corporation + C=TW"
        );

        assert_eq!(
            cert_1.issuer_ca_url(),
            "https://www.nuvoton.com/security/NTC-TPM-EK-Cert/Nuvoton TPM Root CA 2111.cer"
        );
        assert_eq!(
            cert_2.issuer_ca_url(),
            "https://www.nuvoton.com/security/NTC-TPM-EK-Cert/Nuvoton TPM Root CA 2112.cer"
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_tpm_show_unmatched_ek_cert_one_matched_one_unmatched_return_unmatched(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - create machine and insert to ek statuses into db, then add CA cert
        let env = create_test_env(pool).await;

        let machineid_1 = create_machine_with_ek_cert(&EK_CERT_SERIALIZED, &env).await?;
        let machineid_2 = create_machine_with_ek_cert(&EK2_CERT_SERIALIZED, &env).await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        // insert two new ek cert statuses
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            &machineid_1,
        )
        .await?;
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK2_CERT_SERIALIZED.to_vec()),
            &machineid_2,
        )
        .await?;

        txn.commit().await?;

        // execute - before making a match
        let show_ek_certs_request = tonic::Request::new(());
        let ek_statuses = env
            .api
            .tpm_show_unmatched_ek_certs(show_ek_certs_request)
            .await?
            .into_inner();

        assert_eq!(ek_statuses.tpm_ek_cert_statuses.len(), 2);

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        // this will match one of the ek certs, so that there will be only one unmatched ek left
        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        let show_ek_certs_request = tonic::Request::new(());
        let ek_statuses = env
            .api
            .tpm_show_unmatched_ek_certs(show_ek_certs_request)
            .await?
            .into_inner();

        // verify
        assert_eq!(ek_statuses.tpm_ek_cert_statuses.len(), 1);

        let unmatched_ek = &ek_statuses.tpm_ek_cert_statuses[0];

        assert_eq!(unmatched_ek.serial_num, "00:f8:f0:21:90:d3:11:1f:67:2b:a7");

        assert_eq!(
            unmatched_ek.issuer,
            "CN=Nuvoton TPM Root CA 2112 + O=Nuvoton Technology Corporation + C=TW"
        );

        assert_eq!(
            unmatched_ek.issuer_ca_url(),
            "https://www.nuvoton.com/security/NTC-TPM-EK-Cert/Nuvoton TPM Root CA 2112.cer"
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_tpm_delete_ca_cert_two_ca_present_should_delete_one_ca_only(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - insert two ca certs
        let env: TestEnv = create_test_env(pool).await;

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA2_CERT_SERIALIZED.to_vec(),
        });

        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        // get the list of ca certs and extract their ids
        let show_ca_certs_request = tonic::Request::new(());
        let ca_certs = env
            .api
            .tpm_show_ca_certs(show_ca_certs_request)
            .await?
            .into_inner();

        assert_eq!(ca_certs.tpm_ca_cert_details.len(), 2);

        let ca_cert_id_to_delete = ca_certs.tpm_ca_cert_details[0].ca_cert_id;
        let ca_cert_id_to_keep = ca_certs.tpm_ca_cert_details[1].ca_cert_id;

        // execute - remove ca cert
        let delete_ca_certs_request = tonic::Request::new(TpmCaCertId {
            ca_cert_id: ca_cert_id_to_delete,
        });
        env.api.tpm_delete_ca_cert(delete_ca_certs_request).await?;

        // verify - show should return one remaining ca cert
        let show_ca_certs_request = tonic::Request::new(());
        let ca_certs = env
            .api
            .tpm_show_ca_certs(show_ca_certs_request)
            .await?
            .into_inner();
        assert_eq!(ca_certs.tpm_ca_cert_details.len(), 1);
        assert_eq!(
            ca_certs.tpm_ca_cert_details[0].ca_cert_id,
            ca_cert_id_to_keep
        );

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_tpm_delete_ca_cert_one_ek_matched_one_unmatched_should_unmatch_one_ek(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - insert ca, two eks, one ek is matched
        //          show ca to get its id
        // set up - create machine and insert to ek statuses into db, then add CA cert
        let env = create_test_env(pool).await;

        let machineid_1 = create_machine_with_ek_cert(&EK_CERT_SERIALIZED, &env).await?;
        let machineid_2 = create_machine_with_ek_cert(&EK2_CERT_SERIALIZED, &env).await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();

        // insert two new ek cert statuses
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()),
            &machineid_1,
        )
        .await?;
        match_insert_new_ek_cert_status_against_ca(
            &mut txn,
            &TpmEkCertificate::from(EK2_CERT_SERIALIZED.to_vec()),
            &machineid_2,
        )
        .await?;

        txn.commit().await?;

        // make sure we have two unmatched eks
        let show_unmatched_ek_certs_request = tonic::Request::new(());
        let ek_statuses = env
            .api
            .tpm_show_unmatched_ek_certs(show_unmatched_ek_certs_request)
            .await?
            .into_inner();

        assert_eq!(ek_statuses.tpm_ek_cert_statuses.len(), 2);

        // add a ca cert - one of the eks should be matched now
        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        // make sure we have only one unmatched ek
        let show_unmatched_ek_certs_request = tonic::Request::new(());
        let ek_statuses = env
            .api
            .tpm_show_unmatched_ek_certs(show_unmatched_ek_certs_request)
            .await?
            .into_inner();

        assert_eq!(ek_statuses.tpm_ek_cert_statuses.len(), 1);

        // execute - delete that ca (first find out its ID)
        let show_ca_certs_request = tonic::Request::new(());
        let ca_certs = env
            .api
            .tpm_show_ca_certs(show_ca_certs_request)
            .await?
            .into_inner();

        assert_eq!(ca_certs.tpm_ca_cert_details.len(), 1);

        let ca_cert_id_to_delete = ca_certs.tpm_ca_cert_details[0].ca_cert_id;

        let delete_ca_certs_request = tonic::Request::new(TpmCaCertId {
            ca_cert_id: ca_cert_id_to_delete,
        });
        env.api.tpm_delete_ca_cert(delete_ca_certs_request).await?;

        // verify - the delete must be successful and the matched ek should become unmatched
        // make sure we have two unmatched eks
        let show_unmatched_ek_certs_request = tonic::Request::new(());
        let ek_statuses = env
            .api
            .tpm_show_unmatched_ek_certs(show_unmatched_ek_certs_request)
            .await?
            .into_inner();

        assert_eq!(ek_statuses.tpm_ek_cert_statuses.len(), 2);

        // make sure there are no more ca certs
        let show_ca_certs_request = tonic::Request::new(());
        let ca_certs = env
            .api
            .tpm_show_ca_certs(show_ca_certs_request)
            .await?
            .into_inner();

        assert_eq!(ca_certs.tpm_ca_cert_details.len(), 0);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_discover_machine_ca_present_should_insert_and_match_new_ek(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - create dpu and hw info
        //          insert ca
        let env = create_test_env(pool).await;
        let host_config = env.managed_host_config();
        let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

        let host_machine_interface_id =
            host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

        let mut hardware_info = HardwareInfo::from(&host_config);
        hardware_info.tpm_ek_certificate =
            Some(TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()));

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        env.api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert");

        // execute
        let response = env
            .api
            .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                machine_interface_id: Some(host_machine_interface_id),
                discovery_data: Some(rpc::DiscoveryData::Info(
                    rpc::DiscoveryInfo::try_from(hardware_info).unwrap(),
                )),
                create_machine: true,
                ..Default::default()
            }))
            .await;

        // verify
        response.expect("Expected discover_machine to return Ok");

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        let query = "SELECT * from ek_cert_verification_status;";
        let all_ek_cert_statuses =
            sqlx::query_as::<_, model::attestation::EkCertVerificationStatus>(query)
                .fetch_all(&mut *txn)
                .await?;

        assert_eq!(all_ek_cert_statuses.len(), 1);

        assert!(all_ek_cert_statuses[0].signing_ca_found);

        Ok(())
    }

    #[crate::sqlx_test]
    async fn test_discover_machine_ca_not_present_should_insert_new_unmatched_ek(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - create dpu and hw info
        let env = create_test_env(pool).await;
        let host_config = env.managed_host_config();
        let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

        let host_machine_interface_id =
            host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

        let mut hardware_info = HardwareInfo::from(&host_config);
        hardware_info.tpm_ek_certificate =
            Some(TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()));

        // execute
        let response = env
            .api
            .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                machine_interface_id: Some(host_machine_interface_id),
                discovery_data: Some(rpc::DiscoveryData::Info(
                    rpc::DiscoveryInfo::try_from(hardware_info).unwrap(),
                )),
                create_machine: true,
                ..Default::default()
            }))
            .await;

        // verify
        response.expect("Expected discover_machine to return Ok");

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        let query = "SELECT * from ek_cert_verification_status;";
        let all_ek_cert_statuses =
            sqlx::query_as::<_, model::attestation::EkCertVerificationStatus>(query)
                .fetch_all(&mut *txn)
                .await?;

        assert_eq!(all_ek_cert_statuses.len(), 1);

        assert!(!all_ek_cert_statuses[0].signing_ca_found);

        Ok(())
    }

    // Strictly speaking this is an impossible situation: if ca was inserted after ek
    // then it would match on ca insertion, if ek was inserted after ca, then it could
    // only be inserted via discover_machine, which was already tested above.
    #[crate::sqlx_test]
    async fn test_discover_machine_ca_present_ek_present_should_update_and_match_existing_ek(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // set up - create dpu and hw info
        //          insert ca
        let env = create_test_env(pool).await;
        let host_config = env.managed_host_config();
        let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

        let host_machine_interface_id =
            host_discover_dhcp(&env, &host_config, &dpu_machine_id).await;

        let mut hardware_info = HardwareInfo::from(&host_config);
        hardware_info.tpm_ek_certificate =
            Some(TpmEkCertificate::from(EK_CERT_SERIALIZED.to_vec()));

        let add_ca_request = tonic::Request::new(TpmCaCert {
            ca_cert: CA_CERT_SERIALIZED.to_vec(),
        });

        let cert_id_response = env
            .api
            .tpm_add_ca_cert(add_ca_request)
            .await
            .expect("Failed to add CA cert")
            .into_inner();

        let response = env
            .api
            .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                machine_interface_id: Some(host_machine_interface_id),
                discovery_data: Some(rpc::DiscoveryData::Info(
                    rpc::DiscoveryInfo::try_from(hardware_info.clone()).unwrap(),
                )),
                create_machine: true,
                ..Default::default()
            }))
            .await;

        response.expect("Failed to call discover_machine first time");

        // now aftifically unmatch the ek status
        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        ek_cert_verification_status::unmatch_ca_verification_status(
            &mut txn,
            cert_id_response.id.unwrap().ca_cert_id,
        )
        .await?;

        txn.commit().await?;

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        let query = "SELECT * from ek_cert_verification_status;";
        let all_ek_cert_statuses =
            sqlx::query_as::<_, model::attestation::EkCertVerificationStatus>(query)
                .fetch_all(&mut *txn)
                .await?;

        assert_eq!(all_ek_cert_statuses.len(), 1);

        assert!(!all_ek_cert_statuses[0].signing_ca_found);

        // execute
        let response = env
            .api
            .discover_machine(tonic::Request::new(rpc::MachineDiscoveryInfo {
                machine_interface_id: Some(host_machine_interface_id),
                discovery_data: Some(rpc::DiscoveryData::Info(
                    rpc::DiscoveryInfo::try_from(hardware_info).unwrap(),
                )),
                create_machine: true,
                ..Default::default()
            }))
            .await;

        // verify
        response.expect("Failed to call discover_machine second time");

        let mut txn: sqlx::Transaction<'_, sqlx::Postgres> = env.pool.begin().await.unwrap();
        let query = "SELECT * from ek_cert_verification_status;";
        let all_ek_cert_statuses =
            sqlx::query_as::<_, model::attestation::EkCertVerificationStatus>(query)
                .fetch_all(&mut *txn)
                .await?;

        assert_eq!(all_ek_cert_statuses.len(), 1);

        assert!(all_ek_cert_statuses[0].signing_ca_found);

        Ok(())
    }

    // ---------- helper functions ---------
    async fn create_machine_with_ek_cert(
        ek_cert: &[u8],
        env: &TestEnv,
    ) -> Result<MachineId, Box<dyn std::error::Error>> {
        let mut host_config = env.managed_host_config();
        host_config.tpm_ek_cert = TpmEkCertificate::from(ek_cert.to_vec());
        let dpu = host_config.get_and_assert_single_dpu();

        let mut txn = env.pool.begin().await?;

        let segment = db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(db::network_segment::IdColumn, env.admin_segment_ref()),
            network_segment::NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap()
        .remove(0);

        let iface = db::machine_interface::create(
            &mut txn,
            std::slice::from_ref(&segment),
            &dpu.host_mac_address,
            true,
            model::address_selection_strategy::AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await
        .unwrap();
        let hardware_info = HardwareInfo::from(&host_config);
        let machine_id = from_hardware_info(&hardware_info).unwrap();
        let machine = db::machine::get_or_create(&mut txn, None, &machine_id, &iface)
            .await
            .unwrap();

        txn.commit().await?;

        let mut txn = env.pool.begin().await?;

        db::machine_topology::create_or_update(&mut txn, &machine.id, &hardware_info).await?;

        txn.commit().await?;

        Ok(machine_id)
    }
}
