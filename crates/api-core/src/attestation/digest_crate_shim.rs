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

use rsa::signature::digest::{Output, Reset};

/// Sha256LegacyDigestShim provides an implementation of the version of the `Digest` trait the `rsa`
/// crate expects, by delegating out to the version of the `Digest` trait the `sha2` crate expects.
///
/// The main thing we have to do is convert between the `Output` representations in each crate,
/// in this case by laundering it through a `&mut [u8;32]` to convert between the representations.
#[derive(Clone)]
pub(super) struct Sha256LegacyDigestShim(sha2::Sha256);

impl rsa::signature::digest::OutputSizeUser for Sha256LegacyDigestShim {
    type OutputSize = <sha2::Sha256 as sha2::digest::OutputSizeUser>::OutputSize;
}

impl rsa::signature::digest::FixedOutput for Sha256LegacyDigestShim {
    fn finalize_into(self, out: &mut Output<Self>) {
        let arr: &mut [u8; 32] = out
            .as_mut_slice()
            .try_into()
            .expect("unexpected output length");
        sha2::digest::FixedOutput::finalize_into(self.0, arr.into())
    }
}

impl rsa::signature::digest::Update for Sha256LegacyDigestShim {
    fn update(&mut self, data: &[u8]) {
        sha2::digest::Update::update(&mut self.0, data)
    }
}

impl rsa::signature::digest::Reset for Sha256LegacyDigestShim {
    fn reset(&mut self) {
        sha2::digest::Reset::reset(&mut self.0)
    }
}

impl rsa::signature::digest::FixedOutputReset for Sha256LegacyDigestShim {
    fn finalize_into_reset(&mut self, out: &mut rsa::signature::digest::Output<Self>) {
        let slice: &mut [u8; 32] = out
            .as_mut_slice()
            .try_into()
            .expect("unexpected output length");
        sha2::digest::FixedOutputReset::finalize_into_reset(&mut self.0, slice.into())
    }
}

impl rsa::signature::digest::Digest for Sha256LegacyDigestShim {
    fn new() -> Self {
        Self(sha2::Sha256::default())
    }

    fn new_with_prefix(data: impl AsRef<[u8]>) -> Self {
        Self(sha2::digest::Digest::new_with_prefix(data))
    }

    fn update(&mut self, data: impl AsRef<[u8]>) {
        sha2::digest::Digest::update(&mut self.0, data)
    }

    fn chain_update(self, data: impl AsRef<[u8]>) -> Self {
        Self(sha2::digest::Digest::chain_update(self.0, data))
    }

    fn finalize(self) -> Output<Self> {
        sha2::digest::Digest::finalize(self.0).0.into()
    }

    fn finalize_into(self, out: &mut Output<Self>) {
        // Delegate to FixedOutput impl above
        <Self as rsa::signature::digest::FixedOutput>::finalize_into(self, out)
    }

    fn finalize_reset(&mut self) -> Output<Self> {
        sha2::digest::Digest::finalize_reset(&mut self.0).0.into()
    }

    fn finalize_into_reset(&mut self, out: &mut Output<Self>) {
        // Delegate to FixedOutputReset impl above
        <Self as rsa::signature::digest::FixedOutputReset>::finalize_into_reset(self, out)
    }

    fn reset(&mut self)
    where
        Self: Reset,
    {
        // Delegate to Reset impl above
        <Self as rsa::signature::digest::Reset>::reset(self)
    }

    fn output_size() -> usize {
        <sha2::Sha256 as sha2::digest::Digest>::output_size()
    }

    fn digest(data: impl AsRef<[u8]>) -> Output<Self> {
        let sha2_output = <sha2::Sha256 as sha2::digest::Digest>::digest(data);
        sha2_output.0.into()
    }
}
