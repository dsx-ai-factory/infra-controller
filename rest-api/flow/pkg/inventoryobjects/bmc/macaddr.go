// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bmc

import "net"

// MACAddress wraps net.HardwareAddr and implements encoding.TextMarshaler and
// encoding.TextUnmarshaler so that JSON serialization produces the human-readable
// colon-separated MAC address string (e.g. "aa:bb:cc:dd:ee:ff") rather than a
// base64-encoded byte array.
type MACAddress struct {
	net.HardwareAddr
}

// MarshalText implements encoding.TextMarshaler.
func (m MACAddress) MarshalText() ([]byte, error) {
	return []byte(m.String()), nil
}

// UnmarshalText implements encoding.TextUnmarshaler.
func (m *MACAddress) UnmarshalText(text []byte) error {
	if len(text) == 0 {
		m.HardwareAddr = nil
		return nil
	}

	addr, err := net.ParseMAC(string(text))
	if err != nil {
		return err
	}

	m.HardwareAddr = addr
	return nil
}
