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

//! Redaction of every secret the run knows from text bound for the terminal.

use std::sync::Mutex;

const REDACTED: &str = "********";

pub(crate) struct Redactor {
    secrets: Mutex<Vec<String>>,
}

impl Redactor {
    pub(crate) fn new<'a>(secrets: impl IntoIterator<Item = &'a str>) -> Self {
        let redactor = Self {
            secrets: Mutex::new(Vec::new()),
        };
        for secret in secrets {
            redactor.add(secret);
        }
        redactor
    }

    pub(crate) fn add(&self, secret: &str) {
        if !secret.is_empty() {
            self.secrets
                .lock()
                .expect("secrets lock")
                .push(secret.to_string());
        }
    }

    pub(crate) fn apply(&self, text: &str) -> String {
        self.secrets
            .lock()
            .expect("secrets lock")
            .iter()
            .fold(text.to_string(), |text, secret| {
                text.replace(secret, REDACTED)
            })
    }
}
