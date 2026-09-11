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

mod client;
mod extended_processor;
mod on_change_processor;
mod sample_processor;
pub(in crate::collectors) mod subscriber;

mod proto {
    // prost generates ExtensionId::EidUnset / EidExperimental from gnmi_ext.proto,
    // where the proto convention prefixes every value with the enum abbreviation.
    // clippy flags the shared "Eid" prefix but we can't control generated code.
    #![allow(clippy::enum_variant_names)]
    #![allow(
        unreachable_pub,
        reason = "tonic_prost_build emits public items for this crate-internal protocol module"
    )]

    mod gnmi_ext {
        tonic::include_proto!("gnmi_ext");
    }
    tonic::include_proto!("gnmi");
}
