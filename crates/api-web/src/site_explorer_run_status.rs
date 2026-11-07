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

use rpc::site_explorer::SiteExplorerLastRun;

const GENERIC_RUN_FAILURE_MESSAGE: &str = "Site Explorer run failed";

fn display_error(run: &SiteExplorerLastRun) -> String {
    match run.failure_category.as_deref() {
        Some("missing_credentials" | "set_credentials") => {
            "Site Explorer credentials are missing or invalid".to_string()
        }
        Some("secrets_engine") => "Site Explorer could not access credentials".to_string(),
        _ if run.success => String::new(),
        _ => GENERIC_RUN_FAILURE_MESSAGE.to_string(),
    }
}

pub(super) struct SiteExplorerLastRunDisplay {
    pub(super) status_label: &'static str,
    pub(super) status_class: &'static str,
    pub(super) started_at: String,
    pub(super) finished_at: String,
    pub(super) endpoint_explorations: i64,
    pub(super) endpoint_explorations_success: i64,
    pub(super) endpoint_explorations_failed: i64,
    pub(super) failure_category: String,
    pub(super) last_successful_finished_at: String,
    pub(super) last_failed_finished_at: String,
    pub(super) error: String,
}

impl From<&SiteExplorerLastRun> for SiteExplorerLastRunDisplay {
    fn from(run: &SiteExplorerLastRun) -> Self {
        let (status_label, status_class) = if run.success {
            ("Success", "success")
        } else {
            ("Failed", "error")
        };
        Self {
            status_label,
            status_class,
            started_at: run.started_at.clone(),
            finished_at: run.finished_at.clone(),
            endpoint_explorations: run.endpoint_explorations,
            endpoint_explorations_success: run.endpoint_explorations_success,
            endpoint_explorations_failed: run.endpoint_explorations_failed,
            failure_category: run.failure_category.clone().unwrap_or_default(),
            last_successful_finished_at: run
                .last_successful_finished_at
                .clone()
                .unwrap_or_else(|| "Never".to_string()),
            last_failed_finished_at: run
                .last_failed_finished_at
                .clone()
                .unwrap_or_else(|| "Never".to_string()),
            error: display_error(run),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn last_run_with(
        success: bool,
        failure_category: Option<&str>,
        error: Option<&str>,
    ) -> SiteExplorerLastRun {
        SiteExplorerLastRun {
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: "2026-01-01T00:01:00Z".to_string(),
            success,
            error: error.map(str::to_string),
            failure_category: failure_category.map(str::to_string),
            endpoint_explorations: 0,
            endpoint_explorations_success: 0,
            endpoint_explorations_failed: 0,
            last_successful_finished_at: None,
            last_failed_finished_at: None,
        }
    }

    #[test]
    fn display_error_sanitizes_unknown_failure_categories() {
        let display = SiteExplorerLastRunDisplay::from(&last_run_with(
            false,
            Some("internal"),
            Some("machines/bmc/site/root"),
        ));
        assert_eq!(display.error, GENERIC_RUN_FAILURE_MESSAGE);
    }

    #[test]
    fn display_error_maps_credential_categories() {
        let display = SiteExplorerLastRunDisplay::from(&last_run_with(
            false,
            Some("missing_credentials"),
            Some("machines/bmc/site/root"),
        ));
        assert_eq!(
            display.error,
            "Site Explorer credentials are missing or invalid"
        );
    }

    #[test]
    fn display_error_is_empty_for_successful_runs() {
        let display = SiteExplorerLastRunDisplay::from(&last_run_with(true, None, Some("ignored")));
        assert!(display.error.is_empty());
    }
}
