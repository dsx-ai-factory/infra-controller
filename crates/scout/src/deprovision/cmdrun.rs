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
use std::ffi::OsStr;

use carbide_utils::cmd::TokioCmd;
use scout::CarbideClientError;

pub(super) async fn run_prog<I, S>(command: S, args: I) -> Result<String, CarbideClientError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let command = TokioCmd::new(command);
    command
        .args(args)
        .output()
        .await
        .map_err(CarbideClientError::from)
}
