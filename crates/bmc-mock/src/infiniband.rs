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

use std::fmt;

/// An InfiniBand port GUID in network byte order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Guid([u8; 8]);

impl From<[u8; 8]> for Guid {
    fn from(bytes: [u8; 8]) -> Self {
        Self(bytes)
    }
}

impl From<Guid> for [u8; 8] {
    fn from(guid: Guid) -> Self {
        guid.0
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::Guid;

    #[test]
    fn display_is_normalized_hexadecimal() {
        check_values(
            [
                Check {
                    scenario: "leading zeroes",
                    input: Guid::from([0, 0, 0, 0, 0, 0, 0, 1]),
                    expect: "0000000000000001".to_string(),
                },
                Check {
                    scenario: "lowercase hexadecimal",
                    input: Guid::from([0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10]),
                    expect: "fedcba9876543210".to_string(),
                },
            ],
            |guid| guid.to_string(),
        );
    }
}
