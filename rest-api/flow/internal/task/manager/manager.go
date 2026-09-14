// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	inventorystore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/inventory/store"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/conflict"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/message"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/inventoryobjects/rack"
)

const (
	defaultMaxWaitingPerRack = 5
	defaultQueueTimeout      = time.Hour
)

// ErrRackConflict marks a rejected task submission caused by an active
// conflicting task on the target rack.
var ErrRackConflict = errors.New("rack conflict")

// Config holds the configuration for the task manager.
type Config struct {
	InventoryStore inventorystore.Store // For rack/component lookups (read-only)
	TaskStore      taskstore.Store      // For task persistence
	ExecutorConfig executor.ExecutorConfig
	// Note: RuleResolver is created internally from TaskStore

	// MaxWaitingTasksPerRack is the maximum number of queued tasks allowed per
	// rack. Zero uses the default (5).
	MaxWaitingTasksPerRack int
	// DefaultQueueTimeout is the expiry duration for tasks that do not supply
	// their own QueueTimeout. Zero uses the default (1 hour).
	DefaultQueueTimeout time.Duration
	// PromoterConfig tunes the Promoter's sweep interval and channel size.
	// Zero values use the Promoter's own defaults.
	PromoterConfig conflict.PromoterConfig
}

func (c *Config) applyDefaults() {
	if c.MaxWaitingTasksPerRack <= 0 {
		c.MaxWaitingTasksPerRack = defaultMaxWaitingPerRack
	}

	if c.DefaultQueueTimeout <= 0 {
		c.DefaultQueueTimeout = defaultQueueTimeout
	}
}

// Validate returns an error if the Config is missing required fields.
func (c *Config) Validate() error {
	if c == nil {
		return fmt.Errorf("configuration is nil")
	}

	if c.InventoryStore == nil {
		return fmt.Errorf("inventory store is required")
	}

	if c.TaskStore == nil {
		return fmt.Errorf("task store is required")
	}

	if c.ExecutorConfig == nil {
		return fmt.Errorf("executor config is required")
	}

	return c.ExecutorConfig.Validate()
}

// Manager defines the public interface for task lifecycle management.
type Manager interface {
	Start(ctx context.Context) error
	Stop(ctx context.Context)
	SubmitTask(ctx context.Context, req *operation.Request) ([]uuid.UUID, error)
	CancelTask(ctx context.Context, taskID uuid.UUID) error
}

// ManagerImpl maintains unfinished tasks, schedules them via temporal workflows,
// and monitors their progress.
type ManagerImpl struct {
	inventoryStore   inventorystore.Store // For rack/component lookups
	taskStore        taskstore.Store      // For task persistence
	executor         executor.Executor
	ruleResolver     *operationrules.Resolver // Resolves operation rules (created internally)
	conflictResolver *conflict.Resolver
	promoter         *conflict.Promoter

	maxWaitingPerRack   int
	defaultQueueTimeout time.Duration

	ctx       context.Context
	cancel    context.CancelFunc
	startOnce sync.Once
	stopOnce  sync.Once
}

// New creates a new task manager.
func New(ctx context.Context, conf *Config) (*ManagerImpl, error) {
	if err := conf.Validate(); err != nil {
		return nil, err
	}
	conf.applyDefaults()

	// Skeleton manager first — promoteTask is a bound method, m must exist.
	m := &ManagerImpl{
		inventoryStore:      conf.InventoryStore,
		maxWaitingPerRack:   conf.MaxWaitingTasksPerRack,
		defaultQueueTimeout: conf.DefaultQueueTimeout,
	}

	// Promoter needs m.promoteTask.
	promoter := conflict.NewPromoter(
		conf.TaskStore, m.promoteTask, conf.PromoterConfig,
	)

	// wrappedStore must exist before executor.New so it can be passed as the
	// updater. Activities use it so completions fire Promoter notifications.
	wrappedStore := newNotifyingTaskStore(conf.TaskStore, promoter)

	// Build executor — updater is passed explicitly, no global involved at this layer.
	exec, err := executor.New(ctx, conf.ExecutorConfig, wrappedStore, wrappedStore)
	if err != nil {
		return nil, err
	}

	m.executor = exec
	m.taskStore = wrappedStore
	m.promoter = promoter
	m.ruleResolver = operationrules.NewResolver(wrappedStore)
	m.conflictResolver = conflict.NewResolver(wrappedStore)

	return m, nil
}

// Start starts the task manager to make it ready to accept tasks.
func (m *ManagerImpl) Start(ctx context.Context) error {
	var startErr error

	m.startOnce.Do(func() {
		if m.executor == nil {
			startErr = fmt.Errorf("executor is required")
			return
		}

		if err := m.executor.Start(ctx); err != nil {
			startErr = fmt.Errorf("failed to start executor: %w", err)
			return
		}

		startCtx, cancel := context.WithCancel(ctx)
		m.ctx = startCtx
		m.cancel = cancel

		m.promoter.Start(startCtx)
	})

	return startErr
}

// Stop shuts down the manager and waits for all routines to finish.
func (m *ManagerImpl) Stop(ctx context.Context) {
	m.stopOnce.Do(func() {
		if m.cancel != nil {
			m.cancel()
		}

		if m.executor != nil {
			if err := m.executor.Stop(ctx); err != nil {
				log.Warn().Err(err).Msg("failed to stop executor")
			}
		}
	})
}

// SubmitTask submits a task to the task manager.
// The TargetSpec is resolved to racks via inventory; one Task is created per
// rack. Returns the IDs of all created tasks.
func (m *ManagerImpl) SubmitTask(
	ctx context.Context,
	req *operation.Request,
) ([]uuid.UUID, error) {
	if req == nil {
		return nil, fmt.Errorf("request is nil")
	}

	if err := req.Validate(); err != nil {
		return nil, err
	}

	// A caller may retry after task submission succeeded but persisting the
	// returned task ID failed. Once the task is scheduled, its idempotency key
	// owns the outcome; mutable inventory and rule state must not turn that
	// retry into a failure.
	if req.HasIdempotencyKey() {
		existing, err := m.taskStore.GetTaskByIdempotencyKey(ctx, req.IdempotencyKey)
		if err != nil {
			return nil, fmt.Errorf("look up idempotent task: %w", err)
		}
		if err := validateIdempotentTaskRack(req, existing); err != nil {
			return nil, err
		}
		if existing != nil && (existing.IsScheduled() ||
			existing.Status == taskcommon.TaskStatusWaiting) {
			return []uuid.UUID{existing.ID}, nil
		}
	}

	// Fail-fast: verify the requested rule exists before creating any tasks.
	// The resolver will check again at execution time (defense-in-depth for
	// queued tasks whose rule may be deleted while waiting).
	if req.RuleID != nil && *req.RuleID != uuid.Nil {
		rule, err := m.taskStore.GetRule(ctx, *req.RuleID)
		if err != nil {
			return nil, fmt.Errorf("requested rule_id %s: %w", req.RuleID, err)
		}
		if rule == nil {
			return nil, fmt.Errorf("requested rule_id %s not found", req.RuleID)
		}
	}

	// Resolve targets to racks with components.
	rackMap, err := resolveTargetSpecToRacks(ctx, m.inventoryStore, &req.TargetSpec)
	if err != nil {
		return nil, err
	}

	if len(rackMap) == 0 {
		return nil, fmt.Errorf("no valid racks found for request")
	}

	if req.RequiredRackID != uuid.Nil {
		if len(rackMap) != 1 {
			return nil, fmt.Errorf(
				"RequiredRackID: components resolved to %d racks, expected exactly rack %s",
				len(rackMap), req.RequiredRackID,
			)
		}
		if _, ok := rackMap[req.RequiredRackID]; !ok {
			var actualID uuid.UUID
			for id := range rackMap {
				actualID = id
			}
			return nil, fmt.Errorf(
				"RequiredRackID: components resolved to rack %s, expected %s",
				actualID, req.RequiredRackID,
			)
		}
	}

	err = m.validateSubmissionRackTargets(ctx, req.Operation, rackMap)
	if err != nil {
		return nil, err
	}

	// Create and execute task for each rack.
	var taskIDs []uuid.UUID
	for _, targetRack := range rackMap {
		taskID, err := m.createAndExecuteTask(ctx, req, targetRack)
		if err != nil {
			log.Error().
				Err(err).
				Str("rack_id", targetRack.Info.ID.String()).
				Msg("failed to create task for rack")

			// RequiredRackID callers (e.g. the schedule dispatcher) depend on
			// exactly one task ID being returned. Fail fast rather than
			// returning nil error with zero IDs, which the dispatcher would
			// misinterpret as a successful no-op.
			if req.RequiredRackID != uuid.Nil {
				return nil, fmt.Errorf(
					"failed to create task for required rack %s: %w",
					targetRack.Info.ID, err,
				)
			}
			continue
		}
		taskIDs = append(taskIDs, taskID)
	}

	return taskIDs, nil
}

// validateSubmissionRackTargets resolves the effective ingest rule for each
// rack before deciding whether unlinked expected components are safe targets.
func (m *ManagerImpl) validateSubmissionRackTargets(
	ctx context.Context,
	op operation.Wrapper,
	rackMap map[uuid.UUID]*rack.Rack,
) error {
	if op.Type != taskcommon.TaskTypeBringUp || op.Code != taskcommon.OpCodeIngest {
		if err := validateResolvedRackTargets(op, nil, rackMap); err != nil {
			return fmt.Errorf("operation cannot be submitted: %w", err)
		}
		return nil
	}

	rackIDs := make([]uuid.UUID, 0, len(rackMap))
	for rackID := range rackMap {
		rackIDs = append(rackIDs, rackID)
	}
	slices.SortFunc(rackIDs, func(a, b uuid.UUID) int {
		return strings.Compare(a.String(), b.String())
	})

	for _, rackID := range rackIDs {
		rule, err := m.resolveOperationRule(ctx, op, rackID)
		if err != nil {
			return err
		}
		if err := validateResolvedRackTargets(
			op,
			&rule.RuleDefinition,
			map[uuid.UUID]*rack.Rack{rackID: rackMap[rackID]},
		); err != nil {
			return fmt.Errorf("operation cannot be submitted: %w", err)
		}
	}

	return nil
}

// validateResolvedRackTargets enforces the boundary between expected
// inventory and actionable actual devices. Expectation-only operations are
// the exception: they intentionally operate on expected components that may
// not have an external ID yet.
func validateResolvedRackTargets(
	op operation.Wrapper,
	ruleDef *operationrules.RuleDefinition,
	rackMap map[uuid.UUID]*rack.Rack,
) error {
	var emptyRacks []string
	var unlinkedComponents []string
	expectationOnly := (op.Type == taskcommon.TaskTypeInjectExpectation &&
		op.Code == taskcommon.OpCodeInjectExpectation) ||
		(op.Type == taskcommon.TaskTypeBringUp &&
			op.Code == taskcommon.OpCodeIngest &&
			ruleUsesOnlyExpectedInventory(ruleDef))

	for rackID, resolvedRack := range rackMap {
		if resolvedRack == nil || len(resolvedRack.Components) == 0 {
			emptyRacks = append(emptyRacks, rackID.String())
			continue
		}
		if expectationOnly {
			continue
		}
		for _, comp := range resolvedRack.Components {
			if comp.ComponentID == "" {
				unlinkedComponents = append(
					unlinkedComponents,
					fmt.Sprintf(
						"rack %s %s/%s",
						rackID,
						devicetypes.ComponentTypeToString(comp.Type),
						comp.Info.ID,
					),
				)
			}
		}
	}

	if len(emptyRacks) > 0 {
		slices.Sort(emptyRacks)
		return fmt.Errorf(
			"racks have no selected components: %s",
			strings.Join(emptyRacks, ", "),
		)
	}
	if len(unlinkedComponents) > 0 {
		slices.Sort(unlinkedComponents)
		return &unlinkedTargetsError{components: unlinkedComponents}
	}

	return nil
}

type unlinkedTargetsError struct {
	components []string
}

func (e *unlinkedTargetsError) Error() string {
	return fmt.Sprintf(
		"selected components not linked to actual inventory (%d): %s",
		len(e.components),
		strings.Join(e.components, ", "),
	)
}

func ruleUsesOnlyExpectedInventory(ruleDef *operationrules.RuleDefinition) bool {
	if ruleDef == nil {
		return false
	}

	foundInjectExpectation := false
	for _, step := range ruleDef.Steps {
		for _, action := range step.OrderedActions() {
			switch action.Name {
			case operationrules.ActionInjectExpectation:
				foundInjectExpectation = true
			case operationrules.ActionSleep:
				// Sleep has no inventory target side effect.
			default:
				return false
			}
		}
	}

	return foundInjectExpectation
}

// createAndExecuteTask creates a task for a single rack and executes it.
func (m *ManagerImpl) createAndExecuteTask(
	ctx context.Context,
	req *operation.Request,
	targetRack *rack.Rack,
) (uuid.UUID, error) {
	if req.HasIdempotencyKey() {
		return m.createAndExecuteIdempotentTask(ctx, req, targetRack)
	}

	task := newTaskForRack(req, targetRack)

	// Check for conflicts inside a transaction to avoid a race between the
	// check and the creation.
	txErr := m.taskStore.RunInTransaction(
		ctx,
		func(txCtx context.Context) error {
			err := m.lockRackAndResolveConflict(txCtx, req, targetRack, &task)
			if err != nil {
				return err
			}

			return m.taskStore.CreateTask(txCtx, &task)
		},
	)

	if txErr != nil {
		return uuid.Nil, txErr
	}

	if task.Status == taskcommon.TaskStatusWaiting {
		log.Info().
			Str("task_id", task.ID.String()).
			Str("rack_id", targetRack.Info.ID.String()).
			Msg("task queued: waiting for rack to become available")
		return task.ID, nil
	}

	// Task executes immediately — resolve rule and run.
	if err := m.resolveAndExecuteTask(ctx, &task, targetRack); err != nil {
		return uuid.Nil, err
	}

	return task.ID, nil
}

func (m *ManagerImpl) createAndExecuteIdempotentTask(
	ctx context.Context,
	req *operation.Request,
	targetRack *rack.Rack,
) (uuid.UUID, error) {
	var task taskdef.Task
	txErr := m.taskStore.RunInTransaction(
		ctx,
		func(txCtx context.Context) error {
			if err := m.taskStore.LockIdempotencyKey(txCtx, req.IdempotencyKey); err != nil {
				return err
			}

			persistedTask, err := m.taskStore.GetTaskByIdempotencyKey(
				txCtx,
				req.IdempotencyKey,
			)
			if err != nil {
				return err
			}

			if persistedTask != nil {
				if err := validateIdempotentTaskRack(req, persistedTask); err != nil {
					return err
				}
				// There are existing tasks with this idempotency key, reuse it.
				task = *persistedTask

			} else {
				// No existing tasks with this idempotency key, create a new one.
				task = newTaskForRack(req, targetRack)
				if err := m.lockRackAndResolveConflict(txCtx, req, targetRack, &task); err != nil {
					return err
				}

				if err := m.taskStore.CreateTask(txCtx, &task); err != nil {
					return err
				}
			}

			if task.IsScheduled() {
				log.Info().
					Str("task_id", task.ID.String()).
					Str("idempotency_key", task.IdempotencyKey).
					Msg("idempotent duplicate: returning existing scheduled task")
				return nil
			}
			if task.Status == taskcommon.TaskStatusWaiting {
				log.Info().
					Str("task_id", task.ID.String()).
					Str("rack_id", targetRack.Info.ID.String()).
					Msg("task queued: waiting for rack to become available")
				return nil
			}

			// Keep the idempotency lock until the execution ID or deferred
			// status is persisted so a concurrent retry cannot execute the
			// same pending task.
			return m.resolveAndExecuteTaskInTransaction(txCtx, &task, targetRack)
		},
	)

	if txErr != nil {
		return uuid.Nil, txErr
	}
	return task.ID, nil
}

func validateIdempotentTaskRack(req *operation.Request, existing *taskdef.Task) error {
	if existing == nil || existing.RackID == req.RequiredRackID {
		return nil
	}
	return fmt.Errorf(
		"idempotency key %q belongs to rack %s, not requested rack %s",
		req.IdempotencyKey,
		existing.RackID,
		req.RequiredRackID,
	)
}

func newTaskForRack(req *operation.Request, targetRack *rack.Rack) taskdef.Task {
	compsByType := make(
		map[devicetypes.ComponentType][]uuid.UUID,
		len(targetRack.Components),
	)
	for _, c := range targetRack.Components {
		compsByType[c.Type] = append(compsByType[c.Type], c.Info.ID)
	}

	return taskdef.Task{
		ID:        uuid.New(),
		Operation: req.Operation,
		RackID:    targetRack.Info.ID,
		Attributes: taskcommon.TaskAttributes{
			ComponentsByType: compsByType,
		},
		Description:    req.Description,
		ExecutorType:   taskcommon.ExecutorTypeUnknown,
		ExecutionID:    "",
		IdempotencyKey: req.IdempotencyKey,
		TriggerType:    req.TriggerType,
		TriggerID:      req.TriggerID,
	}
}

// lockRackAndResolveConflict must be called inside RunInTransaction.
func (m *ManagerImpl) lockRackAndResolveConflict(
	ctx context.Context,
	req *operation.Request,
	targetRack *rack.Rack,
	task *taskdef.Task,
) error {
	// Serialize rack-level admission so concurrent submissions cannot both
	// observe an empty active set and create conflicting pending tasks.
	if err := m.taskStore.LockRack(ctx, targetRack.Info.ID); err != nil {
		return err
	}

	hasConflict, err := m.conflictResolver.HasConflict(ctx, task)
	if err != nil {
		return err
	}

	if !hasConflict {
		// No active task blocks this operation, so it can be scheduled
		// immediately after the transaction commits.
		task.Status = taskcommon.TaskStatusPending
		task.Message = message.ForStatus(taskcommon.TaskStatusPending)
		return nil
	}

	if req.ConflictStrategy != operation.ConflictStrategyQueue {
		// The caller chose rejection over queueing, so surface the conflict
		// without creating a task row.
		return fmt.Errorf(
			"rack %s already has a conflicting task: %w",
			targetRack.Info.ID, ErrRackConflict,
		)
	}

	count, err := m.taskStore.CountWaitingTasksForRack(ctx, targetRack.Info.ID)
	if err != nil {
		return err
	}

	if count >= m.maxWaitingPerRack {
		// Preserve a bounded per-rack queue; otherwise a stuck rack could
		// accumulate unbounded waiting work.
		return fmt.Errorf(
			"rack %s waiting queue is full (%d/%d tasks)",
			targetRack.Info.ID, count, m.maxWaitingPerRack,
		)
	}

	// Queue the task behind the currently active rack work. The promoter will
	// move it to pending once the rack no longer has a conflicting active task.
	task.Status = taskcommon.TaskStatusWaiting
	task.Message = message.ForStatus(taskcommon.TaskStatusWaiting)
	task.QueueExpiresAt = m.getReqExpiresAt(req)

	return nil
}

// promoteTask is invoked by the Promoter to execute a previously waiting task
// that has been promoted to pending.
func (m *ManagerImpl) promoteTask(ctx context.Context, taskID uuid.UUID) error {
	task, err := m.taskStore.GetTask(ctx, taskID)
	if err != nil {
		return fmt.Errorf("promoteTask: failed to load task %s: %w", taskID, err)
	}

	targetRack, err := m.loadRackForTask(ctx, task)
	if err != nil {
		return fmt.Errorf("promoteTask: failed to load rack: %w", err)
	}

	return m.resolveAndExecuteTask(ctx, task, targetRack)
}

// resolveAndExecuteTask resolves the operation rule for a task, executes it,
// and updates the task record with the execution result. It is shared by the
// immediate-execution path in createAndExecuteTask and the promotion path in
// promoteTask.
func (m *ManagerImpl) resolveAndExecuteTask(
	ctx context.Context,
	task *taskdef.Task,
	targetRack *rack.Rack,
) error {
	return m.resolveAndExecuteTaskWithTransaction(ctx, task, targetRack, false)
}

func (m *ManagerImpl) resolveAndExecuteTaskInTransaction(
	ctx context.Context,
	task *taskdef.Task,
	targetRack *rack.Rack,
) error {
	return m.resolveAndExecuteTaskWithTransaction(ctx, task, targetRack, true)
}

func (m *ManagerImpl) resolveAndExecuteTaskWithTransaction(
	ctx context.Context,
	task *taskdef.Task,
	targetRack *rack.Rack,
	transactionActive bool,
) error {
	rule, err := m.resolveOperationRule(ctx, task.Operation, task.RackID)
	if err != nil {
		return fmt.Errorf("failed to resolve operation rule: %w", err)
	}
	if rule == nil {
		return fmt.Errorf("resolver returned nil rule (should never happen)")
	}

	if rule.ID != uuid.Nil {
		task.AppliedRuleID = &rule.ID
		log.Info().
			Str("rule_name", rule.Name).
			Str("rule_id", rule.ID.String()).
			Str("operation_type", string(task.Operation.Type)).
			Str("operation", task.Operation.Code).
			Str("rack_id", task.RackID.String()).
			Msg("Resolved operation rule for task")
	} else {
		log.Info().
			Str("rule_name", rule.Name).
			Str("operation_type", string(task.Operation.Type)).
			Str("operation", task.Operation.Code).
			Str("rack_id", task.RackID.String()).
			Msg("Using hardcoded default rule for task")
	}

	resp, err := m.executeTask(ctx, task, targetRack, &rule.RuleDefinition)
	if err != nil {
		deferred, deferErr := m.deferUnlinkedTask(ctx, task, err, transactionActive)
		if deferred {
			return deferErr
		}
		if uerr := m.taskStore.UpdateTaskStatus(ctx, &taskdef.TaskStatusUpdate{
			ID:      task.ID,
			Status:  taskcommon.TaskStatusFailed,
			Message: message.ForFailure(err),
		}); uerr != nil {
			log.Error().Err(uerr).
				Msgf("failed to mark task %s failed", task.ID)
		}
		return err
	}

	task.ExecutionID = resp.ExecutionID
	task.ExecutorType = m.executor.Type()
	if err := m.taskStore.UpdateScheduledTask(ctx, task); err != nil {
		log.Error().Err(err).
			Msgf("failed to update scheduled task %s", task.ID)
	}
	return nil
}

func (m *ManagerImpl) deferUnlinkedTask(
	ctx context.Context,
	task *taskdef.Task,
	executionErr error,
	transactionActive bool,
) (bool, error) {
	var unlinkedErr *unlinkedTargetsError
	if !errors.As(executionErr, &unlinkedErr) {
		return false, nil
	}

	deadline := task.QueueExpiresAt
	if deadline == nil {
		timeout := m.defaultQueueTimeout
		if timeout <= 0 {
			timeout = defaultQueueTimeout
		}
		fallback := time.Now().Add(timeout)
		deadline = &fallback
	}

	status := taskcommon.TaskStatusWaiting
	statusMessage := fmt.Sprintf("Waiting for target linkage: %v", unlinkedErr)
	if !time.Now().Before(*deadline) {
		status = taskcommon.TaskStatusTerminated
		statusMessage = fmt.Sprintf(
			"Expired: target linkage unavailable before queue timeout: %v",
			unlinkedErr,
		)
		if err := m.taskStore.UpdateTaskStatus(ctx, &taskdef.TaskStatusUpdate{
			ID:      task.ID,
			Status:  status,
			Message: statusMessage,
		}); err != nil {
			return true, fmt.Errorf("expire task awaiting target linkage: %w", err)
		}
	} else {
		limit := m.maxWaitingPerRack
		if limit <= 0 {
			limit = defaultMaxWaitingPerRack
		}
		persistWaiting := func(txCtx context.Context) error {
			if err := m.taskStore.LockRack(txCtx, task.RackID); err != nil {
				return err
			}
			count, err := m.taskStore.CountWaitingTasksForRack(txCtx, task.RackID)
			if err != nil {
				return err
			}
			if count >= limit {
				status = taskcommon.TaskStatusTerminated
				statusMessage = fmt.Sprintf(
					"Terminated: rack waiting queue is full while target linkage is unavailable (%d/%d tasks): %v",
					count,
					limit,
					unlinkedErr,
				)
			}

			update := &taskdef.TaskStatusUpdate{
				ID:      task.ID,
				Status:  status,
				Message: statusMessage,
			}
			if status == taskcommon.TaskStatusWaiting {
				update.QueueExpiresAt = deadline
			}
			return m.taskStore.UpdateTaskStatus(txCtx, update)
		}

		var err error
		if transactionActive {
			err = persistWaiting(ctx)
		} else {
			err = m.taskStore.RunInTransaction(ctx, persistWaiting)
		}
		if err != nil {
			return true, fmt.Errorf("defer task awaiting target linkage: %w", err)
		}
	}

	task.Status = status
	task.Message = statusMessage
	if status == taskcommon.TaskStatusWaiting {
		task.QueueExpiresAt = deadline
	} else {
		task.QueueExpiresAt = nil
	}
	return true, nil
}

func (m *ManagerImpl) resolveOperationRule(
	ctx context.Context,
	op operation.Wrapper,
	rackID uuid.UUID,
) (*operationrules.OperationRule, error) {
	ruleID := operations.ExtractRuleID(op.Info)
	return m.ruleResolver.ResolveRule(ctx, op.Type, op.Code, rackID, ruleID)
}

// CancelTask cancels a task by its ID.
// Waiting tasks are terminated immediately (no workflow to stop).
// Pending/running tasks have their Temporal workflow terminated.
// Already-terminated tasks return nil (idempotent).
// Completed or failed tasks return an error.
func (m *ManagerImpl) CancelTask(ctx context.Context, taskID uuid.UUID) error {
	task, err := m.taskStore.GetTask(ctx, taskID)
	if err != nil {
		return fmt.Errorf("failed to get task %s: %w", taskID, err)
	}

	if task.Status == taskcommon.TaskStatusTerminated {
		return nil // already cancelled — idempotent
	}

	if task.Status.IsFinished() {
		return fmt.Errorf(
			"task %s cannot be cancelled (status: %s)", taskID, task.Status,
		)
	}

	// Terminate the Temporal workflow if one was scheduled (pending/running).
	// Waiting tasks have no workflow (ExecutionID is empty) so this is skipped.
	if task.IsScheduled() {
		if err := m.executor.TerminateTask(
			ctx, task.ExecutionID, "Cancelled by user",
		); err != nil {
			return fmt.Errorf(
				"failed to terminate workflow for task %s: %w", taskID, err,
			)
		}
	}

	return m.taskStore.UpdateTaskStatus(
		ctx,
		&taskdef.TaskStatusUpdate{
			ID:      taskID,
			Status:  taskcommon.TaskStatusTerminated,
			Message: "Cancelled by user",
		},
	)
}

// loadRackForTask re-fetches the rack for a task and filters its component
// list to only those tracked in task.Attributes.
func (m *ManagerImpl) loadRackForTask(
	ctx context.Context,
	task *taskdef.Task,
) (*rack.Rack, error) {
	rackObj, err := m.inventoryStore.GetRackByIdentifier(
		ctx,
		identifier.Identifier{ID: task.RackID},
		true,
	)
	if err != nil {
		return nil, fmt.Errorf("rack %s not found: %w", task.RackID, err)
	}

	// Filter to the components originally targeted by the task.
	allUUIDs := task.Attributes.AllComponentUUIDs()
	uuidSet := make(map[uuid.UUID]struct{}, len(allUUIDs))
	for _, id := range allUUIDs {
		uuidSet[id] = struct{}{}
	}

	r := rack.New(rackObj.Info, rackObj.Loc)
	for _, comp := range rackObj.Components {
		if _, ok := uuidSet[comp.Info.ID]; ok {
			r.AddComponent(comp)
		}
	}
	return r, nil
}

func workflowComponentsFrom(
	r *rack.Rack,
) []taskdef.WorkflowComponent {
	if r == nil {
		return nil
	}

	comps := make([]taskdef.WorkflowComponent, len(r.Components))
	for i, c := range r.Components {
		comps[i] = taskdef.WorkflowComponent{
			Type:        c.Type,
			ComponentID: c.ComponentID,
		}
	}

	return comps
}

func (m *ManagerImpl) executeTask(
	ctx context.Context,
	task *taskdef.Task,
	targetRack *rack.Rack,
	ruleDef *operationrules.RuleDefinition,
) (*taskdef.ExecutionResponse, error) {
	if task == nil {
		return nil, fmt.Errorf("task is nil")
	}

	err := validateResolvedRackTargets(
		task.Operation,
		ruleDef,
		map[uuid.UUID]*rack.Rack{task.RackID: targetRack},
	)
	if err != nil {
		return nil, fmt.Errorf("operation cannot be executed: %w", err)
	}

	req := taskdef.ExecutionRequest{
		Info: taskdef.ExecutionInfo{
			TaskID:         task.ID,
			Components:     workflowComponentsFrom(targetRack),
			RuleDefinition: ruleDef,
			OperationType:  task.Operation.Type,
			OperationInfo:  task.Operation.Info, // already json.RawMessage from the DB
		},
		Async: true,
	}

	return m.executor.Execute(ctx, &req)
}

func (m *ManagerImpl) getReqExpiresAt(req *operation.Request) *time.Time {
	timeout := req.QueueTimeout
	if timeout <= 0 {
		timeout = m.defaultQueueTimeout
	}

	expiresAt := time.Now().Add(timeout)
	return &expiresAt
}
