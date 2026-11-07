// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package capability

import (
	"errors"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestCapabilitySetNormalize(t *testing.T) {
	capabilities := CapabilitySet{
		" PowerControl ",
		CapabilityFirmwareControl,
		CapabilityPowerControl,
	}

	normalized, err := capabilities.Normalize()

	require.NoError(t, err)
	require.Equal(t, CapabilitySet{
		CapabilityFirmwareControl,
		CapabilityPowerControl,
	}, normalized)
}

func TestNewSet(t *testing.T) {
	capabilities, err := NewSet(
		" PowerControl ",
		CapabilityFirmwareControl,
		CapabilityPowerControl,
	)

	require.NoError(t, err)
	require.Equal(t, CapabilitySet{
		CapabilityFirmwareControl,
		CapabilityPowerControl,
	}, capabilities)
}

func TestCapabilitySetContains(t *testing.T) {
	capabilities := CapabilitySet{
		CapabilityPowerControl,
	}

	require.True(t, capabilities.Contains(CapabilityPowerControl))
	require.False(t, capabilities.Contains(CapabilityFirmwareControl))
}

func TestCapabilitySetAdd(t *testing.T) {
	capabilities := CapabilitySet{
		CapabilityPowerControl,
	}

	var err error
	capabilities, err = capabilities.Add(CapabilityPowerControl)
	require.NoError(t, err)
	capabilities, err = capabilities.Add(" FirmwareControl ")
	require.NoError(t, err)

	require.Equal(t, CapabilitySet{
		CapabilityFirmwareControl,
		CapabilityPowerControl,
	}, capabilities)
}

func TestCapabilitySetAddRejectsInvalidCapabilities(t *testing.T) {
	tests := []struct {
		name       string
		capability Capability
		wantErr    error
	}{
		{
			name:       "empty",
			capability: " ",
			wantErr:    ErrNameEmpty,
		},
		{
			name:       "unknown",
			capability: "PowerStatsu",
			wantErr:    ErrUnknown,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			capabilities, err := CapabilitySet{
				CapabilityPowerControl,
			}.Add(tt.capability)

			require.Nil(t, capabilities)
			require.ErrorIs(t, err, tt.wantErr)
		})
	}
}

func TestCapabilitySetSorted(t *testing.T) {
	capabilities := CapabilitySet{
		CapabilityPowerControl,
		CapabilityFirmwareControl,
	}

	sorted := capabilities.Sorted()

	require.Equal(t, CapabilitySet{
		CapabilityFirmwareControl,
		CapabilityPowerControl,
	}, sorted)
	require.Equal(t, CapabilitySet{
		CapabilityPowerControl,
		CapabilityFirmwareControl,
	}, capabilities)
}

func TestParse(t *testing.T) {
	capability, err := Parse(" PowerControl ")

	require.NoError(t, err)
	require.Equal(t, CapabilityPowerControl, capability)
	require.True(t, capability.Valid())
	require.Equal(t, "PowerControl", capability.String())
}

func TestCapabilitySetNormalizeRejectsInvalidCapabilities(t *testing.T) {
	tests := []struct {
		name         string
		capabilities CapabilitySet
		wantErr      error
		checkFunc    func(*testing.T, error)
	}{
		{
			name:         "empty",
			capabilities: CapabilitySet{" "},
			wantErr:      ErrNameEmpty,
		},
		{
			name:         "unknown",
			capabilities: CapabilitySet{"PowerStatsu"},
			wantErr:      ErrUnknown,
			checkFunc: func(t *testing.T, err error) {
				t.Helper()
				var capabilityErr UnknownError
				require.True(t, errors.As(err, &capabilityErr))
				require.Equal(t, Capability("PowerStatsu"), capabilityErr.Capability)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			capabilities, err := tt.capabilities.Normalize()

			require.Nil(t, capabilities)
			require.Error(t, err)
			require.True(t, errors.Is(err, tt.wantErr))
			if tt.checkFunc != nil {
				tt.checkFunc(t, err)
			}
		})
	}
}
