// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sqltypes

import (
	"database/sql/driver"
	"fmt"
	"net"
	"strings"
)

// IPAddr wraps net.IP to provide SQL driver support for PostgreSQL inet values.
type IPAddr net.IP

// Value implements driver.Valuer for IPAddr.
// It converts the IP address to the string format PostgreSQL's inet type expects.
func (ip IPAddr) Value() (driver.Value, error) {
	if ip == nil {
		return nil, nil
	}
	return net.IP(ip).String(), nil
}

// Scan implements sql.Scanner for IPAddr.
// It handles both string and []byte inputs from PostgreSQL.
func (ip *IPAddr) Scan(src interface{}) error {
	if src == nil {
		*ip = nil
		return nil
	}

	var ipStr string
	switch v := src.(type) {
	case string:
		ipStr = v
	case []byte:
		ipStr = string(v)
	default:
		return fmt.Errorf("cannot scan %T into IPAddr", src)
	}

	// PostgreSQL inet values may include CIDR notation. Strip it if present.
	if i := strings.LastIndexByte(ipStr, '/'); i >= 0 {
		ipStr = ipStr[:i]
	}

	parsed := net.ParseIP(ipStr)
	if parsed == nil {
		return fmt.Errorf("failed to parse IP address %q", ipStr)
	}
	*ip = IPAddr(parsed)
	return nil
}

// IP returns the underlying net.IP.
func (ip IPAddr) IP() net.IP {
	return net.IP(ip)
}

// String returns the IP address as a string.
func (ip IPAddr) String() string {
	return net.IP(ip).String()
}

// Equal returns true if the two IPAddr values are equal.
func (ip IPAddr) Equal(other IPAddr) bool {
	return net.IP(ip).Equal(net.IP(other))
}
