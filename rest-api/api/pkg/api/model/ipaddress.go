// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"

// APIAddressFamily identifies the IP version of an address.
type APIAddressFamily string

const (
	APIAddressFamilyIPv4        APIAddressFamily = "IPv4"
	APIAddressFamilyIPv6        APIAddressFamily = "IPv6"
	APIAddressFamilyUnspecified APIAddressFamily = "Unspecified"
)

// fromCoreName converts the Core enum name
func (f *APIAddressFamily) fromCoreName(name string) {
	switch name {
	case corev1.AddressFamily_ADDRESS_FAMILY_V4.String():
		*f = APIAddressFamilyIPv4
	case corev1.AddressFamily_ADDRESS_FAMILY_V6.String():
		*f = APIAddressFamilyIPv6
	default:
		*f = APIAddressFamilyUnspecified
	}
}

// APIIPAddress is an IP address and its address family.
type APIIPAddress struct {
	AddressFamily APIAddressFamily `json:"addressFamily"`
	Address       string           `json:"address"`
}
