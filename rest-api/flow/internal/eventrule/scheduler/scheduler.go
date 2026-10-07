// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package scheduler owns initial and deferred event-rule execution attempts.
package scheduler

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// TODO(pending-admission-control): Backpressure collectors when durable pending
// work exceeds the capacity established for the event-rule execution path.

type runtime struct {
	store             eventrule.ExecutionStore
	executors         ExecutorRegistry
	policy            PolicyConfig
	pollInterval      time.Duration
	persistTimeout    time.Duration
	claimDuration     time.Duration
	wakeCh            chan struct{}
	fatalWorkerErrors chan error
}

// Scheduler claims and dispatches pending, due deferred, and expired running
// executions.
type Scheduler struct {
	instanceID  string
	runtime     runtime
	lanes       []*lane
	lifecycleMu sync.Mutex
	started     bool
	stopped     bool
	cancel      context.CancelFunc
	done        chan struct{}
	runErr      error
}

// New constructs an execution scheduler without starting it.
func New(config Config) (*Scheduler, error) {
	if err := config.Validate(); err != nil {
		return nil, err
	}

	lanes := make([]*lane, 0, len(laneDefinitions))
	for _, definition := range laneDefinitions {
		lanes = append(lanes, newLane(
			definition.name,
			config.Runtime.laneConfig(definition),
			definition.claimFunc(config.Dependencies.Store),
		))
	}

	return &Scheduler{
		instanceID: config.InstanceID,
		runtime:    config.runtime(),
		lanes:      lanes,
	}, nil
}

// Notify non-blockingly hints that eligible work is available.
func (s *Scheduler) Notify() {
	s.wake()
}

// wake non-blockingly hints that eligible work or worker capacity is
// available. Signals coalesce; periodic polling remains the reliability path.
func (s *Scheduler) wake() {
	select {
	case s.runtime.wakeCh <- struct{}{}:
	default:
	}
}

// Start launches both worker pools and the scheduling loop in the background.
// One Scheduler value may be started only once. Runtime failures are retained
// and returned by Stop.
func (s *Scheduler) Start(ctx context.Context) error {
	s.lifecycleMu.Lock()
	defer s.lifecycleMu.Unlock()

	if s.started {
		return fmt.Errorf("scheduler can only be started once")
	}

	runCtx, cancel := context.WithCancel(ctx)
	s.started = true
	s.cancel = cancel
	s.done = make(chan struct{})

	go func() {
		err := s.run(runCtx)
		if err != nil {
			log.Error().
				Err(err).
				Str("instance_id", s.instanceID).
				Msg("event-rule scheduler stopped unexpectedly")
		}

		s.lifecycleMu.Lock()
		s.runErr = err
		s.lifecycleMu.Unlock()

		close(s.done)
	}()

	return nil
}

// Stop cancels the scheduling loop and waits for all workers to exit. It
// returns any runtime failure that stopped the scheduler.
func (s *Scheduler) Stop() error {
	s.lifecycleMu.Lock()
	if !s.started {
		s.lifecycleMu.Unlock()
		return fmt.Errorf("scheduler cannot be stopped before it is started")
	}
	if s.stopped {
		s.lifecycleMu.Unlock()
		return fmt.Errorf("scheduler can only be stopped once")
	}

	s.stopped = true
	cancel := s.cancel
	done := s.done
	s.lifecycleMu.Unlock()

	cancel()
	<-done

	s.lifecycleMu.Lock()
	defer s.lifecycleMu.Unlock()

	return s.runErr
}

func (s *Scheduler) run(ctx context.Context) error {
	runCtx, cancel := context.WithCancel(ctx)
	var workers sync.WaitGroup
	for _, workLane := range s.lanes {
		workLane.startWorkers(runCtx, &workers, &s.runtime)
	}
	defer func() {
		cancel()
		workers.Wait()
	}()

	ticker := time.NewTicker(s.runtime.pollInterval)
	defer ticker.Stop()
	s.wake()

	for {
		select {
		case <-ctx.Done():
			return nil
		case err := <-s.runtime.fatalWorkerErrors:
			return err
		case <-ticker.C:
			s.wake()
		case <-s.runtime.wakeCh:
			// A select may choose this ready wake signal even when
			// cancellation is also ready, so avoid starting new store work.
			if ctx.Err() != nil {
				return nil
			}

			if err := s.refill(runCtx); err != nil {
				return err
			}
		}
	}
}

func (s *Scheduler) refill(ctx context.Context) error {
	for _, workLane := range s.lanes {
		// Do not claim new work after shutdown begins, including when it begins
		// between two lane refills.
		if ctx.Err() != nil {
			return nil
		}

		batch, err := workLane.claim(
			ctx,
			s.instanceID,
			s.runtime.claimDuration,
			s.runtime.policy.MaxAttempts,
		)
		if err != nil {
			if errors.Is(err, errLaneCapacityAccounting) {
				return err
			}

			// Store availability is independent from scheduler correctness. Report
			// the failed cycle and keep all lanes running; a notification or the
			// periodic poll will retry the claim.
			log.Error().
				Err(err).
				Str("lane", workLane.name).
				Msg("failed to claim event-rule executions")
			continue
		}

		channelCapacityMismatch := false
		for _, claim := range batch.Claims {
			select {
			case workLane.jobs <- claim:
			default:
				// A reserved slot guarantees worker-channel capacity. If the
				// channel is nevertheless full, its accounting is untrustworthy:
				// continue through the already-claimed batch, then stop the
				// scheduler. Do not add special-case claim recovery or repair slot
				// state here: abandoned claims belong to the same stuck-running
				// recovery required for other scheduler failures, while shutdown
				// discards this lane's capacity state.
				channelCapacityMismatch = true
			}
		}

		if channelCapacityMismatch {
			return fmt.Errorf(
				"%s work channel has no capacity despite reserved worker slots",
				workLane.name,
			)
		}

		// A full candidate scan may have left additional eligible work, even when
		// exhausted retries reduced the number of claims it produced.
		if batch.ScanLimitReached {
			s.wake()
		}
	}

	return nil
}
