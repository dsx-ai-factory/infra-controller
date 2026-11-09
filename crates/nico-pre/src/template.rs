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

//! `nico-pre decisions`: the register as a table, and the `decisions`
//! section of a site definition written from it.

use std::fmt::Write as _;

use indexmap::IndexMap;

use crate::register::Register;
use crate::sitedef::DecisionValue;

/// The `decisions` section: stated entries live, carrying the current value
/// or `REQUIRED`; derived entries commented out with their default and the
/// collateral they are read from. Reading the derived values from the
/// collaterals themselves arrives with the config checklist.
pub(crate) fn decisions_section(
    current: &IndexMap<String, DecisionValue>,
    reg: &Register,
) -> String {
    let mut out = String::new();
    out.push_str("decisions:                         # one key per register entry; value = the option's short name, a list for `Any of` entries\n");
    out.push_str(
        "  # Stated: no collateral records these. REQUIRED unless implied by another answer.\n",
    );
    let effective = crate::gate::effective_decisions_from(current, reg);
    for d in reg.stated() {
        let kind = if d.multiple { "any of" } else { "one of" };
        match (current.get(&d.id), effective.values.get(&d.id)) {
            (Some(v), _) => {
                let _ = writeln!(out, "  {}: {}", d.id, v.render());
            }
            (None, Some(implied)) => {
                let _ = writeln!(
                    out,
                    "  # {}: {}   # implied by {}; state it only to pin it",
                    d.id,
                    implied.join(", "),
                    effective
                        .implied_by
                        .get(&d.id)
                        .map(String::as_str)
                        .unwrap_or("a rule")
                );
            }
            (None, None) => {
                let _ = writeln!(out, "  {}: REQUIRED", d.id);
            }
        }
        let _ = writeln!(out, "  #   {kind}: {}", d.option_names());
    }
    out.push_str(
        "  # Derived: read from the collaterals and the setup section. Uncomment to pin.\n",
    );
    let mut section = "";
    for d in reg.decisions.iter().filter(|d| !d.stated) {
        if d.section != section {
            section = &d.section;
            let _ = writeln!(out, "  # -- {section}");
        }
        let value = match current.get(&d.id) {
            Some(v) => v.render(),
            None => match (&d.default, d.multiple) {
                (Some(def), true) => format!("[{}]", def.replace(" ; ", ", ")),
                (Some(def), false) => def.clone(),
                (None, true) => "[]".to_string(),
                (None, false) => String::new(),
            },
        };
        let prefix = if current.contains_key(&d.id) {
            "  "
        } else {
            "  # "
        };
        let _ = writeln!(out, "{prefix}{}: {}", d.id, value);
        let kind = if d.multiple { "any of" } else { "one of" };
        let _ = writeln!(
            out,
            "  #   {kind}: {}; read from: {}",
            d.option_names(),
            brief(&d.source)
        );
    }
    out
}

/// The first clause of a Config File row, capped at 120 characters.
fn brief(source: &str) -> String {
    let first = source.split(';').next().unwrap_or(source).trim();
    if first.chars().count() > 120 {
        let cut: String = first.chars().take(117).collect();
        format!("{cut}...")
    } else {
        first.to_string()
    }
}

/// The register as a table: id, stated or derived, options.
pub(crate) fn register_table(reg: &Register) -> String {
    let mut out = String::new();
    let width = reg.decisions.iter().map(|d| d.id.len()).max().unwrap_or(0);
    let mut section = "";
    for d in &reg.decisions {
        if d.section != section {
            section = &d.section;
            let _ = writeln!(out, "\n{section}");
        }
        let kind = if d.stated { "stated " } else { "derived" };
        let shape = if d.multiple { " (any of)" } else { "" };
        let _ = writeln!(out, "  {:width$}  {kind}  {}{shape}", d.id, d.title);
        if let Some(default) = &d.default {
            let _ = writeln!(out, "  {:width$}           default: {default}", "");
        }
        for o in &d.options {
            // The short name is what site-def.yaml states; show it even where the
            // register's text starts differently (`REST group`: `REST API, Flow ...`).
            let line = if o.text.starts_with(o.name.as_str()) {
                o.text.clone()
            } else {
                format!("{}: {}", o.name, o.text)
            };
            let _ = writeln!(out, "  {:width$}           - {line}", "");
        }
        if let Some(check) = &d.check {
            let _ = writeln!(out, "  {:width$}           check: {check}", "");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sitedef::parse;

    #[test]
    fn template_round_trips_through_the_gate() {
        let sd = parse(include_str!("../examples/site-def.yaml")).expect("sample parses");
        let section = decisions_section(&sd.decisions, Register::builtin());
        // Splice the generated section over the sample's and make sure the gate still passes.
        let start = include_str!("../examples/site-def.yaml")
            .find("decisions:")
            .expect("decisions key");
        let end = include_str!("../examples/site-def.yaml")
            .find("\nhosts:")
            .expect("hosts key");
        let text = format!(
            "{}{}{}",
            &include_str!("../examples/site-def.yaml")[..start],
            section,
            &include_str!("../examples/site-def.yaml")[end..]
        );
        let spliced = parse(&text).expect("spliced site definition parses");
        let rows = crate::gate::run(&spliced, Register::builtin());
        assert!(
            rows.iter().all(|r| r.status == crate::check::Status::Pass),
            "{rows:#?}"
        );
        // A template written with nothing stated marks every stated entry REQUIRED.
        let empty = decisions_section(&IndexMap::new(), Register::builtin());
        assert_eq!(empty.matches(": REQUIRED").count(), 5);
        // With the sample's answers, the implied stated entry is written as a comment, not REQUIRED.
        assert!(
            section
                .contains("# external-registry-access: Not applicable   # implied by staging-host")
        );
        assert_eq!(section.matches(": REQUIRED").count(), 0);
    }
}
