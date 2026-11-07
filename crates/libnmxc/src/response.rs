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

use crate::{NmxcError, nmxc_model};

pub(crate) fn check_server_header_success(
    header: Option<&nmxc_model::ServerHeader>,
    operation: &'static str,
) -> Result<(), NmxcError> {
    let Some(header) = header else {
        return Err(NmxcError::MissingServerHeader { operation });
    };
    if header.return_code == nmxc_model::StReturnCode::NmxStSuccess as i32 {
        Ok(())
    } else {
        Err(NmxcError::NmxReturnCode {
            return_code: header.return_code,
            operation,
        })
    }
}
