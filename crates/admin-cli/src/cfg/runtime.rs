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

use std::time::Duration;

use rpc::admin_cli::OutputFormat;

use crate::cfg::cli_options::SortField;
use crate::errors::CarbideCliError;
use crate::rpc::ApiClient;

// RuntimeContext is context passed to all subcommand
// dispatch handlers. This is built at the beginning of
// runtime and then passed to the appropriate dispatcher.
pub(crate) struct RuntimeContext {
    pub(crate) api_client: ApiClient,
    pub(crate) config: RuntimeConfig,
    pub(crate) output_file: Box<dyn tokio::io::AsyncWrite + Unpin>,
}

// RuntimeConfig contains runtime configuration parameters extracted
// from CLI options. This should contain the entirety of any options
// that need to be leveraged by any downstream command handler.
pub(crate) struct RuntimeConfig {
    pub(crate) format: OutputFormat,
    pub(crate) request_timeout: Option<Duration>,
    pub(crate) page_size: usize,
    pub(crate) extended: bool,
    pub(crate) cloud_unsafe_op: Option<String>,
    pub(crate) sort_by: SortField,
}

impl RuntimeContext {
    pub(crate) fn assert_cloud_unsafe_op_message(&self) -> Result<&str, CarbideCliError> {
        self.config
            .cloud_unsafe_op
            .as_deref()
            .ok_or(CarbideCliError::CloudUnsafeOp)
    }
}
