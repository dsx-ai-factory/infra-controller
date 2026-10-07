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

use crate::site_explorer::EndpointExplorationReport;

impl EndpointExplorationReport {
    /// Formats the structured operator error when available, falling back to
    /// the legacy error string stored by older controllers.
    pub fn last_exploration_error_display(&self) -> String {
        self.last_exploration_error_schema
            .as_ref()
            .map(|schema| {
                serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.text.clone())
            })
            .or_else(|| self.last_exploration_error.clone())
            .unwrap_or_default()
    }

    /// Reports whether the endpoint has either a structured operator error or
    /// a non-empty legacy error string.
    pub fn has_last_exploration_error(&self) -> bool {
        self.last_exploration_error_schema.is_some()
            || self
                .last_exploration_error
                .as_deref()
                .is_some_and(|error| !error.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::site_explorer::OperatorErrorSchema;

    fn operator_error_schema() -> OperatorErrorSchema {
        OperatorErrorSchema {
            error_code: "NICO-SITEEXPLORER-122".to_string(),
            mitigation: Some("Check the HCL".to_string()),
            text: "BMC vendor missing".to_string(),
        }
    }

    fn report(
        schema: Option<OperatorErrorSchema>,
        legacy: Option<&str>,
    ) -> EndpointExplorationReport {
        EndpointExplorationReport {
            last_exploration_error: legacy.map(str::to_string),
            last_exploration_error_schema: schema,
            ..Default::default()
        }
    }

    #[test]
    fn error_display_and_presence_are_schema_aware() {
        let schema_display =
            serde_json::to_string_pretty(&operator_error_schema()).expect("schema serializes");

        check_values(
            [
                Check {
                    scenario: "schema takes precedence over legacy error",
                    input: report(Some(operator_error_schema()), Some("legacy error")),
                    expect: (schema_display.clone(), true),
                },
                Check {
                    scenario: "legacy error remains a fallback",
                    input: report(None, Some("legacy error")),
                    expect: ("legacy error".to_string(), true),
                },
                Check {
                    scenario: "schema-only report is an error",
                    input: report(Some(operator_error_schema()), None),
                    expect: (schema_display, true),
                },
                Check {
                    scenario: "report without either field has no error",
                    input: report(None, None),
                    expect: (String::new(), false),
                },
                Check {
                    scenario: "empty legacy error is not an error",
                    input: report(None, Some("")),
                    expect: (String::new(), false),
                },
            ],
            |report| {
                (
                    report.last_exploration_error_display(),
                    report.has_last_exploration_error(),
                )
            },
        );
    }
}
