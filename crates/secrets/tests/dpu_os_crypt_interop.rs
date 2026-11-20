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

#![cfg(target_os = "linux")]

use std::process::Stdio;
use std::time::Duration;

use carbide_secrets::MemoryCredentialStore;
use carbide_secrets::credentials::{CredentialKey, CredentialWriter, Credentials};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[tokio::test]
async fn generated_password_hashes_match_linux_crypt() {
    let mut cases = Vec::new();
    for (name, password) in [
        ("ASCII", "mynewpassword".to_string()),
        ("leading and trailing spaces", " mynewpassword ".to_string()),
        ("511 ASCII bytes", "a".repeat(511)),
        ("511 UTF-8 bytes", format!("{}a", "€".repeat(170))),
    ] {
        let store = MemoryCredentialStore::default();
        store
            .create_credentials(
                &CredentialKey::DpuOs,
                &Credentials::new("ubuntu", &password),
            )
            .await
            .expect(name);
        let hash = carbide_secrets::dpu_os::password_hash(&store)
            .await
            .expect(name)
            .expect("the credential has an installer hash");
        cases.push(serde_json::json!({"name": name, "password": password, "hash": hash}));
    }

    // Use the host's crypt library independently of the Rust hash implementation.
    // ctypes also works on Python versions that no longer provide the crypt module.
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(
            r#"
import ctypes
import ctypes.util
import json
import sys

library = ctypes.util.find_library("crypt")
if not library:
    raise SystemExit("Linux libcrypt is required for this test")
crypt = ctypes.CDLL(library).crypt
crypt.argtypes = [ctypes.c_char_p, ctypes.c_char_p]
crypt.restype = ctypes.c_char_p
for case in json.load(sys.stdin.buffer):
    stored = case["hash"].encode("ascii")
    actual = crypt(case["password"].encode("utf-8"), stored)
    if actual != stored:
        raise SystemExit("libcrypt hash mismatch: " + case["name"])
    if crypt(b"incorrect-fixture-password", stored) == stored:
        raise SystemExit("libcrypt accepted a wrong password: " + case["name"])
    print("PASS: " + case["name"])
"#,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("start Python 3 for the Linux crypt comparison");
    let mut stdin = child.stdin.take().expect("the input pipe is available");
    stdin
        .write_all(&serde_json::to_vec(&cases).unwrap())
        .await
        .expect("send the fixture cases to Python");
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .expect("the Linux crypt comparison must finish within 30 seconds")
        .expect("wait for the Linux crypt comparison");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    print!("{}", String::from_utf8_lossy(&output.stdout));
}
