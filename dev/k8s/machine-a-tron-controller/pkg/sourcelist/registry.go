// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package sourcelist publishes the set of discovered machine-a-tron instances
// over a pod-local HTTP endpoint so that sidecar containers (the protocol
// gateway) can consume it without Kubernetes API credentials.
//
// Contract (v1):
//
//	GET /v1/sources -> 200 application/json
//	{
//	  "generation": <uint64>,   // increments only when the set of (name, base_url) changes
//	  "ready": <bool>,          // true once the first discovery completed
//	  "sources": [ {"name": "...", "base_url": "https://...", "pod": "..."} ]  // sorted by name
//	}
//	GET /v1/healthz -> 200 when the controller is serving
//
// The generation is an in-memory counter that restarts from 0 with the
// process, so it can repeat a value for a different set after a controller
// restart. Consumers compare the (name, base_url) set and treat the generation
// as a change hint only.
package sourcelist

import (
	"sort"
	"strings"
	"sync"
	"time"
)

// DefaultDebounce is the default time a changed source set must have been
// pending before a later observation of the same set publishes it.
const DefaultDebounce = 5 * time.Second

// Source is one machine-a-tron identity as seen by discovery.
type Source struct {
	// Name is the machine-a-tron identity (the discovered Service name).
	Name string `json:"name"`
	// BaseURL is the scheme, host and port of the machine-a-tron API.
	BaseURL string `json:"base_url"`
	// Pod is the machine-a-tron pod name, or empty when unknown.
	Pod string `json:"pod"`
}

// Response is the JSON body returned by GET /v1/sources.
type Response struct {
	Generation uint64   `json:"generation"`
	Ready      bool     `json:"ready"`
	Sources    []Source `json:"sources"`
}

// Registry holds the published source list and applies debouncing to changes.
//
// The generation counter increments only when the sorted set of
// (name, base_url) pairs changes. A changed set becomes pending when it is
// first observed and is published by a later observation that reports the
// same set at least the debounce period after the first sighting. Discovery
// is the only observer, so a change is confirmed by the next discovery pass
// that still sees it, never by the clock alone: a set that reverts before the
// next pass is never published. Pod name changes are reflected in the
// response without bumping the generation. The very first observation is
// published immediately so consumers waiting for readiness are not delayed by
// the debounce.
type Registry struct {
	debounce time.Duration
	now      func() time.Time

	mu           sync.Mutex
	ready        bool
	generation   uint64
	published    []Source
	publishedKey string

	pending      []Source
	pendingKey   string
	pendingSince time.Time
}

// Option configures a Registry.
type Option func(*Registry)

// WithDebounce sets the debounce period. A non-positive value disables
// debouncing so every change is published by the observation that reports it.
func WithDebounce(d time.Duration) Option {
	return func(r *Registry) { r.debounce = d }
}

// NewRegistry creates an empty, not-ready Registry.
func NewRegistry(opts ...Option) *Registry {
	r := &Registry{
		debounce:  DefaultDebounce,
		now:       time.Now,
		published: []Source{},
	}
	for _, opt := range opts {
		opt(r)
	}
	return r
}

// Observe records one complete discovery result. Callers must pass the full
// set of currently discovered sources on every call; sources absent from the
// slice are treated as removed.
func (r *Registry) Observe(sources []Source) {
	normalized := normalize(sources)
	key := setKey(normalized)
	now := r.now()

	r.mu.Lock()
	defer r.mu.Unlock()

	if !r.ready {
		r.publishLocked(normalized, key)
		return
	}

	if key == r.publishedKey {
		// Same set of (name, base_url); refresh pod names and drop any
		// pending change that has since been reverted.
		r.published = normalized
		r.clearPendingLocked()
		return
	}

	if r.pending == nil || key != r.pendingKey {
		// First sighting of this set: it becomes pending and waits for a
		// later observation to confirm it, unless debouncing is off.
		r.pending = normalized
		r.pendingKey = key
		r.pendingSince = now
		if r.debounce <= 0 {
			r.publishLocked(r.pending, r.pendingKey)
		}
		return
	}

	// The same pending set observed again; keep the original pendingSince
	// but refresh pod names. It is published once the window has elapsed.
	r.pending = normalized
	if now.Sub(r.pendingSince) >= r.debounce {
		r.publishLocked(r.pending, r.pendingKey)
	}
}

// publishLocked replaces the published set and bumps the generation.
// Callers must hold r.mu.
func (r *Registry) publishLocked(sources []Source, key string) {
	r.generation++
	r.ready = true
	r.published = sources
	r.publishedKey = key
	r.clearPendingLocked()
}

// clearPendingLocked discards any pending change. Callers must hold r.mu.
func (r *Registry) clearPendingLocked() {
	r.pending = nil
	r.pendingKey = ""
	r.pendingSince = time.Time{}
}

// Snapshot returns a copy of the currently published state.
func (r *Registry) Snapshot() Response {
	r.mu.Lock()
	defer r.mu.Unlock()

	out := make([]Source, len(r.published))
	copy(out, r.published)
	return Response{
		Generation: r.generation,
		Ready:      r.ready,
		Sources:    out,
	}
}

// normalize returns a sorted copy of sources. The result is never nil so the
// JSON encoding is always an array.
func normalize(sources []Source) []Source {
	out := make([]Source, len(sources))
	copy(out, sources)
	sort.Slice(out, func(i, j int) bool {
		if out[i].Name != out[j].Name {
			return out[i].Name < out[j].Name
		}
		return out[i].BaseURL < out[j].BaseURL
	})
	return out
}

// setKey builds a canonical string identifying the set of (name, base_url)
// pairs. Pod names are deliberately excluded.
func setKey(sorted []Source) string {
	var b strings.Builder
	for _, s := range sorted {
		b.WriteString(s.Name)
		b.WriteByte(0)
		b.WriteString(s.BaseURL)
		b.WriteByte(0)
	}
	return b.String()
}
