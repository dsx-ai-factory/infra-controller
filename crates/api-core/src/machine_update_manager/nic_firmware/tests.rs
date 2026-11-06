// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use model::machine::ManagedHostState;
use model::machine::nic_firmware::NicFirmwarePhase;

use super::*;

// Failed workflows no longer retain artifacts, but still own their host's
// update slot until another lifecycle state takes over.
#[crate::sqlx_test]
async fn failed_workflow_occupies_its_host_until_state_departure(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let host: HostMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [17; 32],
        MachineType::Host,
    )
    .try_into()?;
    let mut operation = model::test_support::nic_firmware::operation();
    operation.phase = NicFirmwarePhase::Failed {
        reason: "settled failure after original lockdown readback".into(),
    };
    operation.validate()?;
    let mut txn = pool.begin().await?;
    db::machine::create(
        &mut txn,
        None,
        &host.into(),
        ManagedHostState::NicFirmwareUpdating { operation },
        None,
        2,
    )
    .await?;
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    assert_eq!(
        NicFirmwareUpdate
            .get_updates_in_progress(txn.as_mut())
            .await?,
        HashSet::from([host]),
    );
    txn.commit().await?;
    sqlx::query("UPDATE machines SET controller_state = $2 WHERE id = $1")
        .bind(host)
        .bind(sqlx::types::Json(ManagedHostState::Ready))
        .execute(&pool)
        .await?;
    let mut txn = pool.begin().await?;
    assert!(
        NicFirmwareUpdate
            .get_updates_in_progress(txn.as_mut())
            .await?
            .is_empty()
    );
    txn.commit().await?;
    Ok(())
}
