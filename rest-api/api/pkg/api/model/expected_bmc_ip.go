// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"net"
)

// validateExpectedBmcIPAddress rejects addresses that cannot identify one BMC.
// Other syntactically valid addresses retain the existing API behavior.
func validateExpectedBmcIPAddress(value interface{}) error {
	var address string
	switch ip := value.(type) {
	case string:
		address = ip
	case *string:
		if ip == nil {
			return nil
		}
		address = *ip
	default:
		return errors.New("BmcIpAddress must be a valid IPv4 or IPv6 address that is not unspecified, multicast, or limited broadcast")
	}

	parsed := net.ParseIP(address)
	if parsed == nil || parsed.IsUnspecified() || parsed.IsMulticast() || parsed.Equal(net.IPv4bcast) {
		return errors.New("BmcIpAddress must be a valid IPv4 or IPv6 address that is not unspecified, multicast, or limited broadcast")
	}
	return nil
}
