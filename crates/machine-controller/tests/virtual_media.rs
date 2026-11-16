/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Native PXE remains the default unless an expected machine opts into a policy.

use carbide_machine_controller::virtual_media::{
    LifecycleBootProgress, reconcile_pending, request_lifecycle_boot,
};
use carbide_redfish::libredfish::{RedfishAuth, RedfishClientCreationError, RedfishClientPool};
use carbide_secrets::credentials::CredentialReader;
use carbide_test_harness::prelude::*;
use model::host_boot::{HostBootIntent, HostBootPhase, HostBootPurpose};

struct NoBmcOperations;

#[async_trait::async_trait]
impl RedfishClientPool for NoBmcOperations {
    async fn create_client(
        &self,
        _host: &str,
        _port: Option<u16>,
        _auth: RedfishAuth,
        _vendor: Option<libredfish::model::service_root::RedfishVendor>,
    ) -> Result<Box<dyn libredfish::Redfish>, RedfishClientCreationError> {
        panic!("recovery must preserve the observation boundary without any BMC dispatch");
    }

    fn credential_reader(&self) -> &dyn CredentialReader {
        panic!("recovery must not require BMC credentials");
    }
}

#[sqlx_test]
async fn no_boot_policy_preserves_native_pxe(pool: PgPool) {
    let progress = request_lifecycle_boot(
        &pool,
        "02:00:00:ef:00:01".parse().unwrap(),
        "native-host:discovery".to_owned(),
        HostBootPurpose::Discovery,
    )
    .await
    .unwrap();
    assert_eq!(progress, LifecycleBootProgress::NativePxe);
    assert!(db::host_boot::list_active(&pool).await.unwrap().is_empty());
}

/// Explicit cancel recovers failed observation boundaries without replaying writes.
#[sqlx_test]
async fn cancelled_failures_resume_observation_without_replaying_boot(pool: PgPool) {
    let mut ids = Vec::new();
    for (index, failed_phase, reset_unknown, expected_phase) in [
        (1, HostBootPhase::Mounting, false, HostBootPhase::Mounting),
        (
            2,
            HostBootPhase::SettingBoot,
            false,
            HostBootPhase::SettingBoot,
        ),
        (3, HostBootPhase::Detaching, false, HostBootPhase::Detaching),
        (
            4,
            HostBootPhase::Resetting,
            true,
            HostBootPhase::AwaitingAcknowledgement,
        ),
        (5, HostBootPhase::Pending, false, HostBootPhase::Cancelled),
    ] {
        let id = uuid::Uuid::new_v4();
        let bmc_mac_address = format!("02:00:00:00:01:{index:02x}").parse().unwrap();
        let boot_mac_address = format!("02:00:00:00:02:{index:02x}").parse().unwrap();
        sqlx::query("INSERT INTO expected_machines (id,serial_number,bmc_mac_address,bmc_username,bmc_password) VALUES ($1,$2,$3,'test','test')")
            .bind(id).bind(format!("RECOVERY{index}")).bind(bmc_mac_address).execute(&pool).await.unwrap();
        let row = HostBootIntent {
            expected_machine_id: id,
            bmc_mac_address,
            boot_mac_address,
            fixed_ip_address: "192.0.2.10".parse().unwrap(),
            generation: uuid::Uuid::new_v4(),
            requested_purpose: HostBootPurpose::Discovery,
            purpose: HostBootPurpose::Discovery,
            artifact_url: "http://example.test/boot.iso".into(),
            artifact_sha256: "a".repeat(64),
            phase: if index == 5 {
                HostBootPhase::Detaching
            } else {
                HostBootPhase::Failed
            },
            media_uri: (index != 5).then(|| "/redfish/v1/Managers/1/VirtualMedia/1".into()),
            previous_boot: (index != 5)
                .then(|| serde_json::json!({"BootSourceOverrideEnabled": "Disabled"})),
            acknowledgement_token: uuid::Uuid::new_v4(),
            acknowledged: false,
            cancelled: true,
            phase_started_at: Some(chrono::Utc::now() - chrono::Duration::minutes(5)),
            reset_attempted_at: reset_unknown.then(chrono::Utc::now),
            last_error: Some("external write needs attention".into()),
            failed_from_phase: Some(failed_phase),
            lifecycle_key: Some("discovery:1".into()),
            revision: 0,
        };
        let mut connection = pool.acquire().await.unwrap();
        db::host_boot::create(&mut connection, &row).await.unwrap();
        ids.push((row, expected_phase));
    }
    let mut background = tokio::task::JoinSet::new();
    let manager = db::work_lock_manager::start(
        &mut background,
        pool.clone(),
        Default::default(),
        carbide_instrument::Instrumentation::global(),
    )
    .await
    .unwrap();
    reconcile_pending(&pool, &NoBmcOperations, &manager)
        .await
        .unwrap();
    for (original, phase) in ids {
        let recovered = db::host_boot::find(&pool, original.expected_machine_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(recovered.phase, phase);
        assert_eq!(recovered.media_uri, original.media_uri);
        assert_eq!(recovered.previous_boot, original.previous_boot);
        assert_eq!(
            recovered.acknowledgement_token,
            original.acknowledgement_token
        );
        assert_eq!(recovered.reset_attempted_at, original.reset_attempted_at);
        assert_eq!(
            recovered.cleanup_complete(),
            phase == HostBootPhase::Cancelled
        );
        assert!(
            request_lifecycle_boot(
                &pool,
                original.bmc_mac_address,
                "new:discovery".into(),
                HostBootPurpose::Discovery
            )
            .await
            .is_err()
        );
    }
    background.abort_all();
    while background.join_next().await.is_some() {}
}
