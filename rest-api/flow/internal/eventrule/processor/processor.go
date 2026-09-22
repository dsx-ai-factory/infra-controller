// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
)

// Processor orchestrates event enrichment, rule selection, and processing.
type Processor struct {
	inventory *inventoryresolver.Resolver
	rules     RuleResolver
	store     eventrule.EventPlanStore
	targets   *target.Registry
	notifier  ExecutionNotifier
}

// New constructs an event processor.
func New(config Config) (*Processor, error) {
	if err := config.Validate(); err != nil {
		return nil, err
	}

	return &Processor{
		inventory: inventoryresolver.New(config.Inventory),
		rules:     config.Rules,
		store:     config.Store,
		targets:   config.Targets,
		notifier:  config.Notifier,
	}, nil
}

// Process deduplicates an envelope and atomically persists its complete event
// plan. The scheduler owns all execution attempts.
func (p *Processor) Process(ctx context.Context, envelope eventrule.Envelope) error {
	prepared, err := p.prepare(ctx, envelope)
	if err != nil || prepared == nil {
		return err
	}

	committed, err := p.plan(ctx, prepared)
	if err != nil || committed == nil {
		return err
	}

	if p.notifier != nil {
		p.notifier.Notify()
	}

	return nil
}
