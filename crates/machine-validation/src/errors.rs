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
#[derive(thiserror::Error, Debug)]
pub enum MachineValidationError {
    #[error("machine validation: {0}")]
    Generic(String),
    #[error("unable to config read: {0}")]
    ConfigFileRead(String),
    #[error("yaml parse error: {0}")]
    Parse(String),
    #[error("{0}: {1}")]
    File(String, String),
    #[error("failed {0}: {1}")]
    ApiClient(String, String),
}
