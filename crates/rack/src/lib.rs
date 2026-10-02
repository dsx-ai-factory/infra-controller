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

use librms::RackManagerError;
use state_controller::state_handler::{ExternalServiceError, StateHandlerError};

#[cfg(feature = "test-support")]
mod rms_sim;

#[cfg(feature = "test-support")]
pub mod test_support {
    pub use crate::rms_sim::{MockRmsClient, RmsSim};
}

pub mod bms_client;
pub mod firmware_object;
pub mod firmware_update;
pub mod rms_node_type;

pub fn rack_manager_error(operation: &'static str, error: RackManagerError) -> StateHandlerError {
    ExternalServiceError::with_source(
        "rack_manager",
        operation,
        error.to_string(),
        "rack_manager_error",
        error,
    )
    .into()
}
