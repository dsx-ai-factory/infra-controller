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

/*!
 *  gRPC conversions between `PcrRegisterValue` and its protobuf
 *  representation `PcrRegisterValuePb`.
 */

use std::convert::From;

use measured_boot::pcr::PcrRegisterValue;

use crate::protos::measured_boot::PcrRegisterValuePb;

impl From<PcrRegisterValue> for PcrRegisterValuePb {
    fn from(val: PcrRegisterValue) -> Self {
        Self {
            pcr_register: val.pcr_register as i32,
            sha_any: val.sha_any,
        }
    }
}

impl From<PcrRegisterValuePb> for PcrRegisterValue {
    fn from(msg: PcrRegisterValuePb) -> Self {
        Self {
            pcr_register: msg.pcr_register as i16,
            sha_any: msg.sha_any,
        }
    }
}
