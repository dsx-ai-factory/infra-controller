// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package credentials

import (
	"strings"
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestConfigValidate(t *testing.T) {
	testCases := map[string]struct {
		cfg         Config
		expectErr   bool
		errContains string
	}{
		"in-memory datastore returns nil": {
			cfg:       Config{DataStoreType: DatastoreTypeInMemory},
			expectErr: false,
		},
		"unknown/empty datastore returns nil": {
			cfg:       Config{}, // zero-value DataStoreType means no extra validation
			expectErr: false,
		},
		"vault datastore with nil VaultConfig returns error": {
			cfg: Config{
				DataStoreType: DatastoreTypeVault,
				VaultConfig:   nil,
			},
			expectErr:   true,
			errContains: "vault config needs to be specified",
		},
		"vault datastore with non-nil VaultConfig delegates to VaultConfig.Validate": {
			cfg: Config{
				DataStoreType: DatastoreTypeVault,
				VaultConfig:   &VaultConfig{Address: "http://127.0.0.1", Token: "x"},
			},
			expectErr: false,
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			err := tc.cfg.Validate()

			if tc.errContains != "" {
				assert.Error(t, err)
				assert.Contains(t, strings.ToLower(err.Error()), strings.ToLower(tc.errContains))
				return
			}

			if tc.expectErr {
				assert.Error(t, err)
			} else {
				assert.NoError(t, err)
			}
		})
	}
}
