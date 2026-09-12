-- SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
-- SPDX-License-Identifier: Apache-2.0

-- Support case-insensitive lookups without rewriting existing BMC identities.
CREATE INDEX bmc_mac_address_lower_idx ON bmc (lower(mac_address));
