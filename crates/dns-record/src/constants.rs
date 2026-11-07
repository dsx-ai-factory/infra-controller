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

//! DNS record type constants
//!
//! This module defines constants for DNS record types and their numeric codes
//! as specified in RFC 1035 and related RFCs.

// DNS record type string constants
pub const DNS_TYPE_SOA: &str = "SOA";
pub const DNS_TYPE_NS: &str = "NS";
pub const DNS_TYPE_A: &str = "A";
pub const DNS_TYPE_AAAA: &str = "AAAA";
pub const DNS_TYPE_CNAME: &str = "CNAME";
pub const DNS_TYPE_MX: &str = "MX";
pub const DNS_TYPE_TXT: &str = "TXT";
pub const DNS_TYPE_PTR: &str = "PTR";
pub const DNS_TYPE_SRV: &str = "SRV";
pub const DNS_TYPE_ANY: &str = "ANY";

// DNS QTYPE numeric codes from RFC 1035
pub const DNS_QTYPE_A: u16 = 1;
pub const DNS_QTYPE_NS: u16 = 2;
pub const DNS_QTYPE_CNAME: u16 = 5;
pub const DNS_QTYPE_SOA: u16 = 6;
pub const DNS_QTYPE_PTR: u16 = 12;
pub const DNS_QTYPE_MX: u16 = 15;
pub const DNS_QTYPE_TXT: u16 = 16;
pub const DNS_QTYPE_AAAA: u16 = 28;
pub const DNS_QTYPE_SRV: u16 = 33;
pub const DNS_QTYPE_ANY: u16 = 255;
