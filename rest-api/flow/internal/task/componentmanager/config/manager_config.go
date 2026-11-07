// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

// This file defines the manager-specific config contract: typed configs, YAML
// decoders, the decoder registry, and shared decoding helpers.

import (
	"bytes"
	"fmt"
	"sync"

	"gopkg.in/yaml.v3"

	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
)

// ManagerConfig is a decoded manager-specific configuration value.
type ManagerConfig interface {
	// Validate verifies this config is valid for the normalized descriptor
	// identity supplied by Config.ManagerConfigs.
	Validate(expectedIdentity cmcatalog.DescriptorIdentity) error
}

// ManagerConfigDecoder owns manager-specific config defaults and YAML decoding
// for one descriptor identity.
type ManagerConfigDecoder interface {
	// Identity returns the descriptor identity handled by this decoder.
	Identity() cmcatalog.DescriptorIdentity

	// DefaultConfig returns a typed default config for this manager.
	DefaultConfig() ManagerConfig

	// DecodeYAML decodes a manager-specific YAML node into a typed config.
	DecodeYAML(raw yaml.Node) (ManagerConfig, error)
}

// ManagerConfigDecoderRegistry manages manager config decoders by descriptor
// identity.
type ManagerConfigDecoderRegistry struct {
	mu       sync.RWMutex
	decoders map[cmcatalog.DescriptorIdentity]ManagerConfigDecoder
}

// NewManagerConfigDecoderRegistry creates a new ManagerConfigDecoderRegistry.
func NewManagerConfigDecoderRegistry() *ManagerConfigDecoderRegistry {
	return &ManagerConfigDecoderRegistry{
		decoders: make(map[cmcatalog.DescriptorIdentity]ManagerConfigDecoder),
	}
}

// Register adds a manager config decoder to the registry using the decoder's
// normalized descriptor identity.
func (r *ManagerConfigDecoderRegistry) Register(decoder ManagerConfigDecoder) error {
	if r == nil {
		return ErrManagerConfigDecoderRegistryNotConfigured
	}

	if decoder == nil {
		return ErrManagerConfigDecoderNotConfigured
	}

	identity, err := decoder.Identity().Normalize()
	if err != nil {
		return err
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if _, exists := r.decoders[identity]; exists {
		return ManagerConfigDecoderAlreadyRegisteredError{Identity: identity}
	}

	r.decoders[identity] = decoder
	return nil
}

// Get retrieves a manager config decoder by normalized descriptor identity.
// Get does not normalize identity; callers should normalize and handle any
// error before calling. Passing an unnormalized identity simply misses and
// returns nil. A nil registry behaves like an empty registry.
func (r *ManagerConfigDecoderRegistry) Get(
	identity cmcatalog.DescriptorIdentity,
) ManagerConfigDecoder {
	if r == nil {
		return nil
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	return r.decoders[identity]
}

// List returns all registered manager config decoder identities, sorted by
// component type and implementation.
func (r *ManagerConfigDecoderRegistry) List() []cmcatalog.DescriptorIdentity {
	if r == nil {
		return nil
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	identities := make([]cmcatalog.DescriptorIdentity, 0, len(r.decoders))
	for identity := range r.decoders {
		identities = append(identities, identity)
	}
	cmcatalog.SortDescriptorIdentities(identities)
	return identities
}

// DecodeYAMLStrict decodes a YAML node into out and rejects unknown fields. An
// empty node is treated as "no manager-specific YAML"; callers keep their
// default config values in that case. The node is accepted by value to match
// ManagerConfigDecoder.DecodeYAML; yaml.Node's nested content is still shared.
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
