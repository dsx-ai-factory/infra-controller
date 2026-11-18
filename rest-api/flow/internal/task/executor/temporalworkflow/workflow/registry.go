// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"encoding/json"
	"fmt"
	"sync"
	"time"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
)

// WorkflowDescriptor describes a Temporal workflow.
// It is a pure data struct — no Temporal client dependency.
type WorkflowDescriptor struct {
	// TaskType identifies this workflow for task dispatch via Execute().
	// Zero value (IsZero) means this workflow is internal and not dispatched as a task.
	TaskType taskcommon.TaskType

	// WorkflowName is the Temporal workflow name used in StartWorkflowOptions
	// and RegisterWorkflowWithOptions.
	WorkflowName string

	// WorkflowFunc is the Go workflow function registered with the Temporal worker.
	WorkflowFunc any

	// Timeout is the maximum wall-clock execution time for this workflow type.
	// May be zero for internal workflows where timeout is managed per invocation.
	Timeout time.Duration

	// Unmarshal deserializes the raw OperationInfo bytes into the typed payload
	// that the Temporal workflow function expects as its second argument.
	// Nil for internal workflows that are not dispatched via ExecutionRequest.
	Unmarshal func(raw json.RawMessage) (any, error)
}

// workflowRegistry holds three views of the registered workflows:
//   - descriptors: keyed by TaskType, used for task-dispatch in Execute()
//   - workflows: keyed by Temporal workflow name, used for duplicate detection
//   - ordered: insertion-order slice, used by GetAllWorkflows() for stable iteration
//
// Thread-safe; populated via register() in each init().
type workflowRegistry struct {
	mu          sync.RWMutex
	descriptors map[taskcommon.TaskType]WorkflowDescriptor
	workflows   map[string]any
	ordered     []WorkflowDescriptor
}

// globalRegistry is the package-level workflow registry populated by init() calls at startup.
var globalRegistry = &workflowRegistry{
	descriptors: make(map[taskcommon.TaskType]WorkflowDescriptor),
	workflows:   make(map[string]any),
}

// validate checks that the descriptor is well-formed. WorkflowName and
// WorkflowFunc are required for all descriptors (worker registration and
// child-workflow dispatch). Task-dispatched descriptors (non-zero TaskType)
// additionally require Unmarshal, which is called at dispatch time.
func (d WorkflowDescriptor) validate() error {
	if d.WorkflowName == "" {
		return fmt.Errorf("WorkflowName is required")
	}

	if d.WorkflowFunc == nil {
		return fmt.Errorf("WorkflowFunc is required for %q", d.WorkflowName)
	}

	if !d.TaskType.IsZero() {
		if !d.TaskType.IsValid() {
			return fmt.Errorf("invalid TaskType %q", d.TaskType)
		}

		if d.Unmarshal == nil {
			return fmt.Errorf(
				"missing Unmarshal for task-dispatched workflow %q",
				d.WorkflowName,
			)
		}
	}

	return nil
}

// register adds a WorkflowDescriptor to the registry.
// If desc.TaskType is non-zero, the descriptor is also indexed by task type
// for dispatch via Execute(). Panics on an invalid or duplicate registration.
func register(desc WorkflowDescriptor) {
	if err := desc.validate(); err != nil {
		panic(fmt.Sprintf("invalid workflow descriptor: %v", err))
	}

	globalRegistry.mu.Lock()
	defer globalRegistry.mu.Unlock()

	if !desc.TaskType.IsZero() {
		if _, exists := globalRegistry.descriptors[desc.TaskType]; exists {
			panic(
				fmt.Sprintf(
					"workflow already registered for task type: %s",
					desc.TaskType.String(),
				),
			)
		}
	}

	if _, exists := globalRegistry.workflows[desc.WorkflowName]; exists {
		panic(
			fmt.Sprintf(
				"workflow already registered with name: %s",
				desc.WorkflowName,
			),
		)
	}

	// Fill the regsitry only after all validation checks have passed.
	if !desc.TaskType.IsZero() {
		globalRegistry.descriptors[desc.TaskType] = desc
	}
	globalRegistry.workflows[desc.WorkflowName] = desc.WorkflowFunc
	globalRegistry.ordered = append(globalRegistry.ordered, desc)
}

// Validator is a type constraint for pointer types that expose a Validate method.
// Used by registerTaskWorkflow and unmarshalAndValidate to enforce that the
// concrete operation-info type can be validated at dispatch time.
type Validator[T any] interface {
	*T
	Validate() error
}

// registerTaskWorkflow is the standard entry point for task-dispatched
// workflow registration. It derives Timeout from the operation options for
// taskType and delegates to unmarshalAndValidate for the Unmarshal closure,
// keeping those two contract details in one place instead of repeated across
// every workflow init(). Panics if taskType is zero or invalid — this function
// is only for task-dispatched workflows; use register() directly for internal ones.
func registerTaskWorkflow[T any, PT Validator[T]](
	taskType taskcommon.TaskType,
	workflowName string,
	workflowFunc any,
) {
	if !taskType.IsValid() {
		panic(fmt.Sprintf(
			"registerTaskWorkflow requires a valid TaskType, got %q — "+
				"use register() directly for internal workflows",
			taskType,
		))
	}

	register(WorkflowDescriptor{
		TaskType:     taskType,
		WorkflowName: workflowName,
		WorkflowFunc: workflowFunc,
		Timeout:      operations.GetOperationOptions(taskType).Timeout,
		Unmarshal:    unmarshalAndValidate[T, PT](),
	})
}

// unmarshalAndValidate returns an Unmarshal function that decodes raw JSON into
// a new *T and calls Validate() on the result. This is the single point where
// operation-info validation is enforced for all task-dispatched workflows —
// individual workflow functions do not need to re-validate. The pointer is
// returned as any so Temporal's codec can deserialize it into the workflow's
// typed parameter at dispatch time.
func unmarshalAndValidate[T any, PT Validator[T]]() func(json.RawMessage) (any, error) { //nolint:lll
	return func(raw json.RawMessage) (any, error) {
		var info T
		if err := json.Unmarshal(raw, &info); err != nil {
			return nil, fmt.Errorf("%T: %w", info, err)
		}

		pt := PT(&info)
		if err := pt.Validate(); err != nil {
			return nil, err
		}

		return pt, nil
	}
}

// Get returns the descriptor for a TaskType. ok is false if not registered.
func Get(taskType taskcommon.TaskType) (WorkflowDescriptor, bool) {
	globalRegistry.mu.RLock()
	defer globalRegistry.mu.RUnlock()
	desc, ok := globalRegistry.descriptors[taskType]
	return desc, ok
}

// RegisteredTaskTypes returns the string names of all task-dispatched (non-zero
// TaskType) workflows in registration order, for use in error messages.
func RegisteredTaskTypes() []string {
	globalRegistry.mu.RLock()
	defer globalRegistry.mu.RUnlock()
	var result []string
	for _, desc := range globalRegistry.ordered {
		if !desc.TaskType.IsZero() {
			result = append(result, desc.TaskType.String())
		}
	}
	return result
}

// GetAllWorkflows returns all registered workflows as a snapshot slice in
// registration order, for Temporal worker registration via
// RegisterWorkflowWithOptions. Order is stable across calls: it reflects the
// order in which register() was invoked, not any particular file-processing order.
func GetAllWorkflows() []WorkflowDescriptor {
	globalRegistry.mu.RLock()
	defer globalRegistry.mu.RUnlock()
	result := make([]WorkflowDescriptor, len(globalRegistry.ordered))
	copy(result, globalRegistry.ordered)
	return result
}
