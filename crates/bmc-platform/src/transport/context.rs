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

use nv_redfish::{Bmc, ServiceRoot};

use super::IpmiOps;

/// Runtime-owned operation context supplied to a stateless driver.
///
/// Redfish operations use wrappers reached through `service_root`; their
/// underlying `B` remains the sole Redfish transport implementation.
pub struct OpCx<'a, B: Bmc> {
    service_root: &'a ServiceRoot<B>,
    ipmi: Option<&'a dyn IpmiOps>,
}

impl<'a, B: Bmc> OpCx<'a, B> {
    pub fn new(service_root: &'a ServiceRoot<B>) -> Self {
        Self {
            service_root,
            ipmi: None,
        }
    }

    pub fn with_ipmi(mut self, ipmi: &'a dyn IpmiOps) -> Self {
        self.ipmi = Some(ipmi);
        self
    }

    pub const fn service_root(&self) -> &'a ServiceRoot<B> {
        self.service_root
    }

    pub fn ipmi(&self) -> Option<&'a dyn IpmiOps> {
        self.ipmi
    }
}
