// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processor

import (
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/target"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
)

// ExecutionNotifier hints that newly committed pending work is available. Notify
// must not block; periodic scheduler polling remains the reliability fallback.
type ExecutionNotifier interface {
	Notify()
}

// Config contains the dependencies for a Processor.
type Config struct {
	Inventory inventoryresolver.InventoryReader
	Rules     RuleResolver
	Store     eventrule.EventPlanStore
	Targets   *target.Registry
	Notifier  ExecutionNotifier
}

// Validate checks that all required processor dependencies are present.
func (c Config) Validate() error {
	if c.Inventory == nil {
		return fmt.Errorf("inventory reader is required")
	}
	if c.Rules == nil {
		return fmt.Errorf("rule resolver is required")
	}
	if c.Store == nil {
		return fmt.Errorf("event plan store is required")
	}
	if c.Targets == nil {
		return fmt.Errorf("target resolver registry is required")
	}

	return nil
}
