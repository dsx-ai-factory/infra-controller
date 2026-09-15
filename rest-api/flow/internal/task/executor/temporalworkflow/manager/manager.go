// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"fmt"
	"slices"

	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/otel"
	temporalactivity "go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/contrib/opentelemetry"
	"go.temporal.io/sdk/worker"
	temporalworkflow "go.temporal.io/sdk/workflow"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/clients/temporal"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/capabilityrequirements"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/activity"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/workflow"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const (
	WorkflowQueue = "flow-tasks"
)

// Config holds all configuration required to build a Temporal-backed executor.
// WorkerOptions maps Temporal task-queue names to per-queue worker settings;
// each key results in a dedicated worker started by Build.
type Config struct {
	ClientConf    temporal.Config
	WorkerOptions map[string]worker.Options

	// ComponentManagerRegistry is the registry containing initialized component managers.
	ComponentManagerRegistry *componentmanager.Registry

	// DataCipher decrypts optional sensitive operation fields only inside the
	// final activity that needs their plaintext value. A nil cipher leaves
	// operations without authentication data available.
	DataCipher *secret.Cipher
}

// Validate checks that the configuration is complete and consistent.
// It validates the Temporal client config.
func (c *Config) Validate() error {
	if c == nil {
		return errors.New("configuration for Temporal Executor is nil")
	}

	if err := c.ClientConf.Validate(); err != nil {
		return err
	}

	if len(c.WorkerOptions) == 0 {
		return errors.New("at least one Temporal worker queue must be configured")
	}

	if _, ok := c.WorkerOptions[WorkflowQueue]; !ok {
		return fmt.Errorf(
			"worker options must include workflow queue %q",
			WorkflowQueue,
		)
	}
	return nil
}

// Manager is the Temporal implementation of executor.Executor. It owns two
// Temporal clients (publisher for workflow submission and status queries,
// subscriber for worker registration) and one worker per configured task queue.
type Manager struct {
	conf             Config
	publisherClient  *temporal.Client
	subscriberClient *temporal.Client
	workers          map[string]worker.Worker
}

// Build creates the Temporal executor: it wires the status updater and component
// manager registry into the activity layer, then starts the Temporal clients and
// workers for each configured queue. On success the caller must eventually call
// Stop() to release the Temporal client connections — typically via defer,
// regardless of whether Start() succeeds.
func (c *Config) Build(
	ctx context.Context,
	updater task.TaskStatusUpdater,
	reportUpdater task.TaskReportUpdater,
) (executor.Executor, error) {
	if err := c.Validate(); err != nil {
		return nil, err
	}

	if updater == nil {
		return nil, errors.New("task status updater is required")
	}

	if c.ComponentManagerRegistry == nil {
		return nil, errors.New("component manager registry is required")
	}

	// Bind dependencies into an Activities instance so each manager has its
	// own isolated copy — no shared mutable globals between managers.
	acts := activity.New(
		updater,
		reportUpdater,
		c.ComponentManagerRegistry,
		c.DataCipher,
	)

	publisherClient, err := temporal.New(c.ClientConf)
	if err != nil {
		return nil, err
	}

	subscriberClient, err := temporal.New(c.ClientConf)
	if err != nil {
		publisherClient.Client().Close()
		return nil, err
	}

	// The client interceptor (temporal.New) writes the context onto the
	// workflow; without the matching worker interceptor here it arrives and
	// stops, and every Core call an activity makes starts a fresh root.
	tracingInterceptor, err := opentelemetry.NewTracingInterceptor(
		opentelemetry.TracerOptions{TextMapPropagator: otel.GetTextMapPropagator()})
	if err != nil {
		publisherClient.Client().Close()
		subscriberClient.Client().Close()
		return nil, fmt.Errorf("creating Temporal tracing interceptor: %w", err)
	}

	allActivities := acts.All()
	allWorkflows := workflow.GetAllWorkflows()
	workers := make(map[string]worker.Worker)
	for queue, options := range c.WorkerOptions {
		// options is a copy of the map value, so this does not mutate c.
		options.Interceptors = append(options.Interceptors, tracingInterceptor)
		worker := worker.New(subscriberClient.Client(), queue, options)
		for name, fn := range allActivities {
			worker.RegisterActivityWithOptions(
				fn,
				temporalactivity.RegisterOptions{Name: name},
			)
		}

		for _, wf := range allWorkflows {
			worker.RegisterWorkflowWithOptions(
				wf.WorkflowFunc,
				temporalworkflow.RegisterOptions{Name: wf.WorkflowName},
			)
		}

		workers[queue] = worker
	}

	return &Manager{
		conf:             *c,
		publisherClient:  publisherClient,
		subscriberClient: subscriberClient,
		workers:          workers,
	}, nil
}

// Start begins polling for workflow and activity tasks on all configured queues.
func (m *Manager) Start(ctx context.Context) error {
	started := make([]worker.Worker, 0, len(m.workers))
	for queue, worker := range m.workers {
		log.Info().Msgf("Starting temporal worker for queue %s", queue)
		if err := worker.Start(); err != nil {
			for _, s := range slices.Backward(started) {
				s.Stop()
			}
			// Do not close publisherClient/subscriberClient here: they are
			// owned by the Manager (created in Build, not in Start) and must
			// remain open until Stop() is called. The caller is expected to
			// defer Stop() immediately after a successful Build(), so Stop()
			// will run even when Start() returns an error.
			return fmt.Errorf("failed to start temporal worker: %w", err)
		}
		started = append(started, worker)
		log.Info().Msgf("Temporal worker started for queue %s", queue)
	}

	return nil
}

// Stop is the full teardown for a Manager created by Build: it stops all
// workers (safe to call even if Start was never called or failed partway) and
// closes the Temporal client connections.
func (m *Manager) Stop(ctx context.Context) error {
	for queue, worker := range m.workers {
		log.Info().Msgf("Stopping temporal worker for queue %s", queue)
		worker.Stop()
		log.Info().Msgf("Temporal worker stopped for queue %s", queue)
	}

	m.publisherClient.Client().Close()
	m.subscriberClient.Client().Close()

	return nil
}

// Type returns ExecutorTypeTemporal, identifying this executor implementation.
func (m *Manager) Type() taskcommon.ExecutorType {
	return taskcommon.ExecutorTypeTemporal
}

// CheckStatus decodes the execution ID and queries Temporal for the current
// workflow execution status, mapping it to a TaskStatus.
func (m *Manager) CheckStatus(
	ctx context.Context,
	encodedExecutionID string,
) (taskcommon.TaskStatus, error) {
	executionID, err := common.NewFromEncoded(encodedExecutionID)
	if err != nil {
		return taskcommon.TaskStatusUnknown, err
	}

	// Use empty runID to get the latest execution.
	resp, err := m.publisherClient.Client().DescribeWorkflowExecution(
		ctx,
		executionID.WorkflowID,
		"",
	)
	if err != nil {
		return taskcommon.TaskStatusUnknown, fmt.Errorf(
			"failed to describe temporal workflow execution %s: %v",
			executionID.String(),
			err,
		)
	}

	return taskStatusFromTemporalWorkflowStatus(
		resp.GetWorkflowExecutionInfo().GetStatus(),
	), nil
}

// TerminateTask terminates the Temporal workflow backing the given execution ID.
func (m *Manager) TerminateTask(
	ctx context.Context,
	encodedExecutionID string,
	reason string,
) error {
	executionID, err := common.NewFromEncoded(encodedExecutionID)
	if err != nil {
		return fmt.Errorf("invalid execution ID %q: %w", encodedExecutionID, err)
	}

	// Empty runID targets the latest run.
	// ignoreNotFound: workflow already completed/terminated before this call.
	return ignoreNotFound(m.publisherClient.Client().TerminateWorkflow(
		ctx,
		executionID.WorkflowID,
		"",
		reason,
	))
}

// validateRequiredCapabilities verifies that the active component managers can
// execute all capabilities required by info before dispatching a workflow.
func validateRequiredCapabilities(
	info task.ExecutionInfo,
	registry *componentmanager.Registry,
) error {
	requirements, err := capabilityrequirements.Required(
		info.RuleDefinition,
		executionComponentTypes(info.Components),
	)
	if err != nil {
		return err
	}

	for _, r := range requirements {
		if err := r.Validate(registry); err != nil {
			return err
		}
	}

	return nil
}

// executionComponentTypes returns the unique component types targeted by the
// task, sorted so capability validation receives deterministic input.
func executionComponentTypes(
	components []task.WorkflowComponent,
) []devicetypes.ComponentType {
	seen := make(map[devicetypes.ComponentType]struct{}, len(components))
	types := make([]devicetypes.ComponentType, 0, len(components))
	for _, c := range components {
		if _, ok := seen[c.Type]; ok {
			continue
		}

		seen[c.Type] = struct{}{}
		types = append(types, c.Type)
	}

	slices.Sort(types)

	return types
}

// Execute dispatches the task to the Temporal workflow registered for its
// OperationType. All Temporal mechanics (client, options, workflow submission)
// are contained here — nothing engine-specific crosses the Executor boundary.
func (m *Manager) Execute(
	ctx context.Context,
	req *task.ExecutionRequest,
) (*task.ExecutionResponse, error) {
	if req == nil {
		return nil, errors.New("execution request is nil")
	}

	if err := req.Validate(); err != nil {
		return nil, fmt.Errorf("invalid execution request: %w", err)
	}

	desc, ok := workflow.Get(req.Info.OperationType)
	if !ok {
		return nil, fmt.Errorf(
			"no workflow registered for task type %q (registered types: %v) — "+
				"ensure the workflow package is imported and its init() runs",
			req.Info.OperationType,
			workflow.RegisteredTaskTypes(),
		)
	}

	// Fail fast before submitting a Temporal workflow that the active component
	// managers cannot execute. Activity-level checks still enforce the same
	// capability contract at the operation boundary.
	err := validateRequiredCapabilities(req.Info, m.conf.ComponentManagerRegistry)
	if err != nil {
		return nil, fmt.Errorf(
			"component manager capability pre-dispatch validation failed: %w",
			err,
		)
	}

	return executeWorkflow(ctx, m.publisherClient.Client(), desc, req)
}
