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

use carbide_machine_controller::write_ops::MachineWriteOp;
use carbide_test_harness::prelude::{sqlx_test, sqlx_testing};
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use chrono::{DateTime, Duration};
use model::machine::{
    CURRENT_STATE_MODEL_VERSION, MachineLastRebootRequested, MachineLastRebootRequestedMode,
    ManagedHostState,
};
use serde_json::json;
use sqlx::PgPool;
use state_controller::db_write_batch::WriteOp;
use state_controller::state_handler::StateHandlerError;

#[sqlx_test]
async fn verification_writes_require_the_captured_reboot_record(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let machine_id = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0xff; 32],
        MachineType::Host,
    );
    let mut txn = pool.begin().await?;
    db::machine::create(
        txn.as_mut(),
        None,
        &machine_id,
        ManagedHostState::Ready,
        None,
        CURRENT_STATE_MODEL_VERSION,
    )
    .await?;
    txn.commit().await?;

    let captured = MachineLastRebootRequested {
        time: DateTime::from_timestamp(1_722_000_000, 123_456_789).unwrap(),
        mode: MachineLastRebootRequestedMode::Reboot,
        restart_verified: Some(false),
        verification_attempts: Some(0),
    };
    let legacy = json!({
        "time": "2023-07-31T11:26:18.261228950+00:00",
        "mode": "Reboot",
        "unrecognized_field": "preserved",
    });
    struct Case {
        scenario: &'static str,
        stored: Option<serde_json::Value>,
        captured: MachineLastRebootRequested,
        applies: bool,
    }
    for case in [
        Case {
            scenario: "the captured record is still current",
            stored: Some(json!(captured)),
            captured,
            applies: true,
        },
        Case {
            scenario: "a newer attempt within the same microsecond is preserved",
            stored: Some(json!(MachineLastRebootRequested {
                time: captured.time + Duration::nanoseconds(1),
                ..captured
            })),
            captured,
            applies: false,
        },
        Case {
            scenario: "verification already changed for the same attempt",
            stored: Some(json!(MachineLastRebootRequested {
                restart_verified: Some(true),
                ..captured
            })),
            captured,
            applies: false,
        },
        Case {
            scenario: "legacy timestamp and omitted verification fields still match",
            captured: serde_json::from_value(legacy.clone())?,
            stored: Some(legacy),
            applies: true,
        },
        Case {
            scenario: "an absent reboot record invalidates verification",
            stored: None,
            captured,
            applies: false,
        },
    ] {
        sqlx::query("UPDATE machines SET last_reboot_requested=$1 WHERE id=$2")
            .bind(case.stored.as_ref().map(sqlx::types::Json))
            .bind(machine_id)
            .execute(&pool)
            .await?;

        let mut txn = pool.begin().await?;
        let result = Box::new(MachineWriteOp::UpdateRestartVerificationStatus {
            machine_id,
            current_reboot: case.captured,
            verified: Some(false),
            attempts: 1,
        })
        .apply(&mut txn)
        .await;
        if case.applies {
            result.expect(case.scenario);
        } else {
            assert!(
                matches!(result, Err(StateHandlerError::IterationInvalidated { .. })),
                "{}: {result:?}",
                case.scenario,
            );
        }
        // Even committing a rejected write must leave the accepted record alone.
        txn.commit().await?;

        let persisted: Option<sqlx::types::Json<serde_json::Value>> =
            sqlx::query_scalar("SELECT last_reboot_requested FROM machines WHERE id=$1")
                .bind(machine_id)
                .fetch_one(&pool)
                .await?;
        let expected = case.stored.map(|mut record| {
            if case.applies {
                record["restart_verified"] = json!(false);
                record["verification_attempts"] = json!(1);
            }
            record
        });
        assert_eq!(
            persisted.map(|record| record.0),
            expected,
            "{}",
            case.scenario
        );
    }
    Ok(())
}
