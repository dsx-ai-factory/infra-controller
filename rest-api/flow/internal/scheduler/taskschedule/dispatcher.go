// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package taskschedule manages user-defined task schedules (task_schedule table).
// The Dispatcher polls for due schedules and submits them to the task manager,
// using FOR UPDATE SKIP LOCKED to prevent double-firing in multi-instance deployments.
//
// Design note: TaskSchedule does not have an internal domain type.
//
// Other objects (Rack, Component, Task) use three layers: proto, internal domain
// type, and DB model, with converters in internal/converter/. TaskSchedule is
// intentionally kept at two layers (proto and DB model) because:
//
//  1. It is a configuration record, not a domain object. It does not flow
//     through business logic across package boundaries — it goes linearly from
//     API to store and back.
//
//  2. The meaningful domain content inside a schedule (the operation and target)
//     is already properly typed as operation.Request / operation.TargetSpec.
//     TaskSchedule itself is just the scheduling envelope around that content.
//
//  3. Every consumer (service, dispatcher, store) already imports dbmodel for
//     dbmodel.TaskSchedule, so an internal type would add a near-identical
//     struct and trivial field-copy converters with no practical decoupling.
package taskschedule

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"github.com/google/uuid"
	"github.com/robfig/cron/v3"
	"github.com/rs/zerolog/log"

	dbmodel "github.com/NVIDIA/infra-controller/rest-api/flow/internal/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskmanager "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/manager"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	identifier "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/Identifier"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const (
	defaultPollInterval = 10 * time.Second
	defaultFetchBatch   = 10

	// phase3MaxAttempts and phase3RetryBaseDelay bound the retry loop for the
	// Phase 3 last_task_id write-back. The write is idempotent so retrying is
	// safe. Delays are 100 ms, 200 ms, 400 ms — well under any poll interval.
	phase3MaxAttempts    = 3
	phase3RetryBaseDelay = 100 * time.Millisecond
)

// Config holds the configuration for a Dispatcher.
type Config struct {
	Store        Store
	TaskManager  taskmanager.Manager
	TaskStore    taskstore.Store
	PollInterval time.Duration
	FetchBatch   int
}

func (c *Config) applyDefaults() {
	if c.PollInterval <= 0 {
		c.PollInterval = defaultPollInterval
	}

	if c.FetchBatch <= 0 {
		c.FetchBatch = defaultFetchBatch
	}
}

// Dispatcher polls the task_schedule table and submits due tasks to the
// task manager.
type Dispatcher struct {
	store       Store
	taskManager taskmanager.Manager
	taskStore   taskstore.Store
	interval    time.Duration
	batch       int

	cancel    atomic.Pointer[context.CancelFunc]
	startOnce sync.Once
	done      chan struct{}
}

// NewDispatcher creates a new Dispatcher.
func NewDispatcher(cfg Config) *Dispatcher {
	cfg.applyDefaults()
	return &Dispatcher{
		store:       cfg.Store,
		taskManager: cfg.TaskManager,
		taskStore:   cfg.TaskStore,
		interval:    cfg.PollInterval,
		batch:       cfg.FetchBatch,
		done:        make(chan struct{}),
	}
}

// Start launches the polling loop. It returns immediately; the loop runs in a
// background goroutine until Stop is called or ctx is cancelled.
// Start is idempotent: subsequent calls after the first are no-ops.
func (d *Dispatcher) Start(ctx context.Context) error {
	d.startOnce.Do(
		func() {
			ctx, cancel := context.WithCancel(ctx)

			// Publish cancel atomically before launching the goroutine so
			// Stop() either sees nil (Start not yet called) or the real
			// cancel (fully published). If Stop() calls cancel() before the
			// goroutine starts, the goroutine finds its context already done
			// and exits immediately, unblocking any Stop() caller on done.
			d.cancel.Store(&cancel)

			go func() {
				defer close(d.done)
				d.run(ctx)
			}()
		},
	)

	return nil
}

// Stop halts the polling loop and waits for it to exit.
// If Start was never called, Stop returns immediately.
// Stop may be called concurrently; all callers unblock once the loop exits.
func (d *Dispatcher) Stop() {
	// Load cancel atomically. The nil check is kept outside any one-shot
	// gate: consuming a once on a pre-start call would leave a goroutine
	// started later unstoppable.
	cancelPtr := d.cancel.Load()
	if cancelPtr == nil {
		return
	}
	(*cancelPtr)()
	<-d.done
}

func (d *Dispatcher) run(ctx context.Context) {
	ticker := time.NewTicker(d.interval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			d.firedue(ctx)
		}
	}
}

// firedue collects due schedule IDs and fires each one in its own transaction.
func (d *Dispatcher) firedue(ctx context.Context) {
	now := time.Now().UTC()

	// Read-only scan — no locking. Each fire call locks its own row.
	ids, err := d.store.FetchDue(ctx, d.batch, now)
	if err != nil {
		log.Error().
			Err(err).
			Msg("task schedule dispatcher: failed to fetch due schedules")

		return
	}

	log.Debug().
		Int("due_count", len(ids)).
		Msg("task schedule dispatcher: poll cycle")

	for _, id := range ids {
		if err := d.fire(ctx, id, now); err != nil {
			log.Error().
				Err(err).
				Str("task_schedule_id", id.String()).
				Msg("task schedule dispatcher: failed to fire schedule")
		}
	}
}

// fire processes a single due schedule.
//
// Phase 1 (transaction): lock the row, fetch scopes, do per-scope overlap check,
// advance next_run_at (so the row is no longer "due" and another instance won't
// grab it), and commit. Committing before calling SubmitTask avoids nesting
// transactions — the task manager opens its own transaction.
//
// Phase 2 (outside transaction): call taskManager.SubmitTask with the target spec
// derived from the non-skipped scopes.
//
// Phase 3 (transaction): write back last_task_id on each scope row.
//
// # At-most-once semantics for one-time schedules
//
// Phase 1 commits the state transition (enabled=false, next_run_at=NULL) before
// Phase 2 runs. If Phase 2 fails entirely (e.g. a malformed operation template
// detected at fire time), the one-time schedule is permanently consumed and no
// task is created. This is intentional: the alternative — re-enabling the
// schedule on Phase 2 failure — would reintroduce the race where a concurrent
// dispatcher instance picks up the same firing. At-most-once is the safer
// trade-off for power-control and firmware operations.
//
// FireNow (TriggerTaskSchedule) uses the same commit-then-submit ordering and
// therefore carries the same at-most-once guarantee.
func (d *Dispatcher) fire(ctx context.Context, id uuid.UUID, now time.Time) error {
	var (
		current     *dbmodel.TaskSchedule
		doFire      bool
		firedScopes []*dbmodel.TaskScheduleScope
	)

	// Phase 1: lock, fetch scopes, overlap check, advance. This needs to be
	// in a transaction to prevent double-firing.
	if err := d.store.RunInTransaction(
		ctx,
		func(ctx context.Context) error {
			// Lock the row if it is due and has not been locked by another
			// instance.
			var err error
			current, err = d.store.LockForFire(ctx, id, now)
			if err != nil {
				return fmt.Errorf("lock: %w", err)
			}

			if current == nil {
				// Row is locked by another instance or no longer due
				return nil
			}

			// Fetch scopes.
			scopes, err := d.store.ListScopes(ctx, id)
			if err != nil {
				return fmt.Errorf("list scopes: %w", err)
			}

			if len(scopes) == 0 {
				// No targets configured — advance without firing.
				log.Info().
					Str("task_schedule_id", current.ID.String()).
					Str("task_schedule_name", current.Name).
					Msg("task schedule dispatcher: schedule has no scopes, advancing without firing") //nolint:lll

				return d.advanceOnly(ctx, current, now)
			}

			// Per-scope overlap check based on the scheduler's overlap policy.
			candidateScopes, err := d.filterScopesByPolicy(ctx, current, scopes)
			if err != nil {
				return err
			}

			if len(candidateScopes) == 0 {
				// No scopes are eligible for firing — advance without firing.
				log.Info().
					Str("task_schedule_id", current.ID.String()).
					Str("task_schedule_name", current.Name).
					Int("total_scopes", len(scopes)).
					Msg("task schedule dispatcher: no scopes are eligible for firing, advancing without firing") //nolint:lll

				return d.advanceOnly(ctx, current, now)
			}

			log.Info().
				Str("task_schedule_id", current.ID.String()).
				Str("task_schedule_name", current.Name).
				Int("total_scopes", len(scopes)).
				Int("eligible_scopes", len(candidateScopes)).
				Msg("task schedule dispatcher: firing schedule")

			// Apply after-fire state and advance the row.
			// - Do this before releasing the lock so another instance
			//   does not see it as due.
			// - Do this before capturing outer variables so a failure here
			//   leaves no partial state outside the transaction.
			if err := d.applyAfterFire(ctx, current, now); err != nil {
				return err
			}

			firedScopes = candidateScopes
			doFire = true

			return nil
		},
	); err != nil {
		return err
	}

	if !doFire {
		return nil
	}

	// Phase 2: submit one task per scope.
	// - This is done outside the transaction to avoid nesting transactions.
	// - Each scope may have a different component_filter kind, which maps to
	//   different TargetSpec variants, so we cannot batch them into one call.
	scopeTaskIDs, _, err := d.submitScopeTasks(ctx, current, firedScopes, now)
	if err != nil {
		return err
	}

	// Phase 3: record the resulting task ID on each scope row.
	// Retried because tasks are already created at this point — a failure here
	// leaves last_task_id stale and can cause overlapping submissions.
	if len(scopeTaskIDs) > 0 {
		return d.updateScopeLastTaskIDsWithRetry(ctx, current.ID, scopeTaskIDs)
	}

	return nil
}

// advanceOnly moves a schedule past its current due time without firing it
// (e.g. when there are no scope targets, or all scopes are skipped due to overlap).
// For one-time schedules this means consuming them: the window has passed and
// there is nothing to retry, so the schedule is disabled and next_run_at is
// cleared to prevent it from being re-processed on every subsequent poll.
func (d *Dispatcher) advanceOnly(
	ctx context.Context,
	ts *dbmodel.TaskSchedule,
	now time.Time,
) error {
	switch ts.SpecType {
	case dbmodel.SpecTypeOneTime:
		// Consume the schedule: clear next_run_at (nil → NULL in the store)
		// and set enabled=false so it is never picked up again.
		return d.store.UpdateAfterFire(
			ctx,
			ts.ID,
			AfterFireUpdate{
				LastRunAt: now,
				Enabled:   false,
				// NextRunAt left nil → store sets next_run_at to NULL.
			},
		)

	case dbmodel.SpecTypeInterval:
		dur, err := time.ParseDuration(ts.Spec)
		if err != nil {
			return fmt.Errorf("invalid interval spec %q: %w", ts.Spec, err)
		}
		if dur <= 0 {
			return fmt.Errorf("interval spec %q must be a positive duration", ts.Spec)
		}

		next := now.Add(dur)
		_, err = d.store.Update(ctx, ts.ID, UpdateFields{NextRunAt: &next})
		return err

	case dbmodel.SpecTypeCron:
		next, err := nextCronTime(ts.Spec, ts.Timezone, now)
		if err != nil {
			return fmt.Errorf("advance cron spec: %w", err)
		}

		_, err = d.store.Update(ctx, ts.ID, UpdateFields{NextRunAt: &next})
		return err

	default:
		return fmt.Errorf("unknown spec_type %q", ts.SpecType)
	}
}

// submitScopeTasks submits one task per scope and returns the scope→task
// mapping and the flat list of submitted task IDs in scope order.
//
// Each scope represents exactly one rack. RequiredRackID is set on every
// request so SubmitTask returns an error (and creates no tasks) if the
// resolved targets do not belong exclusively to scope.RackID — for instance
// when a component in the filter has been moved to a different rack since the
// scope was created. This guarantees that a successful SubmitTask call always
// returns exactly one task ID, and that ID is what is recorded as the scope's
// last_task_id.
//
// Scopes that fail target resolution or task submission are logged and
// skipped; they do not appear in the returned map.
func (d *Dispatcher) submitScopeTasks(
	ctx context.Context,
	ts *dbmodel.TaskSchedule,
	scopes []*dbmodel.TaskScheduleScope,
	now time.Time,
) (map[uuid.UUID]uuid.UUID, []uuid.UUID, error) {
	op, err := WrapperFromTemplate(ts.OperationTemplate)
	if err != nil {
		return nil, nil, err
	}

	opts, err := OptionsFromTemplate(ts.OperationTemplate)
	if err != nil {
		return nil, nil, err
	}

	// Pre-parse the rule ID once rather than per-scope.
	var ruleID *uuid.UUID
	if opts.RuleID != "" {
		id, err := uuid.Parse(opts.RuleID)
		if err != nil || id == uuid.Nil {
			return nil, nil, fmt.Errorf(
				"task schedule %s has invalid rule_id %q",
				ts.ID,
				opts.RuleID,
			)
		}
		ruleID = &id
	}

	desc := fmt.Sprintf("%s — %s", ts.Name, now.UTC().Format(time.RFC3339))

	scopeTaskIDs := make(map[uuid.UUID]uuid.UUID, len(scopes))
	allTaskIDs := make([]uuid.UUID, 0, len(scopes))
	taskIDStrings := make([]string, 0, len(scopes))
	for _, scope := range scopes {
		targetSpec, err := scopeToTargetSpec(scope)
		if err != nil {
			log.Error().Err(err).
				Str("task_schedule_id", ts.ID.String()).
				Str("task_schedule_name", ts.Name).
				Str("scope_id", scope.ID.String()).
				Str("rack_id", scope.RackID.String()).
				Msg("task schedule dispatcher: skipping scope — failed to build target spec")
			continue
		}

		req := &operation.Request{
			Operation:      op,
			TargetSpec:     targetSpec,
			Description:    desc,
			RequiredRackID: scope.RackID,
			ConflictStrategy: func() operation.ConflictStrategy {
				if ts.OverlapPolicy == dbmodel.OverlapPolicyQueue {
					return operation.ConflictStrategyQueue
				}
				return operation.ConflictStrategyReject
			}(),
			QueueTimeout: time.Duration(opts.QueueTimeoutSecs) * time.Second,
			RuleID:       ruleID,
		}

		taskIDs, err := d.taskManager.SubmitTask(ctx, req)
		if err != nil || len(taskIDs) == 0 {
			// SubmitTask failed or returned no IDs — includes the
			// RequiredRackID rejection when components span multiple racks
			// or have moved to a different rack.
			if err == nil {
				err = fmt.Errorf("SubmitTask returned no task IDs")
			}

			log.Error().Err(err).
				Str("task_schedule_id", ts.ID.String()).
				Str("task_schedule_name", ts.Name).
				Str("scope_id", scope.ID.String()).
				Str("rack_id", scope.RackID.String()).
				Msg("task schedule dispatcher: skipping scope")
			continue
		}

		// RequiredRackID guarantees len(taskIDs) == 1 here.
		scopeTaskIDs[scope.ID] = taskIDs[0]
		allTaskIDs = append(allTaskIDs, taskIDs[0])
		taskIDStrings = append(taskIDStrings, taskIDs[0].String())
	}

	log.Info().
		Str("task_schedule_id", ts.ID.String()).
		Str("task_schedule_name", ts.Name).
		Int("expected_tasks", len(scopes)).
		Int("submitted_tasks", len(allTaskIDs)).
		Strs("task_ids", taskIDStrings).
		Msg("task schedule dispatcher: scope tasks submission completed")

	// Return an error when every scope failed so callers can distinguish
	// "nothing attempted" (empty scopes slice) from "everything attempted but
	// nothing succeeded". This surfaces total Phase 2 failure in the firedue
	// log and causes FireNow to return an error to the RPC caller instead of
	// a misleading success with empty task_ids — important for one-time
	// schedules that are already consumed at this point.
	if len(allTaskIDs) == 0 && len(scopes) > 0 {
		return nil, nil, fmt.Errorf(
			"all %d scope submissions failed for schedule %s",
			len(scopes), ts.ID,
		)
	}

	return scopeTaskIDs, allTaskIDs, nil
}

// applyAfterFire computes the post-fire schedule state and persists it.
// For one-time schedules, Enabled is set to false (consumed). For recurring
// schedules (interval, cron), Enabled is preserved from the current schedule
// state — forcing it to true would silently re-enable a schedule that was
// disabled before a manual trigger.
//
// Must be called within a RunInTransaction block.
func (d *Dispatcher) applyAfterFire(
	ctx context.Context,
	ts *dbmodel.TaskSchedule,
	now time.Time,
) error {
	u := AfterFireUpdate{
		LastRunAt: now,
	}

	switch ts.SpecType {
	case dbmodel.SpecTypeOneTime:
		u.Enabled = false // consumed; next_run_at stays nil → set to NULL by store

	case dbmodel.SpecTypeInterval:
		// ComputeFirstRunAt validates that the duration is positive at create/update
		// time. Guard here as well: a zero/negative interval would set next_run_at
		// to now and cause an infinite task-submission loop every poll cycle.
		dur, err := time.ParseDuration(ts.Spec)
		if err != nil {
			return fmt.Errorf("invalid interval spec %q: %w", ts.Spec, err)
		}
		if dur <= 0 {
			return fmt.Errorf("interval spec %q must be a positive duration", ts.Spec)
		}

		next := now.Add(dur)
		u.Enabled = ts.Enabled // Preserve enabled state from the current schedule.
		u.NextRunAt = &next

	case dbmodel.SpecTypeCron:
		next, err := nextCronTime(ts.Spec, ts.Timezone, now)
		if err != nil {
			return fmt.Errorf("advance cron spec: %w", err)
		}

		u.Enabled = ts.Enabled // Preserve enabled state from the current schedule.
		u.NextRunAt = &next

	default:
		return fmt.Errorf("unknown spec_type %q", ts.SpecType)
	}

	if err := d.store.UpdateAfterFire(ctx, ts.ID, u); err != nil {
		return fmt.Errorf("advance schedule: %w", err)
	}

	return nil
}

// filterScopesByPolicy returns the candidate scopes to fire based on the
// schedule's overlap policy. For OverlapPolicyQueue, all scopes are returned
// unconditionally. For any other policy (default: skip), scopes whose last
// task is still active are excluded. Unrecognised future policy values therefore
// fall through to the safer skip behaviour.
//
// Must be called within a RunInTransaction block.
func (d *Dispatcher) filterScopesByPolicy(
	ctx context.Context,
	current *dbmodel.TaskSchedule,
	scopes []*dbmodel.TaskScheduleScope,
) ([]*dbmodel.TaskScheduleScope, error) {
	if current.OverlapPolicy == dbmodel.OverlapPolicyQueue {
		// Queue policy: include all scopes unconditionally.
		return scopes, nil
	}

	// Default (skip): exclude scopes whose last task is still active.
	// Scopes with no last_task_id (never fired) are always included.
	candidates := make([]*dbmodel.TaskScheduleScope, 0, len(scopes))
	for _, s := range scopes {
		if s.LastTaskID == nil {
			// Never fired — always include.
			candidates = append(candidates, s)
			continue
		}

		active, err := d.isTaskActive(ctx, *s.LastTaskID)
		if err != nil {
			return nil, fmt.Errorf("overlap check for scope %s: %w", s.ID, err)
		}

		if active {
			// Previous task is still active, skip this scope.
			log.Debug().
				Str("task_schedule_id", current.ID.String()).
				Str("task_schedule_name", current.Name).
				Str("scope_id", s.ID.String()).
				Str("rack_id", s.RackID.String()).
				Str("last_task_id", s.LastTaskID.String()).
				Msg("task schedule dispatcher: scope skipped — previous task still active")
			continue
		}

		candidates = append(candidates, s)
	}

	return candidates, nil
}

// isTaskActive reports whether the given task is in a waiting/pending/running state.
func (d *Dispatcher) isTaskActive(ctx context.Context, taskID uuid.UUID) (bool, error) {
	task, err := d.taskStore.GetTask(ctx, taskID)
	if err != nil {
		return false, err
	}

	return task.Status == taskcommon.TaskStatusWaiting ||
		task.Status == taskcommon.TaskStatusPending ||
		task.Status == taskcommon.TaskStatusRunning, nil
}

// updateScopeLastTaskIDsWithRetry writes back the scope→task mapping after a
// firing (Phase 3). It retries up to phase3MaxAttempts times with exponential
// backoff because, by this point, Phase 2 has already created the tasks: a
// failure here leaves last_task_id stale, and the next overlap check would
// treat the new task as absent and potentially submit overlapping work.
//
// The write is idempotent — UpdateScopeLastTaskIDs is a plain SET, so retrying
// the same map is always safe.
func (d *Dispatcher) updateScopeLastTaskIDsWithRetry(
	ctx context.Context,
	scheduleID uuid.UUID,
	scopeTaskIDs map[uuid.UUID]uuid.UUID,
) error {
	var err error
	for attempt := range phase3MaxAttempts {
		if attempt > 0 {
			delay := phase3RetryBaseDelay * (1 << (attempt - 1))
			select {
			case <-ctx.Done():
				return ctx.Err()
			case <-time.After(delay):
			}
			log.Warn().Err(err).
				Int("attempt", attempt+1).
				Int("max_attempts", phase3MaxAttempts).
				Str("task_schedule_id", scheduleID.String()).
				Msg("task schedule dispatcher: Phase 3 last_task_id write failed, retrying")
		}

		err = d.store.RunInTransaction(
			ctx,
			func(ctx context.Context) error {
				return d.store.UpdateScopeLastTaskIDs(ctx, scopeTaskIDs)
			},
		)
		if err == nil {
			return nil
		}
	}

	log.Error().Err(err).
		Str("task_schedule_id", scheduleID.String()).
		Msg("task schedule dispatcher: Phase 3 last_task_id write exhausted retries — " +
			"tasks were created but last_task_id is stale; next overlap check may submit overlapping work")
	return err
}

// FireNow fires a schedule immediately, used by TriggerTaskSchedule.
// It submits tasks for all scopes unconditionally (no overlap check — explicit
// trigger overrides the policy). Returns an error if called on a one-time
// schedule that has already fired.
//
// # Serialization
//
// FireNow uses the same three-phase structure as fire() and acquires a blocking
// FOR UPDATE lock in Phase 1 (rather than the SKIP LOCKED used by the poller).
// This serializes:
//   - Two concurrent FireNow calls: the second blocks on LockForTrigger until
//     the first's Phase 1 commits, then re-reads the updated row.
//   - FireNow racing fire(): if the poller wins the lock first, FireNow blocks
//     until Phase 1 commits; if FireNow wins, the poller's SKIP LOCKED returns
//     nil (row no longer due after applyAfterFire) and skips the schedule.
func (d *Dispatcher) FireNow(
	ctx context.Context,
	id uuid.UUID,
) ([]uuid.UUID, error) {
	now := time.Now().UTC()

	var (
		ts          *dbmodel.TaskSchedule
		firedScopes []*dbmodel.TaskScheduleScope
	)

	// Phase 1: acquire a blocking row lock, validate, fetch scopes, advance state.
	if err := d.store.RunInTransaction(
		ctx,
		func(ctx context.Context) error {
			var err error
			ts, err = d.store.LockForTrigger(ctx, id)
			if err != nil {
				return fmt.Errorf("lock: %w", err)
			}

			// Re-check after acquiring the lock: a concurrent fire() or FireNow
			// may have already consumed a one-time schedule between the RPC
			// reaching this point and the lock being granted.
			if !ts.Enabled &&
				ts.SpecType == dbmodel.SpecTypeOneTime &&
				ts.NextRunAt == nil {
				return fmt.Errorf("cannot trigger a one-time schedule that has already fired; create a new one instead") //nolint:lll
			}

			scopes, err := d.store.ListScopes(ctx, id)
			if err != nil {
				return fmt.Errorf("list scopes: %w", err)
			}

			if len(scopes) == 0 {
				log.Warn().
					Str("task_schedule_id", id.String()).
					Str("task_schedule_name", ts.Name).
					Msg("task schedule dispatcher: manually triggered schedule has no scopes, advancing state only") //nolint:lll
			} else {
				log.Info().
					Str("task_schedule_id", id.String()).
					Str("task_schedule_name", ts.Name).
					Int("scope_count", len(scopes)).
					Msg("task schedule dispatcher: manually triggered schedule")
			}

			// applyAfterFire must run regardless of whether there are scopes:
			// a one-time schedule with no scopes must still be consumed, and
			// recurring schedules must still advance last_run_at / next_run_at
			// as documented for TriggerTaskSchedule.
			if err := d.applyAfterFire(ctx, ts, now); err != nil {
				return err
			}

			firedScopes = scopes
			return nil
		},
	); err != nil {
		return nil, err
	}

	if len(firedScopes) == 0 {
		return nil, nil
	}

	// Phase 2: submit one task per scope outside the transaction.
	scopeTaskIDs, allTaskIDs, err := d.submitScopeTasks(ctx, ts, firedScopes, now)
	if err != nil {
		return nil, err
	}

	// Phase 3: record the resulting task ID on each scope row.
	// Retried because tasks are already created at this point — a failure here
	// leaves last_task_id stale and can cause overlapping submissions.
	if len(scopeTaskIDs) > 0 {
		return allTaskIDs, d.updateScopeLastTaskIDsWithRetry(ctx, ts.ID, scopeTaskIDs)
	}

	return allTaskIDs, nil
}

// scopeToTargetSpec builds an operation.TargetSpec for a single scope row.
//
//   - component_filter == nil → RackTarget with no type filter (all components).
//   - kind == "types"      → RackTarget with ComponentTypes filter.
//   - kind == "components" → ComponentTargets (specific components by UUID).
//
// For the "components" case the returned TargetSpec carries no explicit rack
// constraint; SubmitTask resolves rack membership via inventory. The caller
// (submitScopeTasks) sets RequiredRackID to scope.RackID on the request so
// that SubmitTask rejects any resolution that does not map exclusively to
// scope.RackID — whether the components moved to a different single rack or
// span multiple racks.
func scopeToTargetSpec(scope *dbmodel.TaskScheduleScope) (operation.TargetSpec, error) {
	cf, err := dbmodel.UnmarshalComponentFilter(scope.ComponentFilter)
	if err != nil {
		return operation.TargetSpec{}, fmt.Errorf("unmarshal component_filter for scope %s: %w", scope.ID, err)
	}

	if cf == nil {
		// No filter — target all components in the rack.
		return operation.TargetSpec{
			Racks: []operation.RackTarget{
				{Identifier: identifier.Identifier{ID: scope.RackID}},
			},
		}, nil
	}

	switch cf.Kind {
	case dbmodel.ComponentFilterKindTypes:
		rt := operation.RackTarget{
			Identifier: identifier.Identifier{ID: scope.RackID},
		}
		for _, t := range cf.Types {
			rt.ComponentTypes = append(
				rt.ComponentTypes,
				devicetypes.ComponentTypeFromString(t),
			)
		}
		return operation.TargetSpec{Racks: []operation.RackTarget{rt}}, nil

	case dbmodel.ComponentFilterKindComponents:
		targets := make([]operation.ComponentTarget, 0, len(cf.Components))
		for _, id := range cf.Components {
			targets = append(targets, operation.ComponentTarget{UUID: id})
		}
		return operation.TargetSpec{Components: targets}, nil

	default:
		return operation.TargetSpec{}, fmt.Errorf(
			"unknown component_filter kind %q for scope %s", cf.Kind, scope.ID,
		)
	}
}

// ComputeFirstRunAt returns the first execution time for a new schedule.
func ComputeFirstRunAt(
	specType dbmodel.SpecType,
	spec string,
	timezone string,
) (time.Time, error) {
	now := time.Now().UTC()

	switch specType {
	case dbmodel.SpecTypeOneTime:
		t, err := time.Parse(time.RFC3339, spec)
		if err != nil {
			return time.Time{}, fmt.Errorf("invalid one-time spec %q: %w", spec, err)
		}
		return t.UTC(), nil

	case dbmodel.SpecTypeInterval:
		dur, err := time.ParseDuration(spec)
		if err != nil {
			return time.Time{}, fmt.Errorf("invalid interval spec %q: %w", spec, err)
		}
		if dur <= 0 {
			return time.Time{}, fmt.Errorf("interval spec %q must be a positive duration", spec)
		}
		return now.Add(dur), nil

	case dbmodel.SpecTypeCron:
		return nextCronTime(spec, timezone, now)

	default:
		return time.Time{}, fmt.Errorf("unknown spec_type %q", specType)
	}
}

// nextCronTime computes the next firing time for a cron spec after the given
// reference time, interpreting the spec in the IANA timezone tz.
func nextCronTime(spec string, tz string, after time.Time) (time.Time, error) {
	loc, err := time.LoadLocation(tz)
	if err != nil {
		return time.Time{}, fmt.Errorf("invalid timezone %q: %w", tz, err)
	}

	// ParseStandard expects a 5-field cron expression (minute hour dom month dow).
	schedule, err := cron.ParseStandard(spec)
	if err != nil {
		return time.Time{}, fmt.Errorf("invalid cron spec %q: %w", spec, err)
	}

	return schedule.Next(after.In(loc)).UTC(), nil
}
