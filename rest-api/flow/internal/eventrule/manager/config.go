// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	eventexecutor "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
	eventscheduler "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/scheduler"
	inventoryresolver "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/resolver"
	"github.com/google/uuid"
)

// HandlingReader retrieves the handling history produced by event-rule
// processing. Its implementation may combine event-rule execution data with
// work created in another subsystem.
type HandlingReader interface {
	GetEventHandling(
		context.Context,
		eventrule.Type,
		eventrule.ResourceKind,
		uuid.UUID,
		int,
		*eventrule.HandlingCursor,
	) ([]eventrule.Handling, bool, error)
}

// SchedulerConfig identifies the manager-owned scheduler and controls its
// runtime mechanics and execution policy.
type SchedulerConfig struct {
	InstanceID string
	Runtime    eventscheduler.RuntimeConfig
	Policy     eventscheduler.PolicyConfig
}

// Validate checks the scheduler identity, runtime mechanics, and policy.
func (c SchedulerConfig) Validate() error {
	if err := eventrule.ValidateExecutionClaimOwner(c.InstanceID); err != nil {
		return fmt.Errorf("scheduler instance ID: %w", err)
	}

	if err := c.Runtime.Validate(); err != nil {
		return err
	}

	return c.Policy.Validate()
}

// Config contains the external capabilities used to assemble an event-rule
// manager. Internal registries, the processor, and the scheduler are
// constructed by New.
type Config struct {
	Store          eventrule.Store
	Scheduler      SchedulerConfig
	Inventory      inventoryresolver.InventoryReader
	TaskManager    eventexecutor.TaskManager
	AlertSender    eventexecutor.AlertSender
	HandlingReader HandlingReader
}

// Validate checks that the manager can assemble a complete internal runtime.
func (c Config) Validate() error {
	if c.Store == nil {
		return fmt.Errorf("event-rule store is required")
	}

	if err := c.Scheduler.Validate(); err != nil {
		return err
	}

	if c.Inventory == nil {
		return fmt.Errorf("inventory reader is required")
	}

	if c.TaskManager == nil {
		return fmt.Errorf("task manager is required")
	}
	if c.HandlingReader == nil {
		return fmt.Errorf("event handling reader is required")
	}

	return nil
}
