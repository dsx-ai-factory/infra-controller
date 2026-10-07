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
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tracing_subscriber::filter::EnvFilter;

use super::level_filter::ActiveLevel;
use super::stream::LogStream;

#[derive(Debug, Clone, Default)]
pub struct Logging {
    pub filter: Arc<ActiveLevel>,
    pub tracing_enabled: Arc<AtomicBool>,
    pub spancount_reader: Option<spancounter::SpanCountReader>,
    /// Log stream used to feed the admin web UI. Only fed when the admin UI is
    /// enabled (`enable_admin_ui`); otherwise no [`LogStreamLayer`] is
    /// installed and the stream stays empty.
    pub log_stream: LogStream,
}

pub fn dep_log_filter(env_filter: EnvFilter) -> EnvFilter {
    const DEPS: &str = "sqlxmq::runner=warn,sqlx::query=warn,\
        sqlx::extract_query_data=warn,rustify=off,hyper=error,\
        rustls=warn,tokio_util::codec=warn,vaultrs=error,h2=warn";

    let user = env_filter.to_string();
    let combined = if user.is_empty() {
        DEPS.to_string()
    } else {
        format!("{DEPS},{user}")
    };

    EnvFilter::builder()
        .parse(&combined)
        .unwrap_or_else(|err| panic!("could not reparse combined filter '{combined}': {err}"))
}
