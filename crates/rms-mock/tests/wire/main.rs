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

//! Wire-level tests.
//!
//! These drive the mock through a real gRPC client over a real socket,
//! rather than calling the trait methods directly, because the thing most
//! likely to break is the transport: codec, HTTP/2 framing, and the router
//! paths the services are mounted on. A test that called the impl directly
//! would pass even if nothing were reachable. One binary holds every module,
//! so each fixture in `common` has a caller.

mod common;
mod firmware;
mod grpc;
mod lifecycle;
mod nvos;
