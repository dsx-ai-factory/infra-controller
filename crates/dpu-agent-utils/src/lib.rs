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
pub mod machine_identity;
pub mod utils;

/// `FMDS_IPV6_ADDRESS` is the tenant metadata endpoint shared by the embedded
/// agent and standalone FMDS service.
pub const FMDS_IPV6_ADDRESS: std::net::Ipv6Addr =
    std::net::Ipv6Addr::new(0xfd00, 0x76b9, 0, 0, 0, 0, 0, 0x254);

/// `FMDS_IPV6_GATEWAY` is HBN's peer on the metadata `/127`, used for replies
/// to tenant networks.
pub const FMDS_IPV6_GATEWAY: std::net::Ipv6Addr =
    std::net::Ipv6Addr::new(0xfd00, 0x76b9, 0, 0, 0, 0, 0, 0x255);
