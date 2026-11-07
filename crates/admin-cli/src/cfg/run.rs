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

use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

// Run is a trait implemented by leaf argument structs,
// allowing them to execute themselves given a RuntimeContext.
// This complements Dispatch (which is implemented on the
// top-level Cmd enum) by pushing execution logic down to
// the individual command structs.
pub(crate) trait Run {
    fn run(
        self,
        ctx: &mut RuntimeContext,
    ) -> impl std::future::Future<Output = CarbideCliResult<()>>;
}
