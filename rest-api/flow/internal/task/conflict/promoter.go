// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package conflict

import (
	"context"
	"fmt"
	"time"

	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	taskstore "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/store"
	taskdef "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

const (
	defaultSweepInterval  = 5 * time.Minute
	defaultNotifyChSize   = 64
	defaultRestartBackoff = 5 * time.Second
)

// PromoterConfig holds tunable parameters for the Promoter.
type PromoterConfig struct {
	// SweepInterval controls how often the promoter scans all racks for
	// expired waiting tasks and stranded queues. An immediate sweep is
	// always performed on Start. Defaults to 5 minutes.
	SweepInterval time.Duration

	// NotifyChannelSize is the buffer size of the rack-notification channel.
	// Defaults to 64.
	NotifyChannelSize int

	// RestartBackoff is the delay before restarting the event loop after an
	// unexpected exit (e.g. a recovered panic). Defaults to 5 seconds.
	RestartBackoff time.Duration
}

func (c *PromoterConfig) applyDefaults() {
	if c.SweepInterval <= 0 {
		c.SweepInterval = defaultSweepInterval
	}
	if c.NotifyChannelSize <= 0 {
		c.NotifyChannelSize = defaultNotifyChSize
	}
	if c.RestartBackoff <= 0 {
		c.RestartBackoff = defaultRestartBackoff
	}
}

// Promoter listens for task-completion events and promotes the next eligible
// waiting task for a rack to pending, then triggers its execution.
//
// It is event-driven: the notifyCh receives rack IDs whenever a task on that
// rack finishes. A single background goroutine drains the channel, purges
// expired waiting tasks, and promotes the oldest non-expired one.
// A periodic ticker additionally sweeps all racks so that expiry is
// predictable and stranded waiting tasks are recovered after restarts.
type Promoter struct {
	store          taskstore.Store
	notifyCh       chan uuid.UUID
	sweepInterval  time.Duration
	restartBackoff time.Duration
	promoteFunc    func(ctx context.Context, taskID uuid.UUID) error
}

// NewPromoter creates a fully initialized Promoter. promoteFunc is the
// callback invoked to execute a task that has been promoted from waiting to
// pending.
func NewPromoter(
	store taskstore.Store,
	promoteFunc func(ctx context.Context, taskID uuid.UUID) error,
	conf PromoterConfig,
) *Promoter {
	conf.applyDefaults()
	return &Promoter{
		store:          store,
		notifyCh:       make(chan uuid.UUID, conf.NotifyChannelSize),
		sweepInterval:  conf.SweepInterval,
		restartBackoff: conf.RestartBackoff,
		promoteFunc:    promoteFunc,
	}
}

// Notify queues a rack ID for promotion processing. Non-blocking: if the
// channel is full the notification is silently dropped (the next completion
// event will re-trigger processing for the rack).
func (p *Promoter) Notify(rackID uuid.UUID) {
	select {
	case p.notifyCh <- rackID:
	default:
		log.Warn().
			Str("rack_id", rackID.String()).
			Msg("promoter notify channel full; notification dropped")
	}
}

// Start launches the promotion event loop in a background goroutine and
// returns immediately. The loop runs until ctx is cancelled. If the loop
// exits unexpectedly (e.g. due to a recovered panic) it is restarted after
// RestartBackoff.
func (p *Promoter) Start(ctx context.Context) {
	go func() {
		log.Info().Msg("task promoter started")
		for {
			p.runLoop(ctx)
			if ctx.Err() != nil {
				log.Info().Msg("task promoter stopped")
				return
			}
			// runLoop returned without ctx cancellation — unexpected; restart.
			log.Warn().
				Dur("backoff", p.restartBackoff).
				Msg("task promoter restarting after unexpected exit")
			select {
			case <-ctx.Done():
				log.Info().Msg("task promoter stopped")
				return
			case <-time.After(p.restartBackoff):
			}
		}
	}()
}

// runLoop runs a single iteration of the promotion event loop. It returns
// when ctx is cancelled or if a panic is recovered.
func (p *Promoter) runLoop(ctx context.Context) {
	defer func() {
		if r := recover(); r != nil {
			log.Error().
				Str("panic", fmt.Sprintf("%v", r)).
				Msg("task promoter panic recovered")
		}
	}()

	// Recover stranded waiting tasks left over from before restart.
	p.sweepAllRacks(ctx)

	ticker := time.NewTicker(p.sweepInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case rackID := <-p.notifyCh:
			p.processRack(ctx, rackID)
		case <-ticker.C:
			// Periodic sweep serves two purposes:
			// 1. Predictable expiry: terminate timed-out waiting tasks
			//    regardless of whether any task completion event fired.
			// 2. Missed-notification recovery: if a completion event was
			//    dropped (full channel) or the service restarted, the
			//    sweep detects a free rack and promotes stranded waiting
			//    tasks.
			p.sweepAllRacks(ctx)
		}
	}
}

// sweepAllRacks queries every rack that has waiting tasks and processes each
// one — purging expired tasks and promoting the oldest eligible one.
func (p *Promoter) sweepAllRacks(ctx context.Context) {
	rackIDs, err := p.store.ListRacksWithWaitingTasks(ctx)
	if err != nil {
		log.Error().Err(err).
			Msg("promoter: failed to list racks with waiting tasks")
		return
	}
	for _, rackID := range rackIDs {
		p.processRack(ctx, rackID)
	}
}

// processRack fetches all waiting tasks for a rack once, terminates expired
// ones, then promotes eligible candidates using the builtinRule.
//
// Candidates are evaluated in FIFO order. Promotion continues as long as
// consecutive candidates do not conflict with the current active set
// (including tasks promoted earlier in this pass). The loop stops at the
// first conflicting candidate — tasks behind it are not promoted even if
// they would not conflict themselves. This preserves strict submission
// ordering at the cost of some parallelism, which is the right trade-off
// for hardware operations where users expect sequential execution.
func (p *Promoter) processRack(ctx context.Context, rackID uuid.UUID) {
	waiting, err := p.store.ListWaitingTasksForRack(ctx, rackID)
	if err != nil {
		log.Error().Err(err).
			Str("rack_id", rackID.String()).
			Msg("promoter: failed to list waiting tasks")
		return
	}

	// Split into expired (terminate immediately) and candidates (promote).
	now := time.Now()
	candidates := make([]*taskdef.Task, 0, len(waiting))
	for _, t := range waiting {
		if t.QueueExpiresAt != nil && now.After(*t.QueueExpiresAt) {
			if err := p.store.UpdateTaskStatus(ctx, &taskdef.TaskStatusUpdate{
				ID:      t.ID,
				Status:  taskcommon.TaskStatusTerminated,
				Message: "Expired: queue timeout reached",
			}); err != nil {
				log.Error().Err(err).
					Str("task_id", t.ID.String()).
					Msg("promoter: failed to terminate expired task")
			}
			continue
		}
		candidates = append(candidates, t)
	}

	if len(candidates) == 0 {
		return
	}

	// Atomically fetch the active set and promote candidates in FIFO
	// order, stopping at the first conflict.
	var toExecute []*taskdef.Task
	txErr := p.store.RunInTransaction(ctx, func(txCtx context.Context) error {
		toExecute = nil // reset on retry

		active, err := p.store.ListActiveTasksForRack(txCtx, rackID)
		if err != nil {
			return err
		}

		// workingActive grows as candidates are promoted within this
		// pass, so each subsequent candidate is checked against an
		// up-to-date effective active set.
		workingActive := make([]*taskdef.Task, len(active))
		copy(workingActive, active)
		for _, candidate := range candidates {
			if builtinRule.Conflicts(candidate, workingActive) {
				break // stop; preserve FIFO ordering
			}

			if err := p.store.UpdateTaskStatus(txCtx,
				&taskdef.TaskStatusUpdate{
					ID:     candidate.ID,
					Status: taskcommon.TaskStatusPending,
					Message: "Promoted: no conflicting" +
						" active tasks",
				},
			); err != nil {
				return err
			}
			toExecute = append(toExecute, candidate)
			workingActive = append(workingActive, candidate)
		}
		return nil
	})
	if txErr != nil {
		log.Error().Err(txErr).
			Str("rack_id", rackID.String()).
			Msg("promoter: failed to promote waiting tasks")
		return
	}

	for _, task := range toExecute {
		if err := p.promoteFunc(ctx, task.ID); err != nil {
			log.Error().Err(err).
				Str("task_id", task.ID.String()).
				Msg("promoter: failed to execute promoted task")
		}
	}
}
