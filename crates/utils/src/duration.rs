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

/// Formats elapsed time rounded to the nearest millisecond below one second,
/// or nearest whole second otherwise, with halfway values rounded up.
/// Carries into larger units and zero-pads minute/second remainders.
pub fn format_duration_compact(duration: Duration) -> String {
    let milliseconds = (duration.as_nanos() + 500_000) / 1_000_000;
    if milliseconds < 1000 {
        return format!("{milliseconds}ms");
    }
    let seconds = (duration.as_millis() + 500) / 1000;
    if seconds < 60 {
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
            run = format_duration_compact;
            "milliseconds" {
                Duration::ZERO => "0ms".to_string(),
                Duration::from_micros(499) => "0ms".to_string(),
                Duration::from_micros(500) => "1ms".to_string(),
                Duration::from_micros(999_499) => "999ms".to_string(),
                Duration::from_micros(999_500) => "1s".to_string(),
            }
            "seconds" {
                Duration::from_micros(1_499_999) => "1s".to_string(),
                Duration::from_millis(1500) => "2s".to_string(),
            }
            "minutes" {
                Duration::from_millis(59_499) => "59s".to_string(),
                Duration::from_millis(59_500) => "1m 00s".to_string(),
                Duration::from_millis(65_000) => "1m 05s".to_string(),
            }
            "hours" {
                Duration::from_millis(3_599_499) => "59m 59s".to_string(),
                Duration::from_millis(3_599_500) => "1h 00m 00s".to_string(),
                Duration::from_millis(3_599_999) => "1h 00m 00s".to_string(),
                Duration::from_millis(3_661_000) => "1h 01m 01s".to_string(),
                Duration::from_millis(90_061_000) => "25h 01m 01s".to_string(),
            }
        );
    }
}
