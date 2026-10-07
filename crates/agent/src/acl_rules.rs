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

/// Path to the legacy ETV ACL rules file (used by cleanup_old_acls)
pub(super) const PATH: &str = "etc/cumulus/acl/policy.d/60-forge.rules";

/// Command to reload ACL rules
pub(super) const RELOAD_CMD: &str = "cl-acltool -i";

/// ACL to suppress ARP packets before encapsulation
pub(super) const ARP_SUPPRESSION_RULE: &str = r"
[ebtables]
# Suppress ARP packets before they get encapsulated.
-A OUTPUT -o vxlan48 -p ARP -j DROP
";
