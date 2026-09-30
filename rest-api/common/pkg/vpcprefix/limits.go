// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package vpcprefix defines the allocation limits shared by the REST API and
// nicocli when they create a VPC Prefix. The values mirror Core's allocation
// policy in crates/api-model/src/vpc/mod.rs.
package vpcprefix

// IPFamily identifies the address family of the IP Block used by a VPC Prefix.
type IPFamily string

const (
	// IPFamilyIPv4 identifies an IPv4 IP Block.
	IPFamilyIPv4 IPFamily = "IPv4"
	// IPFamilyIPv6 identifies an IPv6 IP Block.
	IPFamilyIPv6 IPFamily = "IPv6"

	// PrefixLengthMinimum is the smallest VPC Prefix accepted by the REST API.
	PrefixLengthMinimum = 8
	// IPv4PrefixLengthMaximum is the `/31` limit that Core permits when the
	// VPC Prefix and Interface prefix have the same length.
	IPv4PrefixLengthMaximum = 31
	// IPv6SLAACPrefixLengthMaximum leaves room for Core to allocate a `/64`
	// Interface prefix from the VPC Prefix.
	IPv6SLAACPrefixLengthMaximum = 63
	// IPv6StatefulPrefixLengthMaximum leaves room for Core to allocate a `/127`
	// Interface prefix from the VPC Prefix. A `/127` VPC Prefix has no room for
	// a `/127` child, so `/126` is the largest usable parent.
	IPv6StatefulPrefixLengthMaximum = 126
	// PrefixLengthMaximum is the structural request limit before the IP Block
	// family and VPC address mode are known.
	PrefixLengthMaximum = IPv6StatefulPrefixLengthMaximum
)

// MaximumPrefixLength returns the VPC Prefix limit for an IP family and VPC
// address mode. Unknown families use the structural request limit and return
// false so API callers can reject them while nicocli preserves manual ID entry.
func (family IPFamily) MaximumPrefixLength(slaacEnabled bool) (int, bool) {
	switch family {
	case IPFamilyIPv4:
		return IPv4PrefixLengthMaximum, true
	case IPFamilyIPv6:
		if slaacEnabled {
			return IPv6SLAACPrefixLengthMaximum, true
		}
		return IPv6StatefulPrefixLengthMaximum, true
	default:
		return PrefixLengthMaximum, false
	}
}
