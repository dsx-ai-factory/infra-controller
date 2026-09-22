// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package ingestion registers event sources and delivers their normalized
// envelopes through the event-rule sink.
package ingestion

import (
	"fmt"
	"sync"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// registry owns unique event source names.
type registry struct {
	mu      sync.Mutex
	sources map[string]struct{}
}

func newRegistry() *registry {
	return &registry{sources: make(map[string]struct{})}
}

func (r *registry) register(sourceName string) error {
	if r == nil {
		return fmt.Errorf("event source registry is nil")
	}

	if err := eventrule.ValidateIdentifier("event source name", sourceName); err != nil {
		return err
	}

	r.mu.Lock()
	defer r.mu.Unlock()

	if _, exists := r.sources[sourceName]; exists {
		return fmt.Errorf("event source %q is already registered", sourceName)
	}

	r.sources[sourceName] = struct{}{}

	return nil
}
