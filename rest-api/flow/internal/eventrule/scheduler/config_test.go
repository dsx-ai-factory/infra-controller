// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"testing"
	"time"

	"github.com/stretchr/testify/require"
)

func TestConfigValidate(t *testing.T) {
	tests := map[string]struct {
		mutate  func(*Config)
		wantErr string
	}{
		"valid": {},
		"missing store": {
			mutate:  func(config *Config) { config.Dependencies.Store = nil },
			wantErr: "scheduler store is required",
		},
		"missing executor registry": {
			mutate:  func(config *Config) { config.Dependencies.Executors = nil },
			wantErr: "executor registry is required",
		},
		"invalid poll interval": {
			mutate:  func(config *Config) { config.Runtime.PollInterval = 0 },
			wantErr: "scheduler poll interval must be positive",
		},
		"missing instance id": {
			mutate:  func(config *Config) { config.InstanceID = "" },
			wantErr: "execution claim owner is empty",
		},
		"invalid pending worker count": {
			mutate: func(config *Config) {
				config.Runtime.Lanes["pending"] = LaneConfig{ScanLimit: 1}
			},
			wantErr: "pending worker count must be positive",
		},
		"invalid deferred worker count": {
			mutate: func(config *Config) {
				config.Runtime.Lanes["deferred"] = LaneConfig{ScanLimit: 1}
			},
			wantErr: "deferred worker count must be positive",
		},
		"invalid pending scan limit": {
			mutate: func(config *Config) {
				config.Runtime.Lanes["pending"] = LaneConfig{Workers: 1}
			},
			wantErr: "pending scan limit must be positive",
		},
		"invalid deferred scan limit": {
			mutate: func(config *Config) {
				config.Runtime.Lanes["deferred"] = LaneConfig{Workers: 1}
			},
			wantErr: "deferred scan limit must be positive",
		},
		"missing pending lane uses default": {
			mutate: func(config *Config) { delete(config.Runtime.Lanes, "pending") },
		},
		"nil lane map uses defaults": {
			mutate: func(config *Config) { config.Runtime.Lanes = nil },
		},
		"unsupported lane": {
			mutate: func(config *Config) {
				config.Runtime.Lanes["unsupported-z"] = LaneConfig{Workers: 1, ScanLimit: 1}
				config.Runtime.Lanes["unsupported-a"] = LaneConfig{Workers: 1, ScanLimit: 1}
			},
			wantErr: `scheduler lane "unsupported-a" is not supported`,
		},
		"invalid persist timeout": {
			mutate:  func(config *Config) { config.Runtime.PersistTimeout = 0 },
			wantErr: "execution persist timeout must be positive",
		},
		"invalid claim duration": {
			mutate:  func(config *Config) { config.Runtime.ClaimDuration = 0 },
			wantErr: "execution claim duration must be positive",
		},
		"invalid max attempts": {
			mutate:  func(config *Config) { config.Policy.MaxAttempts = 0 },
			wantErr: "retry max attempts must be positive",
		},
		"invalid initial delay": {
			mutate:  func(config *Config) { config.Policy.InitialDelay = 0 },
			wantErr: "retry initial delay must be positive",
		},
		"max delay below initial delay": {
			mutate:  func(config *Config) { config.Policy.MaxDelay = time.Second },
			wantErr: "retry max delay must be at least the initial delay",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			config := validConfig()

			if test.mutate != nil {
				test.mutate(&config)
			}

			err := config.Validate()

			if test.wantErr == "" {
				require.NoError(t, err)
				return
			}

			require.EqualError(t, err, test.wantErr)
		})
	}
}

func TestDefaultRuntimeConfig(t *testing.T) {
	expected := RuntimeConfig{
		PollInterval:   time.Minute,
		PersistTimeout: time.Second,
		ClaimDuration:  30 * time.Second,
		Lanes: map[string]LaneConfig{
			"pending":  {Workers: 1, ScanLimit: 1},
			"deferred": {Workers: 1, ScanLimit: 1},
		},
	}

	actual := DefaultRuntimeConfig()

	require.Equal(t, expected, actual)
	require.NoError(t, actual.Validate())

	actual.Lanes["pending"] = LaneConfig{}
	require.Equal(t, expected, DefaultRuntimeConfig())
}

func TestRuntimeConfig_laneConfig(t *testing.T) {
	definition := laneDefinition{
		name:          "test",
		defaultConfig: LaneConfig{Workers: 1, ScanLimit: 2},
	}
	tests := map[string]struct {
		lanes map[string]LaneConfig
		want  LaneConfig
	}{
		"nil map uses default": {
			want: definition.defaultConfig,
		},
		"missing lane uses default": {
			lanes: map[string]LaneConfig{
				"another": {Workers: 2, ScanLimit: 3},
			},
			want: definition.defaultConfig,
		},
		"configured lane overrides default": {
			lanes: map[string]LaneConfig{
				definition.name: {Workers: 4, ScanLimit: 5},
			},
			want: LaneConfig{Workers: 4, ScanLimit: 5},
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			configured := RuntimeConfig{Lanes: test.lanes}

			require.Equal(t, test.want, configured.laneConfig(definition))
		})
	}
}

func TestConfig_runtime(t *testing.T) {
	config := validConfig()
	config.Runtime.Lanes["pending"] = LaneConfig{Workers: 2, ScanLimit: 1}
	delete(config.Runtime.Lanes, "deferred")

	actual := config.runtime()

	require.Equal(t, config.Dependencies.Store, actual.store)
	require.NotNil(t, actual.executors)
	require.Equal(t, config.Policy, actual.policy)
	require.Equal(t, config.Runtime.PollInterval, actual.pollInterval)
	require.Equal(t, config.Runtime.PersistTimeout, actual.persistTimeout)
	require.Equal(t, config.Runtime.ClaimDuration, actual.claimDuration)
	require.Equal(t, 1, cap(actual.wakeCh))
	require.Equal(t, 3, cap(actual.fatalWorkerErrors))
}

func TestNew(t *testing.T) {
	configured, err := New(validConfig())

	require.NoError(t, err)
	require.NotNil(t, configured)
	require.Len(t, configured.lanes, len(laneDefinitions))
	for index, definition := range laneDefinitions {
		require.Equal(t, definition.name, configured.lanes[index].name)
	}

	config := validConfig()
	config.Runtime.Lanes = nil
	configured, err = New(config)

	require.NoError(t, err)
	require.Len(t, configured.lanes, len(laneDefinitions))
	require.Equal(t, 2, cap(configured.runtime.fatalWorkerErrors))

	_, err = New(Config{})

	require.EqualError(t, err, "scheduler store is required")
}
