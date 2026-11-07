// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package bmc

import (
	"encoding/json"
	"net"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestMACAddress_MarshalText(t *testing.T) {
	tests := []struct {
		name string
		mac  string
		want string
	}{
		{"lowercase colon-separated", "aa:bb:cc:dd:ee:ff", "aa:bb:cc:dd:ee:ff"},
		{"uppercase colon-separated", "AA:BB:CC:DD:EE:FF", "aa:bb:cc:dd:ee:ff"},
		{"zeros", "00:00:00:00:00:00", "00:00:00:00:00:00"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			hw, err := net.ParseMAC(tt.mac)
			require.NoError(t, err)

			got, err := MACAddress{HardwareAddr: hw}.MarshalText()
			require.NoError(t, err)
			assert.Equal(t, tt.want, string(got))
		})
	}

	t.Run("nil HardwareAddr", func(t *testing.T) {
		got, err := MACAddress{HardwareAddr: nil}.MarshalText()
		require.NoError(t, err)
		assert.Equal(t, "", string(got))
	})
}

func TestMACAddress_UnmarshalText(t *testing.T) {
	t.Run("valid", func(t *testing.T) {
		var m MACAddress
		require.NoError(t, m.UnmarshalText([]byte("aa:bb:cc:dd:ee:ff")))
		assert.Equal(t, net.HardwareAddr{0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff}, m.HardwareAddr)
	})

	t.Run("invalid", func(t *testing.T) {
		var m MACAddress
		assert.Error(t, m.UnmarshalText([]byte("not-a-mac")))
	})

	t.Run("empty []byte{}", func(t *testing.T) {
		var m MACAddress
		require.NoError(t, m.UnmarshalText([]byte{}))
		assert.Nil(t, m.HardwareAddr)
	})

	t.Run(`empty []byte("")`, func(t *testing.T) {
		var m MACAddress
		require.NoError(t, m.UnmarshalText([]byte("")))
		assert.Nil(t, m.HardwareAddr)
	})
}

func TestMACAddress_JSONMarshal(t *testing.T) {
	hw, err := net.ParseMAC("aa:bb:cc:dd:ee:ff")
	require.NoError(t, err)

	tests := []struct {
		name string
		mac  MACAddress
		want string
	}{
		{"valid", MACAddress{HardwareAddr: hw}, `"aa:bb:cc:dd:ee:ff"`},
		{"nil HardwareAddr", MACAddress{HardwareAddr: nil}, `""`},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			data, err := json.Marshal(tt.mac)
			require.NoError(t, err)
			assert.Equal(t, tt.want, string(data))
		})
	}
}

func TestMACAddress_JSONUnmarshal(t *testing.T) {
	hw := net.HardwareAddr{0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff}

	tests := []struct {
		name    string
		input   string
		wantMAC net.HardwareAddr
		wantErr bool
	}{
		{"valid string", `"aa:bb:cc:dd:ee:ff"`, hw, false},
		{"empty string", `""`, nil, false},
		{"null", `null`, nil, false},
		{"invalid string", `"not-a-mac"`, nil, true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			var m MACAddress
			err := json.Unmarshal([]byte(tt.input), &m)
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			require.NoError(t, err)
			assert.Equal(t, tt.wantMAC, m.HardwareAddr)
		})
	}
}
