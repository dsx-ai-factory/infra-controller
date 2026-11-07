// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvswitchmanager

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/credentials"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/nvswitchregistry"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func newTestManager() *NVSwitchManager {
	return &NVSwitchManager{
		Registry:          nvswitchregistry.NewInMemoryRegistry(),
		CredentialManager: credentials.NewInMemoryCredentialManager(),
	}
}

func mustParseBMC(t *testing.T, mac, ip string) *bmc.BMC {
	t.Helper()
	b, err := bmc.New(mac, ip, nil)
	require.NoError(t, err)
	return b
}

func mustParseNVOS(t *testing.T, mac, ip string) *nvos.NVOS {
	t.Helper()
	n, err := nvos.New(mac, ip, nil)
	require.NoError(t, err)
	return n
}

func newTestTray(t *testing.T) *nvswitch.NVSwitchTray {
	t.Helper()
	return &nvswitch.NVSwitchTray{
		UUID: uuid.New(),
		BMC:  mustParseBMC(t, "AA:BB:CC:DD:EE:FF", "10.0.0.1"),
		NVOS: mustParseNVOS(t, "11:22:33:44:55:66", "10.0.0.2"),
	}
}

func TestNVSwitchManager_Register(t *testing.T) {
	testCases := map[string]struct {
		tray        func(t *testing.T) *nvswitch.NVSwitchTray
		wantErr     bool
		errContains string
	}{
		"register with BMC and NVOS succeeds": {
			tray:    newTestTray,
			wantErr: false,
		},
		"register without BMC returns error": {
			tray: func(t *testing.T) *nvswitch.NVSwitchTray {
				tray := newTestTray(t)
				tray.BMC = nil
				return tray
			},
			wantErr:     true,
			errContains: "BMC subsystem is required",
		},
		"register without NVOS returns error": {
			tray: func(t *testing.T) *nvswitch.NVSwitchTray {
				tray := newTestTray(t)
				tray.NVOS = nil
				return tray
			},
			wantErr:     true,
			errContains: "NVOS subsystem is required",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			nm := newTestManager()
			ctx := context.Background()

			_, _, err := nm.Register(ctx, tc.tray(t))
			if tc.wantErr {
				assert.Error(t, err)
				assert.Contains(t, err.Error(), tc.errContains)
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestNVSwitchManager_Get(t *testing.T) {
	testCases := map[string]struct {
		setupBMCCred  bool
		setupNVOSCred bool
		wantErr       bool
		errContains   string
	}{
		"get with BMC and NVOS credentials succeeds": {
			setupBMCCred:  true,
			setupNVOSCred: true,
			wantErr:       false,
		},
		"get without BMC credentials returns error": {
			setupBMCCred:  false,
			setupNVOSCred: false,
			wantErr:       true,
			errContains:   "loading BMC credentials",
		},
		"get with BMC credentials but missing NVOS credentials returns error": {
			setupBMCCred:  true,
			setupNVOSCred: false,
			wantErr:       true,
			errContains:   "loading NVOS credentials",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			nm := newTestManager()
			ctx := context.Background()

			tray := newTestTray(t)

			if tc.setupBMCCred {
				c := credential.New("admin", "pass")
				require.NoError(t, nm.CredentialManager.PutBMC(ctx, tray.BMC.MAC, &c))
			}
			if tc.setupNVOSCred {
				c := credential.New("nvos_admin", "nvos_pass")
				require.NoError(t, nm.CredentialManager.PutNVOS(ctx, tray.BMC.MAC, &c))
			}

			_, _, err := nm.Registry.Register(ctx, tray)
			require.NoError(t, err)

			got, err := nm.Get(ctx, tray.UUID)
			if tc.wantErr {
				assert.Error(t, err)
				assert.Nil(t, got)
				assert.Contains(t, err.Error(), tc.errContains)
				return
			}

			require.NoError(t, err)
			require.NotNil(t, got)

			assert.NotNil(t, got.BMC.Credential, "BMC credential should be attached")
			assert.Equal(t, "admin", got.BMC.Credential.User)

			assert.NotNil(t, got.NVOS.Credential, "NVOS credential should be attached")
			assert.Equal(t, "nvos_admin", got.NVOS.Credential.User)
		})
	}
}

func TestNVSwitchManager_Get_NotFound(t *testing.T) {
	nm := newTestManager()
	ctx := context.Background()

	got, err := nm.Get(ctx, uuid.New())
	assert.Error(t, err)
	assert.Nil(t, got)
}

func TestNVSwitchManager_Get_NilBMC(t *testing.T) {
	nm := newTestManager()
	ctx := context.Background()

	tray := newTestTray(t)
	_, _, err := nm.Registry.Register(ctx, tray)
	require.NoError(t, err)

	// Nil out BMC after registration to simulate corrupt/incomplete data.
	tray.BMC = nil

	got, err := nm.Get(ctx, tray.UUID)
	assert.Error(t, err)
	assert.Nil(t, got)
	assert.Contains(t, err.Error(), "no BMC subsystem")
}

func TestNVSwitchManager_Get_NilNVOS(t *testing.T) {
	nm := newTestManager()
	ctx := context.Background()

	tray := newTestTray(t)
	_, _, err := nm.Registry.Register(ctx, tray)
	require.NoError(t, err)

	// Nil out NVOS after registration to simulate corrupt/incomplete data.
	tray.NVOS = nil

	got, err := nm.Get(ctx, tray.UUID)
	assert.Error(t, err)
	assert.Nil(t, got)
	assert.Contains(t, err.Error(), "no NVOS subsystem")
}
