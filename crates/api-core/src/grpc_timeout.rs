// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

/// Parses one to eight ASCII digits followed by a standard gRPC timeout unit.
/// Callers choose their own policy for absent or malformed metadata.
pub(crate) fn parse_grpc_timeout(value: &str) -> Option<Duration> {
    let (&unit, digits) = value.as_bytes().split_last()?;
    if digits.is_empty() || digits.len() > 8 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let count = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    Some(match unit {
        b'H' => Duration::from_secs(count * 3600),
        b'M' => Duration::from_secs(count * 60),
        b'S' => Duration::from_secs(count),
        b'm' => Duration::from_millis(count),
        b'u' => Duration::from_micros(count),
        b'n' => Duration::from_nanos(count),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn grpc_timeout_parsing() {
        value_scenarios!(
            run = |header: &str| parse_grpc_timeout(header);
            "wire units and digit boundaries" {
                "99999999H" => Some(Duration::from_secs(99_999_999 * 3600)),
                "2M" => Some(Duration::from_secs(120)),
                "1S" => Some(Duration::from_secs(1)),
                "500m" => Some(Duration::from_millis(500)),
                "1u" => Some(Duration::from_micros(1)),
                "1n" => Some(Duration::from_nanos(1)),
                "0n" => Some(Duration::ZERO),
            }
            "malformed grammar" {
                "" => None,
                "S" => None,
                "+1S" => None,
                "000000001S" => None,
                "1X" => None,
                "1é" => None,
            }
        );
    }
}
