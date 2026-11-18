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

mod add;
mod common;
mod delete;
mod erase;
mod replace_all;
mod show;
#[cfg(test)]
mod tests;
mod update;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    /// Show one or all expected rack groups
    Show(show::Args),
    /// Add an expected rack group
    Add(add::Args),
    /// Delete an expected rack group
    ///
    /// Deletion fails while expected NVLink domain declarations reference the group.
    /// Delete its declarations with expected-nvlink-domain delete first.
    Delete(delete::Args),
    /// Replace all fields of an existing expected rack group
    ///
    /// While the group has expected NVLink domain declarations, its racks field must remain
    /// unchanged. Topology, protocol, and metadata updates are still allowed. Delete its
    /// declarations with expected-nvlink-domain delete before changing racks.
    Update(update::Args),
    /// Replace all expected rack groups from a JSON file
    ///
    /// This operation clears all existing groups before inserting replacements. Any expected
    /// NVLink domain declaration blocks it, even if the replacement preserves the referenced
    /// group. Delete all declarations with expected-nvlink-domain delete first.
    ReplaceAll(replace_all::Args),
    /// Erase all expected rack groups
    ///
    /// Erase fails while any expected NVLink domain declaration references a group. Delete all
    /// declarations with expected-nvlink-domain delete first; --confirm does not bypass this
    /// restriction.
    Erase(erase::Args),
}
