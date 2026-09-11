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

use std::panic::Location;

use db::{ConditionalWrite, ControllerStateNotCurrent};

use crate::state_handler::StateHandlerError;

/// `CheckApplied` requires a conditional write to apply before a handler continues.
///
/// For [`ControllerStateNotCurrent`], a rejected write returns
/// [`StateHandlerError::IterationInvalidated`]. Propagate this error unchanged
/// with `?`. The processor discards the iteration's uncommitted database writes
/// and queues another pass to read fresh state.
///
/// This does not retry the write or undo external effects; those must remain
/// safe to repeat. Match [`ConditionalWrite`] directly when the handler can
/// continue after a rejected write.
pub trait CheckApplied {
    /// The value returned by an applied write.
    type Value;

    /// `check_applied` returns the applied value, or
    /// [`StateHandlerError::IterationInvalidated`] with the caller's location.
    #[track_caller]
    fn check_applied(self) -> Result<Self::Value, StateHandlerError>;
}

impl<T> CheckApplied for ConditionalWrite<T, ControllerStateNotCurrent> {
    type Value = T;

    #[track_caller]
    fn check_applied(self) -> Result<T, StateHandlerError> {
        match self {
            Self::Applied(value) => Ok(value),
            Self::NotApplied(ControllerStateNotCurrent) => {
                Err(StateHandlerError::IterationInvalidated {
                    source_ref: Location::caller(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_applied_reports_the_call_site() {
        let write: ConditionalWrite<(), _> =
            ConditionalWrite::NotApplied(ControllerStateNotCurrent);
        let call_line = line!() + 1;
        let error = write.check_applied().unwrap_err();
        let StateHandlerError::IterationInvalidated { source_ref } = error else {
            panic!("expected iteration invalidation, got {error:?}");
        };
        assert_eq!(source_ref.file(), file!());
        assert_eq!(source_ref.line(), call_line);
    }
}
