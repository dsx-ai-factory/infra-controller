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

//! The decision register compiled into the binary: entries with their options,
//! the implications between them, and the exclusions.

use std::sync::OnceLock;

use serde::Deserialize;

const DECISIONS: &str = include_str!("../register/decisions.yaml");
const RULES: &str = include_str!("../register/rules.yaml");

/// One register entry. A decision is a switch for the checks, never a value
/// they grade; the chosen option selects which assertions run.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Decision {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) section: String,
    /// True for the entries no collateral records; they must be stated in
    /// the site definition. Every other entry is derived from `source`.
    pub(crate) stated: bool,
    /// Where the tool reads the derived value, in the register's words.
    pub(crate) source: String,
    pub(crate) options: Vec<OptionDef>,
    /// True for an "Any of" entry: the site definition states a list, possibly empty.
    #[serde(default)]
    pub(crate) multiple: bool,
    /// Single option name, or several joined with ` ; ` for a multi-select.
    #[serde(default)]
    pub(crate) default: Option<String>,
    #[serde(default)]
    pub(crate) check: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OptionDef {
    /// The short form a site definition states.
    pub(crate) name: String,
    /// The register's full option text.
    pub(crate) text: String,
}

/// A decision and the options of it that trigger a rule or a conflict.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Trigger {
    pub(crate) id: String,
    pub(crate) options: Vec<String>,
    /// True when the trigger is the decision being answered *without* any of
    /// `options` (an unticked box of a multi-select).
    #[serde(default)]
    pub(crate) lacks: bool,
}

/// A decision and the one option a rule implies for it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Implied {
    pub(crate) id: String,
    pub(crate) option: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rule {
    pub(crate) when: Trigger,
    pub(crate) set: Vec<Implied>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Conflict {
    pub(crate) between: Vec<Trigger>,
    pub(crate) reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionsFile {
    decisions: Vec<Decision>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    rules: Vec<Rule>,
    conflicts: Vec<Conflict>,
}

#[derive(Debug)]
pub(crate) struct Register {
    pub(crate) decisions: Vec<Decision>,
    pub(crate) rules: Vec<Rule>,
    pub(crate) conflicts: Vec<Conflict>,
}

impl Register {
    /// The register shipped in this binary.
    pub(crate) fn builtin() -> &'static Register {
        static REGISTER: OnceLock<Register> = OnceLock::new();
        REGISTER.get_or_init(|| {
            let decisions: DecisionsFile =
                serde_yaml::from_str(DECISIONS).expect("register/decisions.yaml parses");
            let rules: RulesFile = serde_yaml::from_str(RULES).expect("register/rules.yaml parses");
            Register {
                decisions: decisions.decisions,
                rules: rules.rules,
                conflicts: rules.conflicts,
            }
        })
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Decision> {
        self.decisions.iter().find(|d| d.id == id)
    }

    pub(crate) fn stated(&self) -> impl Iterator<Item = &Decision> {
        self.decisions.iter().filter(|d| d.stated)
    }
}

impl Decision {
    /// The option whose short name is `value`, compared case-insensitively.
    pub(crate) fn option(&self, value: &str) -> Option<&OptionDef> {
        self.options
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case(value.trim()))
    }

    pub(crate) fn option_names(&self) -> String {
        self.options
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

impl Trigger {
    /// True when the decision is answered and one of its values is a triggering
    /// option, or, with `lacks`, when it is answered and none of them is.
    pub(crate) fn holds(&self, values: Option<&[String]>) -> bool {
        let Some(values) = values else {
            return false;
        };
        let any = values.iter().any(|v| {
            self.options
                .iter()
                .any(|o| o.eq_ignore_ascii_case(v.trim()))
        });
        if self.lacks { !any } else { any }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_register_is_consistent() {
        let reg = Register::builtin();
        assert_eq!(reg.decisions.len(), 38);
        assert_eq!(reg.stated().count(), 5);
        for d in &reg.decisions {
            assert!(!d.options.is_empty(), "{} has no options", d.id);
            if let Some(default) = &d.default {
                assert!(
                    default.split(" ; ").all(|part| d.option(part).is_some()),
                    "{} default `{default}` is not an option",
                    d.id
                );
            }
        }
        let check = |id: &str, option: &str| {
            let d = reg
                .get(id)
                .unwrap_or_else(|| panic!("rule names unknown decision `{id}`"));
            assert!(
                d.option(option).is_some(),
                "`{option}` is not an option of {id}"
            );
        };
        for rule in &reg.rules {
            for option in &rule.when.options {
                check(&rule.when.id, option);
            }
            for implied in &rule.set {
                check(&implied.id, &implied.option);
            }
        }
        for conflict in &reg.conflicts {
            assert_eq!(conflict.between.len(), 2, "{}", conflict.reason);
            for trigger in &conflict.between {
                for option in &trigger.options {
                    check(&trigger.id, option);
                }
            }
        }
    }
}
