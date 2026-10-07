// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"fmt"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// Registry stores one executor for each action type.
type Registry struct {
	executors map[eventrule.ActionType]Executor
}

// Config contains the dependencies used to assemble the supported executors.
type Config struct {
	TaskManager TaskManager
	AlertSender AlertSender
}

// Validate checks that the dependencies for required executors are present.
func (c Config) Validate() error {
	if c.TaskManager == nil {
		return fmt.Errorf("task manager is required")
	}
	return nil
}

// New constructs a registry containing every supported executor available for
// the configured capabilities.
func New(config Config) (*Registry, error) {
	if err := config.Validate(); err != nil {
		return nil, err
	}

	registry := &Registry{
		executors: make(map[eventrule.ActionType]Executor),
	}

	if err := registry.register(eventrule.ActionTypeNoop, NoopExecutor{}); err != nil {
		return nil, err
	}

	taskExecutor := &TaskExecutor{manager: config.TaskManager}
	if err := registry.register(eventrule.ActionTypeSubmitTask, taskExecutor); err != nil {
		return nil, err
	}

	if config.AlertSender != nil {
		alertExecutor := &AlertExecutor{sender: config.AlertSender}
		if err := registry.register(eventrule.ActionTypeSendAlert, alertExecutor); err != nil {
			return nil, err
		}
	}

	return registry, nil
}

func (r *Registry) register(actionType eventrule.ActionType, executor Executor) error {
	if r == nil {
		return fmt.Errorf("executor registry is required")
	}

	if err := actionType.Validate(); err != nil {
		return err
	}

	if executor == nil {
		return fmt.Errorf("%s executor is required", actionType)
	}

	if _, ok := r.executors[actionType]; ok {
		return fmt.Errorf("executor for action type %q is already registered", actionType)
	}

	r.executors[actionType] = executor

	return nil
}

// ValidateActions validates each action and checks that its action type has a
// registered executor.
func (r *Registry) ValidateActions(actions []eventrule.Action) error {
	if r == nil {
		return fmt.Errorf("executor registry is required")
	}

	for i, action := range actions {
		if err := action.Validate(); err != nil {
			return fmt.Errorf("actions[%d]: %w", i, err)
		}

		if _, ok := r.executors[action.Spec.Type()]; !ok {
			return fmt.Errorf(
				"actions[%d]: no executor registered for action type %q",
				i,
				action.Spec.Type(),
			)
		}
	}

	return nil
}

// Executor validates the action type and returns its registered executor.
func (r *Registry) Executor(actionType eventrule.ActionType) (Executor, error) {
	if r == nil {
		return nil, fmt.Errorf("executor registry is required")
	}

	if err := actionType.Validate(); err != nil {
		return nil, err
	}

	executor, ok := r.executors[actionType]
	if !ok {
		return nil, fmt.Errorf("no executor registered for action type %q", actionType)
	}

	return executor, nil
}
