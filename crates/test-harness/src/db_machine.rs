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

use std::future::Future;

use model::machine::{Machine, ManagedHostState};
use sqlx::PgTransaction;

pub trait DbMachineExt {
    fn advance_state<'a, 'txn>(
        &'a self,
        txn: &'a mut PgTransaction<'txn>,
        state: ManagedHostState,
    ) -> impl Future<Output = ()> + 'a;

    fn update_state<'a, 'txn>(
        &'a self,
        txn: &'a mut PgTransaction<'txn>,
        state: ManagedHostState,
    ) -> impl Future<Output = ()> + 'a;
}

impl DbMachineExt for Machine {
    async fn advance_state<'txn>(&self, txn: &mut PgTransaction<'txn>, state: ManagedHostState) {
        db::machine::advance(self, txn.as_mut(), &state, None)
            .await
            .expect("machine state should be advanced");
    }

    async fn update_state<'txn>(&self, txn: &mut PgTransaction<'txn>, state: ManagedHostState) {
        let host_machine_id = self.id.try_into().expect("machine should be a host");
        db::machine::update_state(txn.as_mut(), &host_machine_id, &state)
            .await
            .expect("machine state should be updated");
    }
}
