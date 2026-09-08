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

use std::io;

use super::args::SetArgs;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn registry_set(args: SetArgs, api_client: &ApiClient) -> CarbideCliResult<()> {
    let mut password = String::new();
    io::stdin().read_line(&mut password).map_err(|error| {
        CarbideCliError::GenericError(format!(
            "failed to read registry credential from standard input: {error}"
        ))
    })?;
    if password.contains('\r') || password.contains('\n') {
        return Err(CarbideCliError::GenericError(
            "registry credential from standard input must not contain carriage return or newline characters"
                .to_owned(),
        ));
    }
    if password.is_empty() {
        return Err(CarbideCliError::GenericError(
            "registry password from standard input must not be empty".to_owned(),
        ));
    }

    api_client
        .set_container_registry_credential(args.registry, args.username, password.to_owned())
        .await
}
