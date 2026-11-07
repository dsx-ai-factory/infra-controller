// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nico

import (
	"errors"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"gopkg.in/yaml.v3"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/nicoapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
)

type closeTrackingClient struct {
	nicoapi.Client
	closed bool
}

func (c *closeTrackingClient) Close() error {
	c.closed = true
	return nil
}

func TestProvider_Close(t *testing.T) {
	client := &closeTrackingClient{}
	provider := NewFromClient(client)
	require.NoError(t, provider.Close())
	assert.True(t, client.closed)
}

func TestConfigName(t *testing.T) {
	assert.Equal(t, ProviderName, (&Config{}).Name())
}

func TestConfigDecoderDecodeYAML(t *testing.T) {
	decoder := ConfigDecoder{}

	decoded, err := decoder.DecodeYAML(yaml.Node{})
	require.NoError(t, err)
	config := decoded.(*Config)
	assert.Equal(t, DefaultTimeout, config.Timeout)

	decoded, err = decoder.DecodeYAML(providerYAMLNode(t, `
timeout: 15s
`))
	require.NoError(t, err)
	config = decoded.(*Config)
	assert.Equal(t, 15*time.Second, config.Timeout)

	_, err = decoder.DecodeYAML(providerYAMLNode(t, `timeout: nope`))
	require.Error(t, err)
	assert.True(t, errors.Is(err, providerapi.ErrInvalidProviderConfigField))
	assertInvalidConfigField(t, err, "timeout")

	_, err = decoder.DecodeYAML(providerYAMLNode(t, `timout: 15s`))
	require.Error(t, err)
	assert.True(t, errors.Is(err, providerapi.ErrInvalidProviderConfig))

	var configErr providerapi.InvalidProviderConfigError
	require.True(t, errors.As(err, &configErr))
	assert.Equal(t, ProviderName, configErr.Provider)

	_, err = decoder.DecodeYAML(providerYAMLNode(t, `compute_power_delay: 0s`))
	require.Error(t, err)
	assert.True(t, errors.Is(err, providerapi.ErrInvalidProviderConfig))
}

func assertInvalidConfigField(t *testing.T, err error, field string) {
	t.Helper()

	var fieldErr providerapi.InvalidProviderConfigFieldError
	require.True(t, errors.As(err, &fieldErr))
	assert.Equal(t, ProviderName, fieldErr.Provider)
	assert.Equal(t, field, fieldErr.Field)
}

func providerYAMLNode(t *testing.T, data string) yaml.Node {
	t.Helper()

	var node yaml.Node
	require.NoError(t, yaml.Unmarshal([]byte(data), &node))
	require.NotEmpty(t, node.Content)
	return *node.Content[0]
}
