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

use rcgen::{CertifiedKey, generate_simple_self_signed};

/// Creates a DER-encoded self-signed certificate for test fixtures.
pub fn create_random_self_signed_cert() -> Vec<u8> {
    let subject_alt_names = vec!["hello.world.example".to_string(), "localhost".to_string()];

    let CertifiedKey { cert, .. } = generate_simple_self_signed(subject_alt_names).expect(
        "BUG: Keypair generation should not fail, subject alt names are static and must be valid",
    );

    cert.der().to_vec()
}
