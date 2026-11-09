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

use std::time::Duration;

/// Formats elapsed time with milliseconds below one second and whole seconds
/// otherwise, using compact units and zero-padded minute/second remainders.
pub fn format_duration_compact(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 1 {
        format!("{}ms", duration.as_millis())
    } else if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!(
            "{}h {:02}m {:02}s",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        )
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn compact_durations() {
        value_scenarios!(
            run = |millis| format_duration_compact(Duration::from_millis(millis));
            "milliseconds and seconds" {
                0 => "0ms".to_string(),
                999 => "999ms".to_string(),
                1000 => "1s".to_string(),
                59_999 => "59s".to_string(),
            }
            "minutes" {
                60_000 => "1m 00s".to_string(),
                65_000 => "1m 05s".to_string(),
                3_599_999 => "59m 59s".to_string(),
            }
            "hours" {
                3_600_000 => "1h 00m 00s".to_string(),
                3_661_000 => "1h 01m 01s".to_string(),
                90_061_000 => "25h 01m 01s".to_string(),
            }
        );
    }
}
