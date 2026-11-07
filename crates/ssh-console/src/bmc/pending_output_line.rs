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

/// A pending output line is data we received from a BMC after the last newline character, and is
/// useful to relay to the user after they've connected so they can see what the latest output was.
pub(super) struct PendingOutputLine(Vec<u8>);

impl PendingOutputLine {
    pub(super) fn with_max_size(max_size: usize) -> Self {
        Self(Vec::with_capacity(max_size))
    }

    pub(super) fn extend(&mut self, data: &[u8]) {
        // If there's a newline in the data, start from there
        let newline_index = if let Some(newline_index) = data.iter().position(|b| *b == b'\n') {
            self.0.clear();
            newline_index
        } else {
            0
        };

        // If the amount of data after the newline is more than our capacity, take only the last `capacity` bytes
        let begin_index = if data.len() - newline_index > self.0.capacity() {
            data.len() - self.0.capacity()
        } else {
            newline_index
        };

        // If this data puts us over the edge, clear first. The remaining data will be at most `capacity` bytes per above.
        let slice = &data[begin_index..];
        if self.0.len() + slice.len() > self.0.capacity() {
            self.0.clear();
        }
        self.0.extend(slice);
    }

    pub(super) fn get(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty() {
        let p = PendingOutputLine::with_max_size(8);
        assert_eq!(p.get(), b"");
    }

    #[test]
    fn append_without_newline_under_capacity() {
        let mut p = PendingOutputLine::with_max_size(8);
        p.extend(b"abc");
        assert_eq!(p.get(), b"abc");
        p.extend(b"de");
        // total 5 bytes, still under capacity, no newline => keeps growing
        assert_eq!(p.get(), b"abcde");
    }

    #[test]
    fn append_without_newline_over_capacity_single_call_keeps_tail() {
        let mut p = PendingOutputLine::with_max_size(5);
        p.extend(b"abcdefgh"); // no newline => keep last capacity bytes
        assert_eq!(p.get(), b"defgh");
    }

    #[test]
    fn append_without_newline_over_capacity_across_calls_clears_then_appends() {
        let mut p = PendingOutputLine::with_max_size(5);
        p.extend(b"abcde");
        assert_eq!(p.get(), b"abcde");
        // next chunk would overflow (5 + 3 > 5) so buffer is cleared before extend
        p.extend(b"fgh");
        assert_eq!(p.get(), b"fgh");
    }

    #[test]
    fn newline_resets_and_keeps_from_newline_inclusive() {
        let mut p = PendingOutputLine::with_max_size(10);
        p.extend(b"hello");
        assert_eq!(p.get(), b"hello");
        p.extend(b"world\nagain");
        // On seeing the newline, buffer cleared and we keep from the newline position.
        // Note: current implementation keeps the newline itself.
        assert_eq!(p.get(), b"\nagain");
    }

    #[test]
    fn newline_with_tail_exceeding_capacity_keeps_last_capacity_bytes() {
        let mut p = PendingOutputLine::with_max_size(4);
        p.extend(b"abc\ndefghi"); // tail after '\n' is "defghi" (6 bytes) > capacity
        // Should keep only the last 4 bytes of the whole slice: "fghi"
        assert_eq!(p.get(), b"fghi");
    }

    #[test]
    fn newline_at_start_included_and_truncated_if_needed() {
        let mut p = PendingOutputLine::with_max_size(3);
        p.extend(b"\nxyz"); // newline at index 0 => clear, then consider tail "xyz"
        assert_eq!(p.get(), b"xyz"); // exactly capacity
        p.extend(b"\nabcdef"); // tail "abcdef" (6) > cap(3) -> keep "def"
        assert_eq!(p.get(), b"def");
    }

    #[test]
    fn multiple_newlines_uses_first_newline_position() {
        let mut p = PendingOutputLine::with_max_size(32);
        p.extend(b"start");
        p.extend(b"x\ny\nz");
        // First newline at 'x\n', so keep from that first '\n':
        assert_eq!(p.get(), b"\ny\nz");
    }

    #[test]
    fn never_exceeds_capacity() {
        let mut p = PendingOutputLine::with_max_size(5);
        // hammer with variable-sized chunks
        for chunk in [
            b"12".as_slice(),
            b"345".as_slice(),
            b"6789".as_slice(),
            b"a".as_slice(),
            b"bcdefgh".as_slice(),
        ]
        .iter()
        {
            p.extend(chunk);
            assert!(
                p.get().len() <= 5,
                "len={} > 5 after chunk {:?}",
                p.get().len(),
                chunk
            );
        }
    }
}
