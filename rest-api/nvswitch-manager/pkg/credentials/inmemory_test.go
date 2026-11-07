// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"context"
	"net"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"

	"github.com/stretchr/testify/assert"
)

func newCredential(user, password string) *credential.Credential {
	c := credential.New(user, password)
	return &c
}

func parseMAC(t *testing.T, s string) net.HardwareAddr {
	t.Helper()
	m, err := net.ParseMAC(s)
	assert.NoError(t, err, "failed to parse MAC %q", s)
	return m
}

func TestInMemoryStartStop(t *testing.T) {
	testCases := map[string]struct {
		setup func() *InMemoryCredentialManager
	}{
		"start and stop return nil": {
			setup: func() *InMemoryCredentialManager {
				return NewInMemoryCredentialManager()
			},
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			mgr := tc.setup()
			assert.NoError(t, mgr.Start(context.Background()))
			assert.NoError(t, mgr.Stop(context.Background()))
		})
	}
}

func TestInMemoryBMCPutGet(t *testing.T) {
	testCases := map[string]struct {
		initialPut   bool
		putMAC       string
		putCred      *credential.Credential
		putSameAgain bool // if true, immediately re-put a fresh-but-equal credential to exercise the idempotent skip path
		getMAC       string
		wantErr      bool
		wantUser     string
		wantPass     string
		samePtr      bool
	}{
		"get existing valid BMC credential": {
			initialPut: true,
			putMAC:     "00:11:22:33:44:55",
			putCred:    newCredential("admin", "secret"),
			getMAC:     "00:11:22:33:44:55",
			wantErr:    false,
			wantUser:   "admin",
			wantPass:   "secret",
			samePtr:    true,
		},
		"get existing invalid credential (empty user) returns not found": {
			initialPut: true,
			putMAC:     "00:11:22:33:44:66",
			putCred:    newCredential("", "nopass"),
			getMAC:     "00:11:22:33:44:66",
			wantErr:    true,
		},
		"get missing credential returns not found": {
			initialPut: false,
			getMAC:     "66:77:88:99:00:11",
			wantErr:    true,
		},
		"put same credential is no-op": {
			initialPut:   true,
			putMAC:       "aa:bb:cc:dd:ee:ff",
			putCred:      newCredential("user1", "p1"),
			putSameAgain: true,
			getMAC:       "aa:bb:cc:dd:ee:ff",
			wantErr:      false,
			wantUser:     "user1",
			wantPass:     "p1",
			samePtr:      true, // second put with equal-but-fresh pointer must be skipped, leaving original pointer in place
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Optional initial put
			if tc.initialPut {
				mac := parseMAC(t, tc.putMAC)
				assert.NoError(t, mgr.PutBMC(ctx, mac, tc.putCred))
				// Exercise the idempotent skip path with a fresh-but-equal
				// credential pointer. samePtr below verifies the original
				// pointer survived (i.e. the second Put was actually skipped,
				// not just rewritten with the same values).
				if tc.putSameAgain {
					assert.NoError(t, mgr.PutBMC(ctx, mac, newCredential(tc.putCred.User, tc.putCred.Password.Value)))
				}
			}

			// Get flow
			got, err := mgr.GetBMC(ctx, parseMAC(t, tc.getMAC))
			if tc.wantErr {
				assert.Error(t, err)
				assert.Nil(t, got)
				return
			}

			assert.NoError(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tc.wantUser, got.User)
			assert.Equal(t, tc.wantPass, got.Password.Value)

			if tc.samePtr && tc.initialPut {
				assert.Same(t, tc.putCred, got)
			}
		})
	}
}

func TestInMemoryPutDifferentCredentialOverwrites(t *testing.T) {
	ctx := context.Background()
	mgr := NewInMemoryCredentialManager()
	mac := parseMAC(t, "00:11:22:33:44:55")

	// Initial put succeeds
	assert.NoError(t, mgr.PutBMC(ctx, mac, newCredential("admin", "secret")))
	assert.NoError(t, mgr.PutNVOS(ctx, mac, newCredential("nvos", "nvos_secret")))

	// Put with different credentials overwrites (no error for in-memory)
	assert.NoError(t, mgr.PutBMC(ctx, mac, newCredential("admin", "different_pass")))
	assert.NoError(t, mgr.PutNVOS(ctx, mac, newCredential("nvos", "different_pass")))

	// Credentials are now the new values
	bmcCred, err := mgr.GetBMC(ctx, mac)
	assert.NoError(t, err)
	assert.Equal(t, "admin", bmcCred.User)
	assert.Equal(t, "different_pass", bmcCred.Password.Value)

	nvosCred, err := mgr.GetNVOS(ctx, mac)
	assert.NoError(t, err)
	assert.Equal(t, "nvos", nvosCred.User)
	assert.Equal(t, "different_pass", nvosCred.Password.Value)
}

func TestInMemoryNVOSPutGet(t *testing.T) {
	testCases := map[string]struct {
		initialPut bool
		putMAC     string
		putCred    *credential.Credential
		getMAC     string
		wantErr    bool
		wantUser   string
		wantPass   string
	}{
		"get existing valid NVOS credential": {
			initialPut: true,
			putMAC:     "00:11:22:33:44:55",
			putCred:    newCredential("nvos_admin", "nvos_secret"),
			getMAC:     "00:11:22:33:44:55",
			wantErr:    false,
			wantUser:   "nvos_admin",
			wantPass:   "nvos_secret",
		},
		"get missing NVOS credential returns not found": {
			initialPut: false,
			getMAC:     "66:77:88:99:00:11",
			wantErr:    true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Optional initial put
			if tc.initialPut {
				mac := parseMAC(t, tc.putMAC)
				assert.NoError(t, mgr.PutNVOS(ctx, mac, tc.putCred))
			}

			// Get flow
			got, err := mgr.GetNVOS(ctx, parseMAC(t, tc.getMAC))
			if tc.wantErr {
				assert.Error(t, err)
				assert.Nil(t, got)
				return
			}

			assert.NoError(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tc.wantUser, got.User)
			assert.Equal(t, tc.wantPass, got.Password.Value)
		})
	}
}

func TestInMemoryBMCPatch(t *testing.T) {
	testCases := map[string]struct {
		setupMAC      string
		setupCred     *credential.Credential
		patchMAC      string
		patchCred     *credential.Credential
		wantErr       bool
		wantUser      string
		wantPass      string
		expectSamePtr bool
	}{
		"patch existing replaces value": {
			setupMAC:      "00:11:22:33:44:55",
			setupCred:     newCredential("admin", "old"),
			patchMAC:      "00:11:22:33:44:55",
			patchCred:     newCredential("root", "new"),
			wantErr:       false,
			wantUser:      "root",
			wantPass:      "new",
			expectSamePtr: true,
		},
		"patch missing returns error": {
			setupMAC:  "aa:bb:cc:dd:ee:ff",
			setupCred: newCredential("user", "pass"),
			patchMAC:  "66:77:88:99:00:11",
			patchCred: newCredential("root", "new"),
			wantErr:   true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Put initial
			assert.NoError(t, mgr.PutBMC(ctx, parseMAC(t, tc.setupMAC), tc.setupCred))

			// Patch
			err := mgr.PatchBMC(ctx, parseMAC(t, tc.patchMAC), tc.patchCred)
			if tc.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)

			// Verify Get returns updated credential
			got, err := mgr.GetBMC(ctx, parseMAC(t, tc.patchMAC))
			assert.NoError(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tc.wantUser, got.User)
			assert.Equal(t, tc.wantPass, got.Password.Value)
			if tc.expectSamePtr {
				assert.Same(t, tc.patchCred, got)
			}
		})
	}
}

func TestInMemoryBMCDelete(t *testing.T) {
	testCases := map[string]struct {
		putMAC       string
		putCred      *credential.Credential
		delMAC       string
		expectErrGet bool
	}{
		"delete existing removes entry": {
			putMAC:       "00:11:22:33:44:55",
			putCred:      newCredential("admin", "secret"),
			delMAC:       "00:11:22:33:44:55",
			expectErrGet: true,
		},
		"delete missing returns nil": {
			putMAC:       "aa:bb:cc:dd:ee:ff",
			putCred:      newCredential("user", "p"),
			delMAC:       "66:77:88:99:00:11",
			expectErrGet: false, // original still present
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Put initial
			assert.NoError(t, mgr.PutBMC(ctx, parseMAC(t, tc.putMAC), tc.putCred))

			// Delete target
			assert.NoError(t, mgr.DeleteBMC(ctx, parseMAC(t, tc.delMAC)))

			// Verify
			_, err := mgr.GetBMC(ctx, parseMAC(t, tc.delMAC))
			if tc.expectErrGet {
				assert.Error(t, err)
			} else {
				// Ensure original entry remains when deleting a different MAC
				got, err2 := mgr.GetBMC(ctx, parseMAC(t, tc.putMAC))
				assert.NoError(t, err2)
				assert.NotNil(t, got)
				assert.Equal(t, tc.putCred.User, got.User)
				assert.Equal(t, tc.putCred.Password.Value, got.Password.Value)
			}
		})
	}
}

func TestInMemoryKeys(t *testing.T) {
	testCases := map[string]struct {
		putPairs    [][2]interface{} // [mac string, *credential.Credential]
		expectCount int
		expectSet   map[string]bool
	}{
		"no entries returns empty": {
			putPairs:    nil,
			expectCount: 0,
			expectSet:   map[string]bool{},
		},
		"one entry returns that MAC": {
			putPairs: [][2]interface{}{
				{"00:11:22:33:44:55", newCredential("admin", "secret")},
			},
			expectCount: 1,
			expectSet:   map[string]bool{"00:11:22:33:44:55": true},
		},
		"multiple entries return all MACs": {
			putPairs: [][2]interface{}{
				{"00:11:22:33:44:55", newCredential("admin", "a")},
				{"66:77:88:99:00:11", newCredential("root", "r")},
				{"aa:bb:cc:dd:ee:ff", newCredential("user", "u")},
			},
			expectCount: 3,
			expectSet: map[string]bool{
				"00:11:22:33:44:55": true,
				"66:77:88:99:00:11": true,
				"aa:bb:cc:dd:ee:ff": true,
			},
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Populate
			for _, pair := range tc.putPairs {
				macStr := pair[0].(string)
				cred := pair[1].(*credential.Credential)
				assert.NoError(t, mgr.PutBMC(ctx, parseMAC(t, macStr), cred))
			}

			// Keys
			keys, err := mgr.Keys(ctx)
			assert.NoError(t, err)
			assert.Equal(t, tc.expectCount, len(keys))

			// Build set from returned keys for lookup
			gotSet := make(map[string]bool, len(keys))
			for _, mac := range keys {
				gotSet[mac.String()] = true
			}
			assert.Equal(t, tc.expectSet, gotSet)
		})
	}
}
