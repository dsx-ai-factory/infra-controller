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

//! tests/journal.rs
//!
//! Journal:
//! [ ] test_journal_crudl: Make sure basic CRUDL works as expected.

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::machine::MachineId;
    use carbide_uuid::measured_boot::{MeasurementReportId, MeasurementSystemProfileId};
    use measured_boot::records::MeasurementMachineState;

    // test_journal_crudl makes sure database constraints
    // are honored for inserting new journal entries.
    #[crate::sqlx_test]
    async fn test_journal_crudl(pool: sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
        let mut txn = pool.begin().await?;
        let machine_id =
            MachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")?;
        let report_id = MeasurementReportId::new();
        let profile_id = MeasurementSystemProfileId::new();
        let journal = db::measured_boot::journal::new(
            &mut txn,
            machine_id,
            report_id,
            Some(profile_id),
            None,
            MeasurementMachineState::Discovered,
        )
        .await;

        assert!(journal.is_err());
        Ok(())
    }
}
