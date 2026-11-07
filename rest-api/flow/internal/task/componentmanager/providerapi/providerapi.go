// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package providerapi contains provider abstractions that must be shared
// between the componentmanager package and provider implementation packages
// without creating an import cycle.
package providerapi

import (
	"bytes"
	"context"
	"fmt"
	"strings"
	"sync"

	"gopkg.in/yaml.v3"
)

// Provider is a marker interface for API client providers.
// Each provider wraps an API client and exposes it to component manager
// implementations.
type Provider interface {
	// Name returns the unique identifier for this provider type.
	Name() string
}

// ProviderConfig is a decoded provider-specific configuration that can create
// its provider.
type ProviderConfig interface {
	// Name returns the provider name for this config.
	Name() string

	// NewProvider creates a provider using this config.
	NewProvider(context.Context) (Provider, error)
}

// ProviderConfigDecoder owns provider-specific config defaults and YAML
// decoding. Provider construction belongs to the decoded ProviderConfig.
type ProviderConfigDecoder interface {
	// Name returns the provider name handled by this decoder.
	Name() string

	// DefaultConfig returns a typed default config for this provider.
	DefaultConfig() ProviderConfig

	// DecodeYAML decodes a provider-specific YAML node into a typed config.
	DecodeYAML(raw yaml.Node) (ProviderConfig, error)
}

// DecodeYAMLStrict decodes a YAML node into out and rejects unknown fields.
// An empty node is treated as "no provider-specific YAML"; callers keep their
// default config values in that case.
func DecodeYAMLStrict(raw yaml.Node, out any) error {
	if raw.Kind == 0 {
		return nil
	}

	data, err := yaml.Marshal(&raw)
	if err != nil {
		return fmt.Errorf("marshal YAML node: %w", err)
	}

	decoder := yaml.NewDecoder(bytes.NewReader(data))
	decoder.KnownFields(true)

	return decoder.Decode(out)
}

// ProviderConfigDecoderRegistry manages provider config decoders by provider name.
type ProviderConfigDecoderRegistry struct {
	mu       sync.RWMutex
	decoders map[string]ProviderConfigDecoder
}

// NewProviderConfigDecoderRegistry creates a new ProviderConfigDecoderRegistry instance.
func NewProviderConfigDecoderRegistry() *ProviderConfigDecoderRegistry {
	return &ProviderConfigDecoderRegistry{
		decoders: make(map[string]ProviderConfigDecoder),
	}
}

// Register adds a provider config decoder to the registry.
// It returns an error when the decoder cannot be registered so bootstrap code
// can fail fast instead of losing the failure in logs.
func (r *ProviderConfigDecoderRegistry) Register(decoder ProviderConfigDecoder) error {
	if r == nil {
		return ErrProviderConfigDecoderRegistryNotConfigured
	}

	if decoder == nil {
		return ErrProviderConfigDecoderNotConfigured
	}

	name := strings.TrimSpace(decoder.Name())
	if name == "" {
		return ErrProviderConfigDecoderNameEmpty
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if _, exists := r.decoders[name]; exists {
		return ProviderConfigDecoderAlreadyRegisteredError{Name: name}
	}

	r.decoders[name] = decoder
	return nil
}

// Get retrieves a provider config decoder by name. A nil registry behaves like
// an empty registry.
func (r *ProviderConfigDecoderRegistry) Get(name string) (ProviderConfigDecoder, bool) {
	if r == nil {
		return nil, false
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	decoder, ok := r.decoders[name]
	return decoder, ok
}

// List returns the names of all registered provider config decoders. A nil
// registry behaves like an empty registry.
func (r *ProviderConfigDecoderRegistry) List() []string {
	if r == nil {
		return nil
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	names := make([]string, 0, len(r.decoders))
	for name := range r.decoders {
		names = append(names, name)
	}
	return names
}
