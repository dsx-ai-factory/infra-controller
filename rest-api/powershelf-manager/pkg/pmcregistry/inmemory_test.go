// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmcregistry

import (
	"context"
	"net"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"

	"github.com/stretchr/testify/assert"
)

func parseMAC(t *testing.T, s string) net.HardwareAddr {
	t.Helper()
	m, err := net.ParseMAC(s)
	assert.NoError(t, err, "failed to parse MAC %q", s)
	return m
}

func makePMC(t *testing.T, macStr, ip string) *pmc.PMC {
	t.Helper()
	obj, err := pmc.New(macStr, ip, vendor.VendorCodeLiteon, nil)
	assert.NoError(t, err)
	assert.NotNil(t, obj)
	return obj
}

func TestMemRegistryStartStop(t *testing.T) {
	testCases := map[string]struct {
		setup func() *MemRegistry
	}{
		"start and stop return nil": {
			setup: func() *MemRegistry { return NewMemRegistry() },
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			reg := tc.setup()
			assert.NoError(t, reg.Start(context.Background()))
			assert.NoError(t, reg.Stop(context.Background()))
		})
	}
}

func TestMemRegistryRegisterPmc(t *testing.T) {
	testCases := map[string]struct {
		setup       func() *MemRegistry
		inputPMC    *pmc.PMC
		wantErr     bool
		errContains string
		wantIP      string
	}{
		"nil pmc returns error": {
			setup:       func() *MemRegistry { return NewMemRegistry() },
			inputPMC:    nil,
			wantErr:     true,
			errContains: "cannot register nil PMC",
		},
		"register valid pmc succeeds": {
			setup:    func() *MemRegistry { return NewMemRegistry() },
			inputPMC: makePMC(t, "00:11:22:33:44:55", "192.168.1.10"),
			wantErr:  false,
			wantIP:   "192.168.1.10",
		},
		"duplicate registration upserts": {
			setup: func() *MemRegistry {
				reg := NewMemRegistry()
				p := makePMC(t, "00:11:22:33:44:55", "192.168.1.10")
				assert.NoError(t, reg.RegisterPmc(context.Background(), p))
				return reg
			},
			inputPMC: makePMC(t, "00:11:22:33:44:55", "192.168.1.20"),
			wantErr:  false,
			wantIP:   "192.168.1.20",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			reg := tc.setup()
			err := reg.RegisterPmc(context.Background(), tc.inputPMC)
			if tc.wantErr {
				assert.Error(t, err)
				if tc.errContains != "" {
					assert.Contains(t, err.Error(), tc.errContains)
				}
				return
			}
			assert.NoError(t, err)
			if tc.inputPMC != nil {
				got, err := reg.GetPmc(context.Background(), tc.inputPMC.GetMac())
				assert.NoError(t, err)
				assert.Same(t, tc.inputPMC, got)
				assert.Equal(t, tc.wantIP, got.GetIp().String())
			}
		})
	}
}

func TestMemRegistryIsPmcRegistered(t *testing.T) {
	testCases := map[string]struct {
		setup    func() *MemRegistry
		queryMAC net.HardwareAddr
		wantReg  bool
	}{
		"registered MAC returns true": {
			setup: func() *MemRegistry {
				reg := NewMemRegistry()
				p := makePMC(t, "00:11:22:33:44:55", "192.168.1.10")
				assert.NoError(t, reg.RegisterPmc(context.Background(), p))
				return reg
			},
			queryMAC: parseMAC(t, "00:11:22:33:44:55"),
			wantReg:  true,
		},
		"unregistered MAC returns false": {
			setup: func() *MemRegistry {
				reg := NewMemRegistry()
				p := makePMC(t, "00:11:22:33:44:55", "192.168.1.10")
				assert.NoError(t, reg.RegisterPmc(context.Background(), p))
				return reg
			},
			queryMAC: parseMAC(t, "66:77:88:99:00:11"),
			wantReg:  false,
		},
		"empty registry returns false": {
			setup:    func() *MemRegistry { return NewMemRegistry() },
			queryMAC: parseMAC(t, "00:11:22:33:44:55"),
			wantReg:  false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			reg := tc.setup()
			ok, err := reg.IsPmcRegistered(context.Background(), tc.queryMAC)
			assert.NoError(t, err)
			assert.Equal(t, tc.wantReg, ok)
		})
	}
}

func TestMemRegistryGetPmc(t *testing.T) {
	testCases := map[string]struct {
		setup    func() *MemRegistry
		queryMAC net.HardwareAddr
		wantErr  bool
		wantIP   string
	}{
		"get registered PMC succeeds": {
			setup: func() *MemRegistry {
				reg := NewMemRegistry()
				p := makePMC(t, "00:11:22:33:44:55", "192.168.1.10")
				assert.NoError(t, reg.RegisterPmc(context.Background(), p))
				return reg
			},
			queryMAC: parseMAC(t, "00:11:22:33:44:55"),
			wantErr:  false,
			wantIP:   "192.168.1.10",
		},
		"get unregistered PMC returns error": {
			setup: func() *MemRegistry {
				reg := NewMemRegistry()
				p := makePMC(t, "00:11:22:33:44:55", "192.168.1.10")
				assert.NoError(t, reg.RegisterPmc(context.Background(), p))
				return reg
			},
			queryMAC: parseMAC(t, "66:77:88:99:00:11"),
			wantErr:  true,
		},
		"empty registry returns error": {
			setup:    func() *MemRegistry { return NewMemRegistry() },
			queryMAC: parseMAC(t, "00:11:22:33:44:55"),
			wantErr:  true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			reg := tc.setup()
			got, err := reg.GetPmc(context.Background(), tc.queryMAC)
			if tc.wantErr {
				assert.Error(t, err)
				assert.Nil(t, got)
				return
			}
			assert.NoError(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tc.wantIP, got.GetIp().String())
		})
	}
}

func TestMemRegistryGetAllPmcs(t *testing.T) {
	testCases := map[string]struct {
		setup       func() *MemRegistry
		expectCount int
		expectMACs  map[string]bool
	}{
		"empty registry returns empty slice": {
			setup:       func() *MemRegistry { return NewMemRegistry() },
			expectCount: 0,
			expectMACs:  map[string]bool{},
		},
		"registry with one PMC": {
			setup: func() *MemRegistry {
				reg := NewMemRegistry()
				p := makePMC(t, "00:11:22:33:44:55", "192.168.1.10")
				assert.NoError(t, reg.RegisterPmc(context.Background(), p))
				return reg
			},
			expectCount: 1,
			expectMACs:  map[string]bool{"00:11:22:33:44:55": true},
		},
		"registry with two PMCs": {
			setup: func() *MemRegistry {
				reg := NewMemRegistry()
				p1 := makePMC(t, "00:11:22:33:44:55", "192.168.1.10")
				p2 := makePMC(t, "66:77:88:99:00:11", "192.168.1.11")
				assert.NoError(t, reg.RegisterPmc(context.Background(), p1))
				assert.NoError(t, reg.RegisterPmc(context.Background(), p2))
				return reg
			},
			expectCount: 2,
			expectMACs: map[string]bool{
				"00:11:22:33:44:55": true,
				"66:77:88:99:00:11": true,
			},
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			reg := tc.setup()
			pmcs, err := reg.GetAllPmcs(context.Background())
			assert.NoError(t, err)
			assert.Equal(t, tc.expectCount, len(pmcs))

			gotSet := make(map[string]bool, len(pmcs))
			for _, p := range pmcs {
				gotSet[p.GetMac().String()] = true
			}
			assert.Equal(t, tc.expectMACs, gotSet)
		})
	}
}
