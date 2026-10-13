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

use std::collections::BTreeSet;

/// Device targets parsed from `virsh domblklist --details` output.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct BlockList {
    pub(super) targets: BTreeSet<String>,
}

impl From<&str> for BlockList {
    fn from(listing: &str) -> Self {
        let targets = listing
            .lines()
            .filter_map(|line| {
                let mut columns = line.split_whitespace();
                let kind = columns.next()?;
                let device = columns.next()?;
                let target = columns.next()?;
                if (kind, device, target) == ("Type", "Device", "Target") {
                    return None;
                }
                Some(target.to_owned())
            })
            .collect();
        Self { targets }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_targets_without_interpreting_headers_or_media_sources_as_devices() {
        let listing = " Type   Device  Target  Source\n--------------------------------------\n file   cdrom   sdb     /tmp/install image.iso\n file   disk    vda     /tmp/sdc\n file   cdrom   sdc     -\n";
        assert_eq!(
            BlockList::from(listing).targets,
            ["sdb", "vda", "sdc"].map(str::to_owned).into()
        );
        assert!(BlockList::from("").targets.is_empty());
    }
}
