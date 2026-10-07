// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestConfigValidate(t *testing.T) {
	tests := map[string]struct {
		mutate  func(*Config)
		wantErr string
	}{
		"valid": {},
		"missing inventory reader": {
			mutate:  func(config *Config) { config.Inventory = nil },
			wantErr: "inventory reader is required",
		},
		"missing rule resolver": {
			mutate:  func(config *Config) { config.Rules = nil },
			wantErr: "rule resolver is required",
		},
		"missing event plan store": {
			mutate:  func(config *Config) { config.Store = nil },
			wantErr: "event plan store is required",
		},
		"missing target resolver": {
			mutate:  func(config *Config) { config.Targets = nil },
			wantErr: "target resolver registry is required",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			config := validProcessorConfig(t)

			if test.mutate != nil {
				test.mutate(&config)
			}

			err := config.Validate()

			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)

				return
			}

			require.NoError(t, err)
		})
	}
}

func TestNew(t *testing.T) {
	t.Run("rejects invalid configuration", func(t *testing.T) {
		processor, err := New(Config{})

		require.Error(t, err)
		require.Nil(t, processor)
	})

	t.Run("constructs processor", func(t *testing.T) {
		processor, err := New(validProcessorConfig(t))

		require.NoError(t, err)
		require.NotNil(t, processor)
	})
}
