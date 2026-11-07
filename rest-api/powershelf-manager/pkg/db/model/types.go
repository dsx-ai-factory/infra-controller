// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"database/sql/driver"
	"fmt"
	"net"
)

// MacAddr wraps net.HardwareAddr to provide proper SQL driver support for PostgreSQL macaddr type.
type MacAddr net.HardwareAddr

// Value implements driver.Valuer for MacAddr.
// Converts the MAC address to a string format that PostgreSQL's macaddr type expects.
func (m MacAddr) Value() (driver.Value, error) {
	if m == nil {
		return nil, nil
	}
	return net.HardwareAddr(m).String(), nil
}

// Scan implements sql.Scanner for MacAddr.
// Handles both string and []byte inputs from PostgreSQL.
func (m *MacAddr) Scan(src interface{}) error {
	if src == nil {
		*m = nil
		return nil
	}

	var macStr string
	switch v := src.(type) {
	case string:
		macStr = v
	case []byte:
		macStr = string(v)
	default:
		return fmt.Errorf("cannot scan %T into MacAddr", src)
	}

	mac, err := net.ParseMAC(macStr)
	if err != nil {
		return fmt.Errorf("failed to parse MAC address %q: %w", macStr, err)
	}
	*m = MacAddr(mac)
	return nil
}

// HardwareAddr returns the underlying net.HardwareAddr.
func (m MacAddr) HardwareAddr() net.HardwareAddr {
	return net.HardwareAddr(m)
}

// String returns the MAC address as a string.
func (m MacAddr) String() string {
	return net.HardwareAddr(m).String()
}
