// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package memory implements event-rule stores in memory.
package memory

import (
	"sync"
	"time"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
)

// Store implements all event-rule persistence in memory.
type Store struct {
	mu              sync.RWMutex
	rules           map[uuid.UUID]dbmodel.EventRule
	bindings        map[uuid.UUID]dbmodel.EventRuleBinding
	events          map[uuid.UUID]dbmodel.Event
	eventsByKey     map[eventrule.EventKey]uuid.UUID
	executions      map[uuid.UUID]*memoryExecution
	executionsByKey map[eventrule.ExecutionKey]uuid.UUID
	now             func() time.Time
}

// New constructs an empty in-memory store.
func New() *Store {
	return NewWithClock(time.Now)
}

// NewWithClock constructs an empty in-memory store with an injected
// authoritative clock.
func NewWithClock(now func() time.Time) *Store {
	if now == nil {
		now = time.Now
	}
	return &Store{
		rules:           make(map[uuid.UUID]dbmodel.EventRule),
		bindings:        make(map[uuid.UUID]dbmodel.EventRuleBinding),
		events:          make(map[uuid.UUID]dbmodel.Event),
		eventsByKey:     make(map[eventrule.EventKey]uuid.UUID),
		executions:      make(map[uuid.UUID]*memoryExecution),
		executionsByKey: make(map[eventrule.ExecutionKey]uuid.UUID),
		now:             now,
	}
}

var _ eventrule.Store = (*Store)(nil)
