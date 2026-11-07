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
#![no_main]
use libfuzzer_sys::fuzz_target;
use ssh_console::{EscapeSequence, IPMITOOL_ESCAPE_SEQUENCE};

fuzz_target!(|data: &[u8]| {
    static SINGLE_SEQUENCE: u8 = 0x1b;
    static PAIR_SEQUENCE: (u8, &[u8]) = (0x1b, &[0x28]);
    static IPMITOOL_SEQUENCE_TRAILS: &[u8] = &[b'.', b'B', b'?', 0x1a, 0x18];
    assert!(
        !EscapeSequence::Single(SINGLE_SEQUENCE)
            .filter_escape_sequences(data, false)
            .0
            .contains(&SINGLE_SEQUENCE)
    );

    for result in [
        // Pair, no pending
        EscapeSequence::Pair(PAIR_SEQUENCE).filter_escape_sequences(data, false),
        // Pair, with pending byte from last chunk
        EscapeSequence::Pair(PAIR_SEQUENCE).filter_escape_sequences(data, true),
    ] {
        assert!(
            !result
                .0
                .windows(2)
                .any(|w| w[0] == PAIR_SEQUENCE.0 && w[1] == PAIR_SEQUENCE.1[0])
        );
    }

    for result in [
        // Pair, no pending
        IPMITOOL_ESCAPE_SEQUENCE.filter_escape_sequences(data, false),
        // Pair, with pending byte from last chunk
        IPMITOOL_ESCAPE_SEQUENCE.filter_escape_sequences(data, true),
    ] {
        for &trailing in IPMITOOL_SEQUENCE_TRAILS {
            assert!(
                !result
                    .0
                    .windows(2)
                    .any(|w| w[0] == b'~' && w[1] == trailing)
            );
        }
    }
});
