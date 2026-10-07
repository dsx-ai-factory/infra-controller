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

use colored::*;
use similar::{ChangeTag, TextDiff};

use crate::duppet::SyncOptions;

/// logln is a macro used for conditional logging based
/// around the --dry-run and --quiet sync options.
#[macro_export]
macro_rules! logln {
    ($options:expr, $($arg:tt)*) => {{
        if !$options.quiet {
            let prefix = if $options.dry_run { "[dry-run] " } else { "" };
            tracing::info!("{}{}", prefix, format!($($arg)*));
        }
    }};
}

/// maybe_colorize colorizes log line prefixes with fancy
/// pretty colors. Can be disabled with the --no-color sync
/// option, but why would you?
pub(super) fn maybe_colorize<'a>(
    text: &'a str,
    style: fn(&'a str) -> ColoredString,
    options: &SyncOptions,
) -> String {
    if options.no_color {
        text.to_string()
    } else {
        style(text).to_string()
    }
}

/// build_diff builds a diff between the source (expected) and
/// destination (existing) files, in the case where the destination
/// file already exists.
pub(super) fn build_diff(src: &str, dst: &str) -> String {
    let diff = TextDiff::from_lines(dst, src);
    let mut diff_output = String::new();

    for change in diff.iter_all_changes() {
        let sign = match change.tag() {
            ChangeTag::Delete => "-",
            ChangeTag::Insert => "+",
            ChangeTag::Equal => " ",
        };
        diff_output.push_str(&format!("{sign}{change}"));
    }
    diff_output.trim_end().to_string()
}
