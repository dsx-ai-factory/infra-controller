// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nvswitchregistry_test

import (
	"context"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/nvswitchregistry"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/bmc"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvos"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/objects/nvswitch"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestInMemoryRegistry_Delete(t *testing.T) {
	newTray := func(t *testing.T, bmcMAC, nvosMAC string) *nvswitch.NVSwitchTray {
		t.Helper()
		b, err := bmc.New(bmcMAC, "192.0.2.1", nil)
		require.NoError(t, err)
		n, err := nvos.New(nvosMAC, "192.0.2.2", nil)
		require.NoError(t, err)
		return &nvswitch.NVSwitchTray{BMC: b, NVOS: n}
	}

	testCases := []struct {
		name             string
		deleteRegistered bool
		attempts         int
	}{
		{name: "registered and repeated deletion", deleteRegistered: true, attempts: 2},
		{name: "missing UUID preserves all trays", attempts: 1},
	}
	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			ctx := context.Background()
			registry := nvswitchregistry.NewInMemoryRegistry()
			tray := newTray(t, "02:00:00:00:01:01", "02:00:00:00:01:02")
			other := newTray(t, "02:00:00:00:02:01", "02:00:00:00:02:02")
			id, _, err := registry.Register(ctx, tray)
			require.NoError(t, err)
			otherID, _, err := registry.Register(ctx, other)
			require.NoError(t, err)

			assertIndexed := func(want *nvswitch.NVSwitchTray) {
				t.Helper()
				got, err := registry.Get(ctx, want.UUID)
				require.NoError(t, err)
				assert.Same(t, want, got)
				for _, mac := range []string{want.BMC.MAC.String(), want.NVOS.MAC.String()} {
					got, err = registry.GetByBMCMAC(ctx, mac)
					require.NoError(t, err, "MAC %s", mac)
					assert.Same(t, want, got, "MAC %s", mac)
				}
			}
			// Establish both MAC indexes before testing their removal.
			assertIndexed(tray)
			assertIndexed(other)

			deleteID := uuid.New()
			wantList := []*nvswitch.NVSwitchTray{tray, other}
			if tc.deleteRegistered {
				deleteID = id
				wantList = []*nvswitch.NVSwitchTray{other}
			}
			for range tc.attempts {
				err = registry.Delete(ctx, deleteID)
				require.NoError(t, err)
				if tc.deleteRegistered {
					got, err := registry.Get(ctx, id)
					assert.Error(t, err)
					assert.Nil(t, got)
					for _, mac := range []string{tray.BMC.MAC.String(), tray.NVOS.MAC.String()} {
						got, err = registry.GetByBMCMAC(ctx, mac)
						assert.Error(t, err, "deleted MAC %s must not retain an index", mac)
						assert.Nil(t, got)
					}
				} else {
					assertIndexed(tray)
				}
				assertIndexed(other)
				assert.Equal(t, otherID, other.UUID)
				listed, err := registry.List(ctx)
				require.NoError(t, err)
				assert.ElementsMatch(t, wantList, listed)
			}

			if tc.deleteRegistered {
				// A fresh tray must not inherit the deleted tray's UUID via a stale MAC index.
				replacement := newTray(t, tray.BMC.MAC.String(), tray.NVOS.MAC.String())
				newID, created, err := registry.Register(ctx, replacement)
				require.NoError(t, err)
				assert.True(t, created)
				assert.NotEqual(t, uuid.Nil, newID)
				assert.NotEqual(t, id, newID)
				assertIndexed(replacement)
				assertIndexed(other)
				got, err := registry.Get(ctx, id)
				assert.Error(t, err)
				assert.Nil(t, got)
				listed, err := registry.List(ctx)
				require.NoError(t, err)
				assert.ElementsMatch(t, []*nvswitch.NVSwitchTray{replacement, other}, listed)
			}
		})
	}
}
