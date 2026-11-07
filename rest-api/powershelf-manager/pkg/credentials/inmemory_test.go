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

func newCred(u, p string) *credential.Credential {
	c := credential.New(u, p)
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

func TestInMemoryPutGet(t *testing.T) {
	testCases := map[string]struct {
		initialPut bool
		putMAC     string
		putCred    *credential.Credential
		putAgain   *credential.Credential // if non-nil, perform a second Put to the same MAC after the initial one
		getMAC     string
		wantErr    bool
		wantUser   string
		wantPass   string
		samePtr    bool // expect Get to return the original tc.putCred pointer (i.e. no overwrite happened)
	}{
		"get existing valid credential": {
			initialPut: true,
			putMAC:     "00:11:22:33:44:55",
			putCred:    newCred("admin", "secret"),
			getMAC:     "00:11:22:33:44:55",
			wantErr:    false,
			wantUser:   "admin",
			wantPass:   "secret",
			samePtr:    true,
		},
		"get existing invalid credential (empty user) returns not found": {
			initialPut: true,
			putMAC:     "00:11:22:33:44:66",
			putCred:    newCred("", "nopass"),
			getMAC:     "00:11:22:33:44:66",
			wantErr:    true,
		},
		"get missing credential returns not found": {
			initialPut: false,
			getMAC:     "66:77:88:99:00:11",
			wantErr:    true,
		},
		"put overwrites existing value": {
			initialPut: true,
			putMAC:     "aa:bb:cc:dd:ee:ff",
			putCred:    newCred("user1", "p1"),
			putAgain:   newCred("user2", "p2"),
			getMAC:     "aa:bb:cc:dd:ee:ff",
			wantErr:    false,
			wantUser:   "user2",
			wantPass:   "p2",
			// samePtr stays false: overwrite replaces the stored pointer.
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Optional initial put, with an optional second Put to exercise
			// either the idempotent-skip path (tc.putAgain semantically equal
			// to tc.putCred) or the overwrite path (different values).
			if tc.initialPut {
				mac := parseMAC(t, tc.putMAC)
				assert.NoError(t, mgr.Put(ctx, mac, tc.putCred))
				if tc.putAgain != nil {
					assert.NoError(t, mgr.Put(ctx, mac, tc.putAgain))
				}
			}

			got, err := mgr.Get(ctx, parseMAC(t, tc.getMAC))
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

func TestInMemoryPatch(t *testing.T) {
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
			setupCred:     newCred("admin", "old"),
			patchMAC:      "00:11:22:33:44:55",
			patchCred:     newCred("root", "new"),
			wantErr:       false,
			wantUser:      "root",
			wantPass:      "new",
			expectSamePtr: true,
		},
		"patch missing returns error": {
			setupMAC:  "aa:bb:cc:dd:ee:ff",
			setupCred: newCred("user", "pass"),
			patchMAC:  "66:77:88:99:00:11",
			patchCred: newCred("root", "new"),
			wantErr:   true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Put initial
			assert.NoError(t, mgr.Put(ctx, parseMAC(t, tc.setupMAC), tc.setupCred))

			// Patch
			err := mgr.Patch(ctx, parseMAC(t, tc.patchMAC), tc.patchCred)
			if tc.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)

			// Verify Get returns updated credential
			got, err := mgr.Get(ctx, parseMAC(t, tc.patchMAC))
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

func TestInMemoryDelete(t *testing.T) {
	testCases := map[string]struct {
		putMAC       string
		putCred      *credential.Credential
		delMAC       string
		expectErrGet bool
	}{
		"delete existing removes entry": {
			putMAC:       "00:11:22:33:44:55",
			putCred:      newCred("admin", "secret"),
			delMAC:       "00:11:22:33:44:55",
			expectErrGet: true,
		},
		"delete missing returns nil": {
			putMAC:       "aa:bb:cc:dd:ee:ff",
			putCred:      newCred("user", "p"),
			delMAC:       "66:77:88:99:00:11",
			expectErrGet: false, // original still present
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr := NewInMemoryCredentialManager()

			// Put initial
			assert.NoError(t, mgr.Put(ctx, parseMAC(t, tc.putMAC), tc.putCred))

			// Delete target
			assert.NoError(t, mgr.Delete(ctx, parseMAC(t, tc.delMAC)))

			// Verify
			_, err := mgr.Get(ctx, parseMAC(t, tc.delMAC))
			if tc.expectErrGet {
				assert.Error(t, err)
			} else {
				// Ensure original entry remains when deleting a different MAC
				got, err2 := mgr.Get(ctx, parseMAC(t, tc.putMAC))
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
				{"00:11:22:33:44:55", newCred("admin", "secret")},
			},
			expectCount: 1,
			expectSet:   map[string]bool{"00:11:22:33:44:55": true},
		},
		"multiple entries return all MACs": {
			putPairs: [][2]interface{}{
				{"00:11:22:33:44:55", newCred("admin", "a")},
				{"66:77:88:99:00:11", newCred("root", "r")},
				{"aa:bb:cc:dd:ee:ff", newCred("user", "u")},
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
				assert.NoError(t, mgr.Put(ctx, parseMAC(t, macStr), cred))
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

func TestInMemoryPutIdempotentAndOverwrite(t *testing.T) {
	ctx := context.Background()
	mgr := NewInMemoryCredentialManager()
	mac := parseMAC(t, "00:11:22:33:44:55")

	// First Put succeeds
	assert.NoError(t, mgr.Put(ctx, mac, newCred("admin", "secret")))

	// Idempotent Put with same credentials is a no-op
	assert.NoError(t, mgr.Put(ctx, mac, newCred("admin", "secret")))

	// Put with different credentials overwrites (no error for in-memory)
	assert.NoError(t, mgr.Put(ctx, mac, newCred("admin", "different")))

	// Credentials are now the new values
	got, err := mgr.Get(ctx, mac)
	assert.NoError(t, err)
	assert.Equal(t, "admin", got.User)
	assert.Equal(t, "different", got.Password.Value)
}
