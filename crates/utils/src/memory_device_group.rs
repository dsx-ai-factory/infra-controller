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

/// A memory-device-group-shaped type: an optional DIMM size, an optional DIMM type, and a device
/// count. Implemented for both the API/DB model's `MemoryDeviceGroup` and the RPC proto's
/// `MemoryDeviceGroup`, which are structurally identical but live in crates that can't depend on
/// each other (the RPC crate only depends on the model crate behind an optional feature), so
/// [`condense_memory_device_groups`] can run the same algorithm on either without duplicating it.
pub trait MemoryDeviceGroupLike {
    fn size_mb(&self) -> Option<u32>;
    fn mem_type(&self) -> &Option<String>;
    fn count(&self) -> u32;
    fn add_count(&mut self, extra: u32);
}

/// Rolls up a sequence of memory device groups (or count-1 groups derived from a flat device
/// list), merging consecutive groups with the same `(size_mb, mem_type)` and dropping zero-count
/// groups. Fails via `on_exceeded` once the aggregate count exceeds `max_total`, so a corrupted or
/// malicious input can't produce an unbounded number of groups/devices downstream.
///
/// This is the single point through which every path that produces condensed memory device groups
/// (the API/DB model, RPC-to-model conversion, on-host SMBIOS enumeration) must pass, so the merge
/// and bound-check logic can't drift between call sites.
pub fn condense_memory_device_groups<T, E>(
    groups: impl IntoIterator<Item = T>,
    max_total: u32,
    on_exceeded: impl FnOnce(u64) -> E,
) -> Result<Vec<T>, E>
where
    T: MemoryDeviceGroupLike,
{
    let mut merged: Vec<T> = Vec::new();
    let mut total: u64 = 0;
    for group in groups.into_iter().filter(|group| group.count() > 0) {
        total += u64::from(group.count());
        if total > u64::from(max_total) {
            return Err(on_exceeded(total));
        }
        match merged.last_mut() {
            Some(last)
                if last.size_mb() == group.size_mb() && last.mem_type() == group.mem_type() =>
            {
                last.add_count(group.count());
            }
            _ => merged.push(group),
        }
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct TestGroup {
        size_mb: Option<u32>,
        mem_type: Option<String>,
        count: u32,
    }

    impl MemoryDeviceGroupLike for TestGroup {
        fn size_mb(&self) -> Option<u32> {
            self.size_mb
        }
        fn mem_type(&self) -> &Option<String> {
            &self.mem_type
        }
        fn count(&self) -> u32 {
            self.count
        }
        fn add_count(&mut self, extra: u32) {
            self.count = self.count.saturating_add(extra);
        }
    }

    fn group(size_mb: u32, mem_type: &str, count: u32) -> TestGroup {
        TestGroup {
            size_mb: Some(size_mb),
            mem_type: Some(mem_type.to_string()),
            count,
        }
    }

    #[test]
    fn merges_consecutive_identical_groups() {
        let groups = vec![group(16384, "DDR5", 1), group(16384, "DDR5", 2)];
        let merged = condense_memory_device_groups(groups, 8192, |total| total).unwrap();
        assert_eq!(merged, vec![group(16384, "DDR5", 3)]);
    }

    #[test]
    fn preserves_order_of_non_adjacent_groups() {
        let groups = vec![
            group(16384, "DDR5", 1),
            group(32768, "DDR4", 1),
            group(16384, "DDR5", 1),
        ];
        let merged = condense_memory_device_groups(groups.clone(), 8192, |total| total).unwrap();
        assert_eq!(merged, groups);
    }

    #[test]
    fn drops_zero_count_groups() {
        let groups = vec![group(16384, "DDR5", 0), group(32768, "DDR4", 1)];
        let merged = condense_memory_device_groups(groups, 8192, |total| total).unwrap();
        assert_eq!(merged, vec![group(32768, "DDR4", 1)]);
    }

    #[test]
    fn rejects_aggregate_count_above_max() {
        let groups = vec![group(16384, "DDR5", 5), group(32768, "DDR4", 5)];
        let err = condense_memory_device_groups(groups, 8, |total| total).unwrap_err();
        assert_eq!(err, 10);
    }

    #[test]
    fn accepts_aggregate_count_equal_to_max() {
        let groups = vec![group(16384, "DDR5", 5), group(32768, "DDR4", 5)];
        let merged = condense_memory_device_groups(groups.clone(), 10, |total| total).unwrap();
        assert_eq!(merged, groups);
    }
}
