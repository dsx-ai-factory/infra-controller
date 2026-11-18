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

use carbide_uuid::machine::{DpuMachineId, MachineId, MachineIdSource, MachineType};
use model::dpu_remediation::{ApproveRemediation, EnableRemediation, NewRemediation};
use sqlx::PgPool;

#[crate::sqlx_test]
async fn maximum_retries_allows_initial_attempt(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = pool.begin().await?;
    let remediation = super::persist_remediation(
        NewRemediation {
            script: "echo repair-interface".to_string(),
            metadata: None,
            retries: i32::MAX,
            author: "script-author".to_string().into(),
        },
        &mut txn,
    )
    .await?;
    super::persist_approve_remediation(
        ApproveRemediation {
            id: remediation.id,
            reviewer: "script-reviewer".to_string().into(),
        },
        &mut txn,
    )
    .await?;
    super::persist_enable_remediation(EnableRemediation { id: remediation.id }, &mut txn).await?;

    let machine_id = DpuMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0x75; 32],
        MachineType::Dpu,
    ))?;
    let next_remediation = super::find_next_remediation_for_machine(&mut txn, machine_id).await?;
    assert_eq!(
        next_remediation.map(|next| (next.id, next.retries)),
        Some((remediation.id, i32::MAX)),
    );

    Ok(())
}
