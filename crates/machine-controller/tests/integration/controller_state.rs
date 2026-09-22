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

use carbide_machine_controller::io::MachineStateControllerIO;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::{
    FixtureDefault as _, ManagedHostConfigExt as _,
};
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSource, MachineType};
use config_version::ConfigVersion;
use db::state_history::StateHistoryTableId;
use model::machine::ManagedHostState;
use model::machine::machine_search_config::MachineSearchConfig;
use model::test_support::ManagedHostConfig;
use state_controller::io::StateControllerIO;

#[sqlx_test]
async fn controller_state_rejects_stale_snapshots_without_changing_group_history(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let harness = TestHarness::builder(pool.clone()).build().await;
    let domain = harness.test_domain().await;
    let network_controller = harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = harness.default_test_site_explorer();
    let (host, _) = harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig::default().with_dpu_count(2))
        .build()
        .await;
    let machine_ids: Vec<_> = std::iter::once(host.host.id())
        .chain(host.dpus.iter().map(TestMachine::id))
        .collect();
    let io = MachineStateControllerIO::default();
    let mut txn = pool.begin().await?;
    let snapshot = io
        .load_object_state(&mut txn, &host.host.id)
        .await?
        .expect("host snapshot");
    let old_version = io
        .load_controller_state(&mut txn, &host.host.id, &snapshot)
        .await?
        .version;
    // The accepted write fills the history limit. A rejected write must undo
    // its retention deletion as well as its own history entry.
    let host_history =
        db::state_history::for_object(&mut txn, StateHistoryTableId::Machine, &host.host.id)
            .await?;
    assert!(
        host_history.len() < 250,
        "fixture must leave room for one history entry"
    );
    for _ in host_history.len()..249 {
        db::state_history::persist(
            &mut txn,
            StateHistoryTableId::Machine,
            &host.host.id,
            &snapshot.host_snapshot.state.value,
            old_version,
        )
        .await?;
    }
    let initial_histories =
        db::state_history::find_by_object_ids(&mut txn, StateHistoryTableId::Machine, &machine_ids)
            .await?;
    txn.commit().await?;

    // A distinct replacement proves persistence uses the processor's token,
    // rather than reading the host again and calculating its own increment.
    let winner_version = ConfigVersion::new(old_version.version_nr() + 10);
    struct Case {
        scenario: &'static str,
        new_state: ManagedHostState,
        new_version: ConfigVersion,
        expected: db::ConditionalWrite<(), db::ControllerStateNotCurrent>,
    }
    for case in [
        Case {
            scenario: "current snapshot applies the supplied version",
            new_state: ManagedHostState::Ready,
            new_version: winner_version,
            expected: db::ConditionalWrite::Applied(()),
        },
        Case {
            scenario: "earlier snapshot cannot replace the committed state",
            new_state: ManagedHostState::ForceDeletion,
            new_version: winner_version.increment(),
            expected: db::ConditionalWrite::NotApplied(db::ControllerStateNotCurrent),
        },
    ] {
        let mut txn = pool.begin().await?;
        assert_eq!(
            io.persist_controller_state(
                &mut txn,
                &host.host.id,
                old_version,
                case.new_version,
                &case.new_state,
            )
            .await?,
            case.expected,
            "{}",
            case.scenario,
        );
        // Even committing a rejection must not add history or change a DPU.
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let histories = db::state_history::find_by_object_ids(
            &mut txn,
            StateHistoryTableId::Machine,
            &machine_ids,
        )
        .await?;
        for machine_id in &machine_ids {
            let machine =
                db::machine::find_one(txn.as_mut(), machine_id, MachineSearchConfig::default())
                    .await?
                    .expect("group member");
            assert_eq!(
                machine.state.value,
                ManagedHostState::Ready,
                "{}",
                case.scenario
            );
            assert_eq!(machine.state.version, winner_version, "{}", case.scenario);

            let initial_history = &initial_histories[&machine_id.to_string()];
            let history = &histories[&machine_id.to_string()];
            assert_eq!(
                history.len(),
                initial_history.len() + 1,
                "{}",
                case.scenario
            );
            for (actual, initial) in history.iter().zip(initial_history) {
                assert_eq!(actual.state, initial.state, "{}", case.scenario);
                assert_eq!(
                    actual.state_version, initial.state_version,
                    "{}",
                    case.scenario
                );
                assert_eq!(actual.time, initial.time, "{}", case.scenario);
            }
            let last = history.last().expect("accepted state history");
            assert_eq!(last.state_version, winner_version, "{}", case.scenario);
            assert_eq!(
                serde_json::from_str::<ManagedHostState>(&last.state)?,
                ManagedHostState::Ready,
                "{}",
                case.scenario
            );
        }
        txn.commit().await?;
    }
    Ok(())
}

#[sqlx_test]
async fn controller_state_rejects_a_missing_host_without_recording_history(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let host_id: HostMachineId = MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [0xff; 32],
        MachineType::Host,
    )
    .try_into()?;
    let old_version = ConfigVersion::initial();
    let mut txn = pool.begin().await?;
    assert_eq!(
        MachineStateControllerIO::default()
            .persist_controller_state(
                &mut txn,
                &host_id,
                old_version,
                old_version.increment(),
                &ManagedHostState::Ready,
            )
            .await?,
        db::ConditionalWrite::NotApplied(db::ControllerStateNotCurrent)
    );
    txn.commit().await?;

    let mut txn = pool.begin().await?;
    assert!(
        db::state_history::for_object(&mut txn, StateHistoryTableId::Machine, &host_id)
            .await?
            .is_empty()
    );
    txn.commit().await?;
    Ok(())
}
