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

use model::controller_outcome::{PersistentSourceReference, PersistentStateHandlerOutcome};

use crate as rpc;

impl From<PersistentSourceReference> for rpc::forge::ControllerStateSourceReference {
    fn from(source_ref: PersistentSourceReference) -> Self {
        rpc::forge::ControllerStateSourceReference {
            file: source_ref.file,
            line: source_ref.line.try_into().unwrap_or_default(),
        }
    }
}

impl From<PersistentStateHandlerOutcome> for rpc::forge::ControllerStateReason {
    fn from(p: PersistentStateHandlerOutcome) -> rpc::forge::ControllerStateReason {
        use crate::forge::ControllerStateOutcome::*;
        let (outcome, outcome_msg, source_ref) = match p {
            PersistentStateHandlerOutcome::Wait { reason, source_ref } => {
                (Wait, Some(reason), source_ref)
            }
            PersistentStateHandlerOutcome::Error { err, source_ref } => {
                (Error, Some(err), source_ref)
            }
            PersistentStateHandlerOutcome::Transition { source_ref } => {
                (Transition, None, source_ref)
            }
            PersistentStateHandlerOutcome::DoNothing { source_ref } => {
                (DoNothing, None, source_ref)
            }
            PersistentStateHandlerOutcome::DoNothingWithDetails => (DoNothing, None, None),
        };
        rpc::forge::ControllerStateReason {
            outcome: outcome.into(), // into converts it to i32
            outcome_msg,
            source_ref: source_ref.map(Into::into),
        }
    }
}
