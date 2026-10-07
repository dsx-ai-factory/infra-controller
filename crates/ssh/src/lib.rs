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

pub mod ssh;
pub mod ssh_client;

#[derive(thiserror::Error, Debug)]
pub enum SshError {
    #[error("SSH error: {0}")]
    Russh(#[from] russh::Error),
    #[error("error reading SSH keys: {0}")]
    SshKey(String),
    #[error("SSH authentication failed: {0}")]
    AuthenticationFailed(String),
    #[error("the executed command did not send an exit status")]
    CommandDidNotExit,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("error running SSH command: {0}")]
    Command(String),
}

pub type SshResult<T> = Result<T, SshError>;
