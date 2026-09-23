// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package providerapi

import (
	"context"
	"errors"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"gopkg.in/yaml.v3"
)

type testProvider struct {
	name string
}

func (p testProvider) Name() string {
	return p.name
}

type closeableTestProvider struct {
	testProvider
	closed bool
	err    error
}

func (p *closeableTestProvider) Close() error {
	p.closed = true
	return p.err
}

func TestProviderRegistry_Close(t *testing.T) {
	for name, tc := range map[string]struct{ fail, nilRegistry bool }{
		"successful cleanup":            {},
		"cleanup continues after error": {fail: true},
		"nil registry":                  {nilRegistry: true},
	} {
		t.Run(name, func(t *testing.T) {
			if tc.nilRegistry {
				var registry *ProviderRegistry
				require.NoError(t, registry.Close())
				return
			}
			registry := NewProviderRegistry()
			first := &closeableTestProvider{testProvider: testProvider{name: "first"}}
			second := &closeableTestProvider{testProvider: testProvider{name: "second"}}
			if tc.fail {
				first.err = errors.New("close failed")
			}
			require.NoError(t, registry.Register(first))
			require.NoError(t, registry.Register(second))
			require.NoError(t, registry.Register(testProvider{name: "no resources"}))
			err := registry.Close()
			if tc.fail {
				require.ErrorIs(t, err, first.err)
			} else {
				require.NoError(t, err)
			}
			assert.True(t, first.closed)
			assert.True(t, second.closed)
			assert.Empty(t, registry.List())
			require.NoError(t, registry.Close())
		})
	}
}

type testProviderConfig struct {
	name string
}

func (c testProviderConfig) Name() string {
	return c.name
}

func (c testProviderConfig) NewProvider(context.Context) (Provider, error) {
	return testProvider{name: c.name}, nil
}

type testProviderConfigDecoder struct {
	name string
}

func (d testProviderConfigDecoder) Name() string {
	return d.name
}

func (d testProviderConfigDecoder) DefaultConfig() ProviderConfig {
	return testProviderConfig{name: d.name}
}

func (d testProviderConfigDecoder) DecodeYAML(raw yaml.Node) (ProviderConfig, error) {
	return d.DefaultConfig(), nil
}

func TestProviderConfigDecoderRegistry(t *testing.T) {
	registry := NewProviderConfigDecoderRegistry()
	decoder := testProviderConfigDecoder{name: "test"}

	require.NoError(t, registry.Register(decoder))
	err := registry.Register(decoder)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrProviderConfigDecoderAlreadyRegistered))

	var duplicateErr ProviderConfigDecoderAlreadyRegisteredError
	require.True(t, errors.As(err, &duplicateErr))
	assert.Equal(t, "test", duplicateErr.Name)

	got, ok := registry.Get("test")
	require.True(t, ok)
	assert.Equal(t, "test", got.Name())

	_, ok = registry.Get("missing")
	assert.False(t, ok)

	assert.ElementsMatch(t, []string{"test"}, registry.List())

	config := got.DefaultConfig()
	provider, err := config.NewProvider(context.Background())
	require.NoError(t, err)
	assert.Equal(t, "test", provider.Name())
}

func TestProviderConfigDecoderRegistryRegisterValidation(t *testing.T) {
	registry := NewProviderConfigDecoderRegistry()

	err := registry.Register(nil)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrProviderConfigDecoderNotConfigured))

	err = registry.Register(testProviderConfigDecoder{})
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrProviderConfigDecoderNameEmpty))

	var nilRegistry *ProviderConfigDecoderRegistry
	err = nilRegistry.Register(testProviderConfigDecoder{name: "test"})
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrProviderConfigDecoderRegistryNotConfigured))
}

func TestProviderConfigDecoderRegistryNilReadMethods(t *testing.T) {
	var registry *ProviderConfigDecoderRegistry

	decoder, ok := registry.Get("missing")

	assert.Nil(t, decoder)
	assert.False(t, ok)
	assert.Nil(t, registry.List())
}

func TestProviderRegistry(t *testing.T) {
	registry := NewProviderRegistry()
	provider := testProvider{name: "test"}

	require.NoError(t, registry.Register(provider))
	err := registry.Register(provider)
	require.Error(t, err)
	assert.True(t, errors.Is(err, ErrDuplicateProvider))

	var duplicateErr DuplicateProviderError
	require.True(t, errors.As(err, &duplicateErr))
	assert.Equal(t, "test", duplicateErr.Name)

	assert.Equal(t, provider, registry.Get("test"))
	assert.Nil(t, registry.Get("missing"))
	assert.True(t, registry.Has("test"))
	assert.False(t, registry.Has("missing"))
	assert.ElementsMatch(t, []string{"test"}, registry.List())
}

func TestProviderRegistryNilReadMethods(t *testing.T) {
	var registry *ProviderRegistry

	assert.Nil(t, registry.Get("missing"))
	assert.False(t, registry.Has("missing"))
	assert.Nil(t, registry.List())
}
