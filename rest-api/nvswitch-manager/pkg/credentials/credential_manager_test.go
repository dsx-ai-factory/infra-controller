// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"context"
	"reflect"
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestNewCredentialManager_TypeByConfig(t *testing.T) {
	testCases := map[string]struct {
		cfg             Config
		expectErr       bool
		errContains     string
		checkTypeWithFn func() CredentialManager
	}{
		"in-memory returns in-memory manager type": {
			cfg:       Config{DataStoreType: DatastoreTypeInMemory},
			expectErr: false,
			checkTypeWithFn: func() CredentialManager {
				return NewInMemoryCredentialManager()
			},
		},
		"vault with nil config returns validation error": {
			cfg: Config{
				DataStoreType: DatastoreTypeVault,
				VaultConfig:   nil,
			},
			expectErr:   true,
			errContains: "vault config needs to be specified",
		},
		"unsupported datastore type returns error": {
			cfg: Config{
				DataStoreType: DataStoreType("UnknownType"),
			},
			expectErr:   true,
			errContains: "unsupported datastore type",
		},
		"vault with non-nil config: returned type matches VaultConfig.NewManager()": {
			cfg: Config{
				DataStoreType: DatastoreTypeVault,
				VaultConfig:   &VaultConfig{Address: "http://127.0.0.1", Token: "x"},
			},
			expectErr: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			mgr, err := New(ctx, &tc.cfg)

			if tc.errContains != "" {
				assert.Error(t, err)
				assert.Nil(t, mgr)
				assert.Contains(t, strings.ToLower(err.Error()), strings.ToLower(tc.errContains))
				return
			}
			if tc.expectErr {
				assert.Error(t, err)
				assert.Nil(t, mgr)
				return
			}
			assert.NoError(t, err)
			assert.NotNil(t, mgr)

			if tc.checkTypeWithFn != nil {
				expected := tc.checkTypeWithFn()
				assert.Equal(t, reflect.TypeOf(expected), reflect.TypeOf(mgr))
				return
			}

			if tc.cfg.DataStoreType == DatastoreTypeVault && tc.cfg.VaultConfig != nil {
				expMgr, expErr := tc.cfg.VaultConfig.NewManager()
				assert.NoError(t, expErr)
				assert.NotNil(t, expMgr)
				assert.Equal(t, reflect.TypeOf(expMgr), reflect.TypeOf(mgr))
			}
		})
	}
}
