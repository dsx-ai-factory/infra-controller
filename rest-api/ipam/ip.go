/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"net/netip"
)

// IP is a single ipaddress.
type IP struct {
	IP           netip.Addr
	ParentPrefix string
	Namespace    string
}
