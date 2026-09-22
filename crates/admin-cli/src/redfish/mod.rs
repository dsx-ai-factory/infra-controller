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

// `redfish` is intentionally OUTSIDE the `Dispatch`/`Run` trait flow: it talks
// straight to a machine's BMC, so `main` handles it *before* the API client
// (and thus `RuntimeContext`) is built -- see the `CliCommand::Redfish` branch
// in `main.rs`, which calls `redfish::action` directly. Those traits carry a
// `RuntimeContext` redfish never has, so it stays a plain `action` fn rather
// than implementing them. Please don't "realign" it onto the traits.
mod args;
mod cmds;

pub(crate) use args::RedfishAction;
pub(crate) use cmds::action;
