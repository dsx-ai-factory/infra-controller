// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/credential"

	"github.com/stretchr/testify/assert"
)

type fakeVaultKVServer struct {
	store        map[string]map[string]interface{} // uppercase MAC -> secret data (UsernamePassword map)
	mountPresent bool
}

func newFakeVaultKVServer() *fakeVaultKVServer {
	return &fakeVaultKVServer{
		store:        make(map[string]map[string]interface{}),
		mountPresent: false,
	}
}

func writeJSON(w http.ResponseWriter, status int, v interface{}) {
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(v)
}

func (s *fakeVaultKVServer) handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		path := strings.TrimPrefix(r.URL.Path, "/v1")
		switch {
		// List mounts
		case path == "/sys/mounts" && r.Method == http.MethodGet:
			data := map[string]interface{}{}
			if s.mountPresent {
				data["secrets/"] = map[string]interface{}{"type": "kv"}
			}
			writeJSON(w, http.StatusOK, map[string]interface{}{"data": data})
			return

		// Enable mount (Vault client uses PUT)
		case path == "/sys/mounts/secrets" && r.Method == http.MethodPut:
			_, _ = io.ReadAll(r.Body)
			_ = r.Body.Close()
			s.mountPresent = true
			writeJSON(w, http.StatusNoContent, map[string]interface{}{})
			return

		// KV v2 write: Core/NSM layout secrets/data/machines/bmc/{MAC}/root
		case strings.HasPrefix(path, "/secrets/data/machines/bmc/") && strings.HasSuffix(path, "/root") && (r.Method == http.MethodPut || r.Method == http.MethodPost):
			mac := strings.TrimSuffix(strings.TrimPrefix(path, "/secrets/data/machines/bmc/"), "/root")
			var payload map[string]interface{}
			_ = json.NewDecoder(r.Body).Decode(&payload)
			_ = r.Body.Close()
			if data, ok := payload["data"].(map[string]interface{}); ok {
				s.store[mac] = data
				writeJSON(w, http.StatusOK, map[string]interface{}{"data": map[string]interface{}{}})
				return
			}
			writeJSON(w, http.StatusBadRequest, map[string]interface{}{"errors": []string{"bad payload"}})
			return

		// KV v2 read
		case strings.HasPrefix(path, "/secrets/data/machines/bmc/") && strings.HasSuffix(path, "/root") && r.Method == http.MethodGet:
			mac := strings.TrimSuffix(strings.TrimPrefix(path, "/secrets/data/machines/bmc/"), "/root")
			if data, ok := s.store[mac]; ok {
				writeJSON(w, http.StatusOK, map[string]interface{}{
					"data": map[string]interface{}{
						"data": data,
					},
				})
				return
			}
			writeJSON(w, http.StatusNotFound, map[string]interface{}{"errors": []string{"not found"}})
			return

		// KV v2 delete
		case strings.HasPrefix(path, "/secrets/data/machines/bmc/") && strings.HasSuffix(path, "/root") && r.Method == http.MethodDelete:
			mac := strings.TrimSuffix(strings.TrimPrefix(path, "/secrets/data/machines/bmc/"), "/root")
			delete(s.store, mac)
			writeJSON(w, http.StatusNoContent, map[string]interface{}{})
			return

		// KV v2 list: GET /secrets/metadata/machines/bmc?list=true (NSM pattern)
		case path == "/secrets/metadata/machines/bmc" && r.Method == http.MethodGet && r.URL.Query().Get("list") == "true":
			if len(s.store) == 0 {
				writeJSON(w, http.StatusNotFound, map[string]interface{}{"errors": []string{"no keys"}})
				return
			}
			keys := make([]interface{}, 0, len(s.store))
			for mac := range s.store {
				keys = append(keys, mac+"/")
			}
			writeJSON(w, http.StatusOK, map[string]interface{}{
				"data": map[string]interface{}{
					"keys": keys,
				},
			})
			return

		default:
			writeJSON(w, http.StatusNotFound, map[string]interface{}{"errors": []string{fmt.Sprintf("unhandled path %s %s", r.Method, r.URL.Path)}})
			return
		}
	})
}

func newManagerWithServer(t *testing.T, srv *httptest.Server) *VaultCredentialManager {
	t.Helper()
	cfg := &VaultConfig{
		Address: srv.URL,
		Token:   "test-token",
	}
	assert.NoError(t, cfg.Validate())
	mgr, err := cfg.NewManager()
	assert.NoError(t, err)
	assert.NotNil(t, mgr)
	return mgr
}

func TestVaultManager_Start(t *testing.T) {
	testCases := map[string]struct {
		initialMount bool
	}{
		"mount absent -> Start configures kv-v2 successfully": {
			initialMount: false,
		},
		"mount present -> Start returns nil without reconfiguring": {
			initialMount: true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			fake := newFakeVaultKVServer()
			fake.mountPresent = tc.initialMount
			srv := httptest.NewServer(fake.handler())
			defer srv.Close()

			mgr := newManagerWithServer(t, srv)
			err := mgr.Start(context.Background())
			assert.NoError(t, err)
			assert.True(t, fake.mountPresent)

			assert.NoError(t, mgr.Stop(context.Background()))
		})
	}
}

func TestVaultManager_PutGet(t *testing.T) {
	testCases := map[string]struct {
		putMAC   string
		putCred  *credential.Credential
		getMAC   string
		wantErr  bool
		wantUser string
		wantPass string
	}{
		"put and get valid credential": {
			putMAC:   "00:11:22:33:44:55",
			putCred:  newCred("admin", "secret"),
			getMAC:   "00:11:22:33:44:55",
			wantErr:  false,
			wantUser: "admin",
			wantPass: "secret",
		},
		"put invalid credential (empty user) returns error": {
			putMAC:  "00:11:22:33:44:66",
			putCred: newCred("", "nopass"),
			getMAC:  "00:11:22:33:44:66",
			wantErr: true,
		},
		"get missing credential returns not found": {
			putMAC:  "aa:bb:cc:dd:ee:ff",
			putCred: newCred("user", "p"),
			getMAC:  "66:77:88:99:00:11",
			wantErr: true,
		},
		"put with uppercase MAC and get with lowercase resolves correctly": {
			putMAC:   "AA:BB:CC:DD:EE:FF",
			putCred:  newCred("admin", "secret"),
			getMAC:   "aa:bb:cc:dd:ee:ff",
			wantErr:  false,
			wantUser: "admin",
			wantPass: "secret",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			fake := newFakeVaultKVServer()
			fake.mountPresent = true
			srv := httptest.NewServer(fake.handler())
			defer srv.Close()

			ctx := context.Background()
			mgr := newManagerWithServer(t, srv)

			errPut := mgr.Put(ctx, parseMAC(t, tc.putMAC), tc.putCred)
			if tc.putCred.User == "" {
				assert.Error(t, errPut)
			} else {
				assert.NoError(t, errPut)
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
		})
	}
}

func TestVaultManager_Patch(t *testing.T) {
	testCases := map[string]struct {
		setupMAC  string
		setupCred *credential.Credential
		patchMAC  string
		patchCred *credential.Credential
		wantErr   bool
		wantUser  string
		wantPass  string
	}{
		"patch existing replaces value": {
			setupMAC:  "00:11:22:33:44:55",
			setupCred: newCred("admin", "old"),
			patchMAC:  "00:11:22:33:44:55",
			patchCred: newCred("root", "new"),
			wantErr:   false,
			wantUser:  "root",
			wantPass:  "new",
		},
		"patch missing creates value (same as Put)": {
			setupMAC:  "aa:bb:cc:dd:ee:ff",
			setupCred: newCred("user", "pass"),
			patchMAC:  "66:77:88:99:00:11",
			patchCred: newCred("root", "new"),
			wantErr:   false,
			wantUser:  "root",
			wantPass:  "new",
		},
		"patch with invalid credential returns error": {
			setupMAC:  "00:11:22:33:44:66",
			setupCred: newCred("user", "pass"),
			patchMAC:  "00:11:22:33:44:66",
			patchCred: newCred("", "nopass"),
			wantErr:   true,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			fake := newFakeVaultKVServer()
			fake.mountPresent = true
			srv := httptest.NewServer(fake.handler())
			defer srv.Close()

			ctx := context.Background()
			mgr := newManagerWithServer(t, srv)

			assert.NoError(t, mgr.Put(ctx, parseMAC(t, tc.setupMAC), tc.setupCred))

			err := mgr.Patch(ctx, parseMAC(t, tc.patchMAC), tc.patchCred)
			if tc.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)

			got, err := mgr.Get(ctx, parseMAC(t, tc.patchMAC))
			assert.NoError(t, err)
			assert.NotNil(t, got)
			assert.Equal(t, tc.wantUser, got.User)
			assert.Equal(t, tc.wantPass, got.Password.Value)
		})
	}
}

func TestVaultManager_Delete(t *testing.T) {
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
		"delete missing returns nil and does not affect other entries": {
			putMAC:       "aa:bb:cc:dd:ee:ff",
			putCred:      newCred("user", "p"),
			delMAC:       "66:77:88:99:00:11",
			expectErrGet: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			fake := newFakeVaultKVServer()
			fake.mountPresent = true
			srv := httptest.NewServer(fake.handler())
			defer srv.Close()

			ctx := context.Background()
			mgr := newManagerWithServer(t, srv)

			assert.NoError(t, mgr.Put(ctx, parseMAC(t, tc.putMAC), tc.putCred))
			assert.NoError(t, mgr.Delete(ctx, parseMAC(t, tc.delMAC)))

			_, err := mgr.Get(ctx, parseMAC(t, tc.delMAC))
			if tc.expectErrGet {
				assert.Error(t, err)
			} else {
				got, err2 := mgr.Get(ctx, parseMAC(t, tc.putMAC))
				assert.NoError(t, err2)
				assert.NotNil(t, got)
				assert.Equal(t, tc.putCred.User, got.User)
				assert.Equal(t, tc.putCred.Password.Value, got.Password.Value)
			}
		})
	}
}

// TestVaultManager_PutUpsertSemantics pins the upsert contract that Put
// shares with the in-memory backend: identical credentials are a no-op,
// differing credentials overwrite (with a warning log not asserted here),
// and Patch unconditionally replaces.
func TestVaultManager_PutUpsertSemantics(t *testing.T) {
	fake := newFakeVaultKVServer()
	fake.mountPresent = true
	srv := httptest.NewServer(fake.handler())
	defer srv.Close()

	ctx := context.Background()
	mgr := newManagerWithServer(t, srv)
	mac := parseMAC(t, "00:11:22:33:44:55")

	// First Put writes the credentials.
	assert.NoError(t, mgr.Put(ctx, mac, newCred("admin", "secret")))

	// Idempotent Put with identical credentials is skipped, not rewritten.
	assert.NoError(t, mgr.Put(ctx, mac, newCred("admin", "secret")))

	// Put with different credentials succeeds and overwrites the existing
	// entry (warn-and-overwrite). This matches the in-memory backend so that
	// callers like pmcmanager.Register get consistent semantics across
	// datastore types.
	assert.NoError(t, mgr.Put(ctx, mac, newCred("admin", "rotated")))

	got, err := mgr.Get(ctx, mac)
	assert.NoError(t, err)
	assert.Equal(t, "admin", got.User)
	assert.Equal(t, "rotated", got.Password.Value)

	// Patch unconditionally replaces, even when the existing entry differs
	// from the new value.
	assert.NoError(t, mgr.Patch(ctx, mac, newCred("root", "newpass")))
	got, err = mgr.Get(ctx, mac)
	assert.NoError(t, err)
	assert.Equal(t, "root", got.User)
	assert.Equal(t, "newpass", got.Password.Value)
}

func TestVaultManager_Keys(t *testing.T) {
	testCases := map[string]struct {
		putPairs    [][2]interface{} // [mac string, *credential.Credential]
		expectCount int
		expectSet   map[string]bool
		expectErr   bool
	}{
		"no entries -> list returns error (no credentials found)": {
			putPairs:  nil,
			expectErr: true,
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
			fake := newFakeVaultKVServer()
			fake.mountPresent = true
			srv := httptest.NewServer(fake.handler())
			defer srv.Close()

			ctx := context.Background()
			mgr := newManagerWithServer(t, srv)

			for _, pair := range tc.putPairs {
				macStr := pair[0].(string)
				cred := pair[1].(*credential.Credential)
				assert.NoError(t, mgr.Put(ctx, parseMAC(t, macStr), cred))
			}

			keys, err := mgr.Keys(ctx)
			if tc.expectErr {
				assert.Error(t, err)
				assert.Nil(t, keys)
				return
			}

			assert.NoError(t, err)
			assert.Equal(t, tc.expectCount, len(keys))

			gotSet := make(map[string]bool, len(keys))
			for _, mac := range keys {
				gotSet[mac.String()] = true
			}
			assert.Equal(t, tc.expectSet, gotSet)
		})
	}
}
