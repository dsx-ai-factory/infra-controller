// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package pmcregistry

import (
	"context"
	"reflect"
	"strings"
	"testing"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"

	"github.com/stretchr/testify/assert"
)

func TestPmcRegistryNew(t *testing.T) {
	testCases := map[string]struct {
		cfg         Config
		expectErr   bool
		errContains string
		checkTypeFn func() PmcRegistry
	}{
		"in-memory returns in-memory registry type": {
			cfg: Config{
				DSType: RegisterTypeInMemory,
				DSConf: cdb.Config{}, //nolint:exhaustruct // unused for in-memory
			},
			checkTypeFn: func() PmcRegistry { return NewMemRegistry() },
		},
		"postgres with invalid db config returns error": {
			cfg: Config{
				DSType: RegisterTypePostgres,
				DSConf: cdb.Config{}, //nolint:exhaustruct // zero-value triggers validation error
			},
			expectErr:   true,
			errContains: "host is required",
		},
		"unsupported type returns error": {
			cfg: Config{
				DSType: PmcRegisterType("UnknownType"),
				DSConf: cdb.Config{}, //nolint:exhaustruct // unused for error path
			},
			expectErr:   true,
			errContains: "unsupported datastore type",
		},
	}

	for name, tc := range testCases {
		t.Run(name, func(t *testing.T) {
			ctx := context.Background()
			reg, err := New(ctx, &tc.cfg)

			if tc.errContains != "" {
				assert.Error(t, err)
				assert.Nil(t, reg)
				// case-insensitive substring match
				assert.Contains(t, strings.ToLower(err.Error()), strings.ToLower(tc.errContains))
				return
			}

			if tc.expectErr {
				assert.Error(t, err)
				assert.Nil(t, reg)
				return
			}

			assert.NoError(t, err)
			assert.NotNil(t, reg)

			if tc.checkTypeFn != nil {
				expected := tc.checkTypeFn()
				assert.Equal(t, reflect.TypeOf(expected), reflect.TypeOf(reg))
			}
		})
	}
}
