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

use model::state_history::StateHistoryRecord;

use crate as rpc;

impl From<StateHistoryRecord> for rpc::forge::StateHistoryRecord {
    fn from(value: StateHistoryRecord) -> rpc::forge::StateHistoryRecord {
        let time = value
            .time
            .unwrap_or_else(|| value.state_version.timestamp());
        rpc::forge::StateHistoryRecord {
            state: value.state,
            version: value.state_version.version_string(),
            time: Some(time.into()),
        }
    }
}

impl From<StateHistoryRecord> for rpc::forge::MachineEvent {
    fn from(value: StateHistoryRecord) -> rpc::forge::MachineEvent {
        let time = value
            .time
            .unwrap_or_else(|| value.state_version.timestamp());
        rpc::forge::MachineEvent {
            event: value.state,
            version: value.state_version.version_string(),
            time: Some(time.into()),
        }
    }
}
