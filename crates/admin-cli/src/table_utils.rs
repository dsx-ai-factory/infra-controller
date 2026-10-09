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

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

#[derive(Debug, Clone)]
pub(crate) enum MaxWidthSpec {
    All(usize),
    Column(String, usize),
}

impl FromStr for MaxWidthSpec {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.split_once('=') {
            Some((column, width)) => {
                if column.is_empty() {
                    return Err(format!(
                        "invalid column in '{value}': column name cannot be empty"
                    ));
                }
                let width = width.parse::<usize>().map_err(|_| {
                    format!("invalid width '{width}' in '{value}': expected a non-negative integer")
                })?;
                Ok(MaxWidthSpec::Column(column.to_string(), width))
            }
            None => {
                let width = value.parse::<usize>().map_err(|_| {
                    format!("invalid value '{value}': expected WIDTH or COLUMN=WIDTH")
                })?;
                Ok(MaxWidthSpec::All(width))
            }
        }
    }
}

#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct MaxWidthArgs {
    #[clap(
        long = "max-width",
        value_name = "[COLUMN=]WIDTH",
        help = "Limit displayed column width to WIDTH characters, truncating longer values \
                with an ellipsis ('...'). A column never narrows below its header's length, so WIDTH is \
                an upper bound on values, not a guaranteed rendered width: a WIDTH shorter \
                than the header still lets values fill the header's width for free, and \
                the ellipsis is only added when that effective width (WIDTH, or the header's \
                length if longer) exceeds 3 characters; at 3 or fewer there's no room for one, \
                so the value is truncated without it. \
                WIDTH 0 means no limit (the same as not specifying that column at all; \
                useful as COLUMN=0 to exempt one column from a blanket --max-width). \
                Repeatable. A bare WIDTH applies to every column; COLUMN=WIDTH limits just \
                that column, where COLUMN must exactly match the column's displayed header \
                text (case-insensitive), e.g. State=40. For a header containing spaces, \
                quote the whole COLUMN=WIDTH argument, e.g. \"COLUMN NAME=40\". An \
                unmatched COLUMN is ignored with a warning listing the valid headers for \
                this invocation."
    )]
    pub(crate) max_width: Vec<MaxWidthSpec>,
}

impl MaxWidthArgs {
    pub(crate) fn widths(&self) -> ColumnWidths {
        ColumnWidths::new(&self.max_width)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ColumnWidths {
    default: Option<usize>,
    // keyed by lowercased column name -> (name as the user typed it, width)
    per_column: HashMap<String, (String, usize)>,
}

impl ColumnWidths {
    pub(crate) fn new(specs: &[MaxWidthSpec]) -> Self {
        let mut widths = ColumnWidths::default();
        for spec in specs {
            match spec {
                MaxWidthSpec::All(width) => widths.default = Some(*width),
                MaxWidthSpec::Column(name, width) => {
                    widths
                        .per_column
                        .insert(name.to_lowercase(), (name.clone(), *width));
                }
            }
        }
        widths
    }

    /// Truncate `value` per the width configured for `header`, if any.
    /// Multi-line cell values (joined with '\n', as some columns do) are
    /// truncated line-by-line so the '\n' structure survives.
    ///
    /// The column can never render narrower than its (untruncated) header,
    /// so a requested width shorter than the header is floored at the
    /// header's length rather than truncating values below that for no
    /// space savings. `0` is left alone: it's the "no limit" sentinel
    /// (see `truncate_line`), not a real width to floor against.
    pub(crate) fn truncate(&self, header: &str, value: &str) -> String {
        let width = self
            .per_column
            .get(&header.to_lowercase())
            .map(|(_, width)| *width)
            .or(self.default);

        match width {
            Some(0) => value.to_string(),
            Some(width) => {
                let width = width.max(header.chars().count());
                value
                    .split('\n')
                    .map(|line| truncate_line(line, width))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            None => value.to_string(),
        }
    }

    /// A COLUMN=WIDTH spec must match a displayed header exactly
    /// (case-insensitively) or it silently does nothing. Build a warning
    /// naming any configured column that doesn't match one of `headers`,
    /// listing the headers that are actually valid for this invocation
    /// (the set varies with other flags, e.g. `--ips`/`--more`), or `None`
    /// if every configured column matched.
    pub(crate) fn describe_unmatched_columns(&self, headers: &[&str]) -> Option<String> {
        let known: HashSet<String> = headers.iter().map(|h| h.to_lowercase()).collect();
        let mut unmatched: Vec<&str> = self
            .per_column
            .iter()
            .filter(|(key, _)| !known.contains(*key))
            .map(|(_, (name, _))| name.as_str())
            .collect();
        if unmatched.is_empty() {
            return None;
        }
        unmatched.sort_unstable();

        Some(format!(
            "--max-width column(s) {} did not match any column in this output \
             (COLUMN must exactly match the displayed header text, case-insensitive, \
             and was ignored). Valid columns here: {}",
            unmatched
                .iter()
                .map(|c| format!("'{c}'"))
                .collect::<Vec<_>>()
                .join(", "),
            headers
                .iter()
                .map(|h| format!("'{h}'"))
                .collect::<Vec<_>>()
                .join(", "),
        ))
    }
}

#[derive(clap::Args, Debug, Clone, Default)]
pub(crate) struct ColumnsArgs {
    #[clap(
        long = "columns",
        value_name = "COLUMN",
        value_delimiter = ',',
        help = "Only show these columns, in the order given. Comma-separated and/or \
                repeatable. COLUMN must exactly match the column's displayed header text \
                (case-insensitive), e.g. --columns id,state. For a header containing spaces, \
                quote it, e.g. --columns \"id,state version\". Omit to show every column in the \
                table's normal order. The unlabeled health-flag column (U/H) is always shown \
                first and can't be filtered out, since it has no header text to select by. An \
                unmatched COLUMN is ignored with a warning listing the valid headers for this \
                invocation."
    )]
    pub(crate) columns: Vec<String>,
}

impl ColumnsArgs {
    pub(crate) fn selection(&self) -> ColumnSelection {
        ColumnSelection::new(&self.columns)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ColumnSelection {
    // As typed by the user, for building the unmatched-column warning.
    // Empty means no filter was requested: show every column.
    requested: Vec<String>,
}

impl ColumnSelection {
    pub(crate) fn new(requested: &[String]) -> Self {
        Self {
            requested: requested.to_vec(),
        }
    }

    /// `headers` filtered to the selected columns and reordered to match the
    /// order the user typed them in `--columns`. The blank health-flag
    /// column ("") has no name to select by, so it's always kept and pinned
    /// first rather than being placed by request order. Unmatched requests
    /// and duplicates are silently skipped (the former is reported
    /// separately by `describe_unmatched_columns`).
    pub(crate) fn ordered_headers<'a>(&self, headers: &[&'a str]) -> Vec<&'a str> {
        if self.requested.is_empty() {
            return headers.to_vec();
        }

        let mut ordered = Vec::new();
        if let Some(blank) = headers.iter().find(|h| h.is_empty()) {
            ordered.push(*blank);
        }
        for requested in &self.requested {
            if let Some(header) = headers
                .iter()
                .find(|h| !h.is_empty() && h.eq_ignore_ascii_case(requested))
                && !ordered.contains(header)
            {
                ordered.push(*header);
            }
        }
        ordered
    }

    /// A --columns value must match a displayed header exactly
    /// (case-insensitively) or it silently selects nothing. Build a warning
    /// naming any requested column that doesn't match one of `headers`,
    /// listing the headers that are actually valid for this invocation, or
    /// `None` if every requested column matched.
    pub(crate) fn describe_unmatched_columns(&self, headers: &[&str]) -> Option<String> {
        // The blank health-flag header ("") is always present in `headers`,
        // but `ordered_headers` never selects it by name (it's pinned
        // unconditionally instead). Treating an empty request as "known"
        // here would silently swallow a stray blank entry (e.g. from
        // `--columns "id,,state"`) that contributed nothing to the
        // selection, so it's always unmatched regardless of `headers`.
        let known: HashSet<String> = headers.iter().map(|h| h.to_lowercase()).collect();
        let mut unmatched: Vec<&str> = self
            .requested
            .iter()
            .filter(|c| c.is_empty() || !known.contains(&c.to_lowercase()))
            .map(|c| c.as_str())
            .collect();
        if unmatched.is_empty() {
            return None;
        }
        unmatched.sort_unstable();

        Some(format!(
            "--columns value(s) {} did not match any column in this output (COLUMN must \
             exactly match the displayed header text, case-insensitive, and was ignored). \
             Valid columns here: {}",
            unmatched
                .iter()
                .map(|c| format!("'{c}'"))
                .collect::<Vec<_>>()
                .join(", "),
            headers
                .iter()
                .map(|h| format!("'{h}'"))
                .collect::<Vec<_>>()
                .join(", "),
        ))
    }
}

fn truncate_line(line: &str, width: usize) -> String {
    if width == 0 || line.chars().count() <= width {
        return line.to_string();
    }
    if width <= 3 {
        return line.chars().take(width).collect();
    }
    let mut truncated: String = line.chars().take(width - 3).collect();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn short_and_long_values_within_a_bare_width() {
        let widths = ColumnWidths::new(&[MaxWidthSpec::All(10)]);
        value_scenarios!(
            run = |value: &str| widths.truncate("State", value);
            "10-char limit" {
                "short" => "short".to_string(),
                "a very long error message" => "a very ...".to_string(),
            }
        );
    }

    #[test]
    fn per_column_override_wins_over_default() {
        let widths = ColumnWidths::new(&[
            MaxWidthSpec::All(10),
            MaxWidthSpec::Column("state".to_string(), 5),
        ]);
        value_scenarios!(
            run = |(header, value): (&str, &str)| widths.truncate(header, value);
            "5-char column override vs. 10-char default" {
                ("State", "abcdefgh") => "ab...".to_string(),
                ("Other", "abcdefgh") => "abcdefgh".to_string(),
            }
        );
    }

    #[test]
    fn column_match_is_case_insensitive() {
        // Width 4 is floored to "state".len() == 5 here (see
        // `width_below_header_length_is_floored_at_header_length`), so this
        // also covers that the floor uses the matched header, not the raw
        // requested width.
        let widths = ColumnWidths::new(&[MaxWidthSpec::Column("STATE".to_string(), 4)]);
        assert_eq!(widths.truncate("state", "abcdefgh"), "ab...");
    }

    #[test]
    fn width_below_header_length_is_floored_at_header_length() {
        // "State" is 5 characters; a requested width of 1 must not truncate
        // values below what's already needed to fit the header, since the
        // column can't render narrower than its header anyway.
        let widths = ColumnWidths::new(&[MaxWidthSpec::Column("State".to_string(), 1)]);
        assert_eq!(widths.truncate("State", "abcdefgh"), "ab...");
    }

    #[test]
    fn zero_width_is_a_no_op() {
        let widths = ColumnWidths::new(&[MaxWidthSpec::All(0)]);
        assert_eq!(
            widths.truncate("State", "a very long error message"),
            "a very long error message"
        );
    }

    #[test]
    fn zero_width_column_override_exempts_column_from_default() {
        let widths = ColumnWidths::new(&[
            MaxWidthSpec::All(5),
            MaxWidthSpec::Column("state".to_string(), 0),
        ]);
        value_scenarios!(
            run = |(header, value): (&str, &str)| widths.truncate(header, value);
            "column override of 0 exempts State; Other still floors to the 5-char default" {
                ("State", "a very long error message") => "a very long error message".to_string(),
                ("Other", "a very long error message") => "a ...".to_string(),
            }
        );
    }

    #[test]
    fn multiline_values_are_truncated_per_line() {
        let widths = ColumnWidths::new(&[MaxWidthSpec::All(5)]);
        assert_eq!(
            widths.truncate("State", "short\na very long line"),
            "short\na ..."
        );
    }

    #[test]
    fn no_widths_configured_is_a_no_op() {
        let widths = ColumnWidths::default();
        assert_eq!(
            widths.truncate("State", "a very long error message"),
            "a very long error message"
        );
    }

    #[test]
    fn parses_bare_width() {
        let spec = "40".parse::<MaxWidthSpec>().unwrap();
        assert!(matches!(spec, MaxWidthSpec::All(40)));
    }

    #[test]
    fn parses_column_width() {
        let spec = "State=40".parse::<MaxWidthSpec>().unwrap();
        assert!(matches!(spec, MaxWidthSpec::Column(name, 40) if name == "State"));
    }

    #[test]
    fn rejects_invalid_width() {
        assert!("abc".parse::<MaxWidthSpec>().is_err());
        assert!("State=abc".parse::<MaxWidthSpec>().is_err());
        assert!("=40".parse::<MaxWidthSpec>().is_err());
    }

    #[test]
    fn unmatched_column_produces_a_warning_listing_valid_headers() {
        let widths = ColumnWidths::new(&[MaxWidthSpec::Column("Sate".to_string(), 40)]);
        let message = widths
            .describe_unmatched_columns(&["", "Machine IDs (H/D)", "State"])
            .expect("should warn about the unmatched column");
        assert!(message.contains("'Sate'"));
        assert!(message.contains("'State'"));
        assert!(message.contains("'Machine IDs (H/D)'"));
    }

    #[test]
    fn matched_column_produces_no_warning() {
        let widths = ColumnWidths::new(&[MaxWidthSpec::Column("state".to_string(), 40)]);
        assert!(widths.describe_unmatched_columns(&["", "State"]).is_none());
    }

    #[test]
    fn bare_width_never_warns() {
        let widths = ColumnWidths::new(&[MaxWidthSpec::All(40)]);
        assert!(widths.describe_unmatched_columns(&["State"]).is_none());
    }

    #[test]
    fn no_columns_requested_shows_everything() {
        let selection = ColumnSelection::new(&[]);
        assert_eq!(
            selection.ordered_headers(&["", "Id", "State", "Labels"]),
            vec!["", "Id", "State", "Labels"]
        );
    }

    #[test]
    fn requested_columns_are_shown_others_are_not() {
        let selection = ColumnSelection::new(&["Id".to_string(), "State".to_string()]);
        assert_eq!(
            selection.ordered_headers(&["", "Id", "State", "Labels"]),
            vec!["", "Id", "State"]
        );
    }

    #[test]
    fn blank_header_is_always_shown() {
        let selection = ColumnSelection::new(&["Id".to_string()]);
        assert_eq!(selection.ordered_headers(&["", "Id"]), vec!["", "Id"]);
    }

    #[test]
    fn unmatched_requested_column_produces_a_warning_listing_valid_headers() {
        let selection = ColumnSelection::new(&["Sate".to_string()]);
        let message = selection
            .describe_unmatched_columns(&["", "Id", "State"])
            .expect("should warn about the unmatched column");
        assert!(message.contains("'Sate'"));
        assert!(message.contains("'State'"));
        assert!(message.contains("'Id'"));
    }

    #[test]
    fn matched_requested_column_produces_no_warning() {
        let selection = ColumnSelection::new(&["state".to_string()]);
        assert!(
            selection
                .describe_unmatched_columns(&["", "State"])
                .is_none()
        );
    }

    #[test]
    fn empty_selection_never_warns() {
        let selection = ColumnSelection::new(&[]);
        assert!(selection.describe_unmatched_columns(&["State"]).is_none());
    }

    #[test]
    fn stray_empty_entry_in_requested_columns_warns() {
        // A stray blank entry (e.g. from a trailing/consecutive comma in
        // `--columns`) must not be silently treated as matching the blank
        // health-flag header, which is always present in `headers` but is
        // never selectable by name.
        value_scenarios!(
            run = |columns: &[&str]| {
                let requested: Vec<String> = columns.iter().map(|c| c.to_string()).collect();
                ColumnSelection::new(&requested)
                    .describe_unmatched_columns(&["", "Id", "State"])
                    .is_some()
            };
            "warns only when a requested entry fails to match a real header" {
                [].as_slice() => false,
                ["Id", "State"].as_slice() => false,
                ["Id", ""].as_slice() => true,
                ["Id", "", "State"].as_slice() => true,
            }
        );
    }

    #[test]
    fn no_selection_keeps_headers_in_original_order() {
        let selection = ColumnSelection::new(&[]);
        assert_eq!(
            selection.ordered_headers(&["", "Id", "State", "Labels"]),
            vec!["", "Id", "State", "Labels"]
        );
    }

    #[test]
    fn selection_reorders_headers_to_match_requested_order() {
        let selection = ColumnSelection::new(&["State".to_string(), "Id".to_string()]);
        assert_eq!(
            selection.ordered_headers(&["", "Id", "State", "Labels"]),
            vec!["", "State", "Id"]
        );
    }

    #[test]
    fn ordered_headers_pins_blank_column_first_regardless_of_request_order() {
        let selection = ColumnSelection::new(&["Labels".to_string(), "Id".to_string()]);
        assert_eq!(
            selection.ordered_headers(&["", "Id", "State", "Labels"]),
            vec!["", "Labels", "Id"]
        );
    }

    #[test]
    fn ordered_headers_is_case_insensitive() {
        let selection = ColumnSelection::new(&["STATE".to_string()]);
        assert_eq!(
            selection.ordered_headers(&["", "Id", "State"]),
            vec!["", "State"]
        );
    }

    #[test]
    fn ordered_headers_skips_unmatched_and_duplicate_requests() {
        let selection =
            ColumnSelection::new(&["State".to_string(), "Sate".to_string(), "State".to_string()]);
        assert_eq!(
            selection.ordered_headers(&["", "Id", "State"]),
            vec!["", "State"]
        );
    }
}
