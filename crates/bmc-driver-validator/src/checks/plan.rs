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

//! Which checks a run plans, and which of them cannot run before any does.

use bmc_platform::Capability;

use super::model::{Check, Inputs, Outcome};
use super::validate::{self, EndState};
use super::{apply, reads};
use crate::selection::Selection;

pub(crate) struct Planned {
    pub(crate) check: Check,
    /// Why the check will not run, decided before any check runs.
    pub(crate) skip: Option<Outcome>,
}

/// Which checks a run plans.
pub(crate) enum Scope<'a> {
    /// Every read check, narrowed to the capabilities given, if any.
    Reads(&'a [Capability]),
    /// Exactly the checks named. A `--validate` step runs alone, without the
    /// steps it follows there.
    Checks(&'a [String]),
    /// The `--validate` sequence.
    Validate(EndState),
    /// The factory reset `--factory-reset-first` runs before `--validate`.
    FactoryReset,
}

pub(crate) fn plan(
    selection: &Selection,
    inputs: &Inputs,
    scope: Scope<'_>,
) -> Result<Vec<Planned>, String> {
    let checks = match scope {
        Scope::Reads(only) => reads::checks()
            .into_iter()
            .filter(|check| only.is_empty() || only.contains(&check.capability))
            .collect(),
        Scope::Checks(ids) => named(ids)?,
        Scope::Validate(end_state) => {
            validate::checks(selection.is_supported(Capability::Dpu), end_state)
        }
        Scope::FactoryReset => validate::factory_reset_first(),
    };
    Ok(checks
        .into_iter()
        .map(|check| {
            let skip = if !selection.is_supported(check.capability) {
                Some(Outcome::Unsupported(selection.reason(check.capability)))
            } else {
                check
                    .needs
                    .iter()
                    .find(|needs| !inputs.has(**needs))
                    .map(|needs| needs.skipped())
            };
            Planned { check, skip }
        })
        .collect())
}

/// The checks `ids` name, in that order, each run alone.
fn named(ids: &[String]) -> Result<Vec<Check>, String> {
    ids.iter()
        .map(|id| {
            catalog()
                .into_iter()
                .find(|check| check.id.as_str() == id)
                .ok_or_else(|| {
                    let known: Vec<&str> =
                        catalog().iter().map(|check| check.id.as_str()).collect();
                    format!("unknown check {id}; known checks: {}", known.join(", "))
                })
        })
        .collect()
}

/// Every check `--check` can name, once each: the read checks, the apply
/// checks, and the `--validate` steps.
fn catalog() -> Vec<Check> {
    let mut all: Vec<Check> = Vec::new();
    for check in reads::checks()
        .into_iter()
        .chain(apply::checks())
        .chain(validate::steps())
    {
        if !all.iter().any(|known| known.id == check.id) {
            all.push(check.after(&[]));
        }
    }
    all
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--check` runs exactly the checks it names, a `--validate` step among
    /// them alone, and refuses a name it does not know.
    #[test]
    fn checks_are_run_as_named() {
        let ids = |checks: &[Check]| -> Vec<&'static str> {
            checks.iter().map(|check| check.id.as_str()).collect()
        };
        let chosen = named(&["bmc_control.ntp".to_string(), "power.state".to_string()])
            .expect("known checks");
        assert_eq!(ids(&chosen), ["bmc_control.ntp", "power.state"]);
        assert!(chosen[0].after.is_empty(), "{:?}", chosen[0].after);

        let unknown = named(&["no.such_check".to_string()]).map(|checks| ids(&checks));
        assert!(
            unknown
                .as_ref()
                .is_err_and(|message| message.contains("unknown check no.such_check")),
            "{unknown:?}"
        );
    }
}
