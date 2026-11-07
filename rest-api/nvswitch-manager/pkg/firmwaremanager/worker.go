// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/firmwaremanager/packages"
	"github.com/NVIDIA/infra-controller/rest-api/nvswitch-manager/pkg/nvswitchmanager"

	"github.com/google/uuid"
	log "github.com/sirupsen/logrus"
)

const (
	// DefaultSchedulerInterval is how often the scheduler queries for pending updates.
	DefaultSchedulerInterval = 5 * time.Second
	// DefaultNumWorkers is the default number of concurrent workers.
	DefaultNumWorkers = 10
)

// WorkItem represents a unit of work dispatched to a worker.
type WorkItem struct {
	Update *FirmwareUpdate
	Done   func() // Called when processing is complete
}

// WorkerPool manages concurrent firmware update execution using a scheduler-based model.
//
// Architecture:
//   - Scheduler: Single goroutine that queries DB for pending updates and dispatches to workChan
//   - Workers: N goroutines that read from workChan and process updates
//   - Batch-and-Wait: Scheduler waits for all dispatched work to complete before next cycle
//
// This ensures no two workers process the same update simultaneously (channel = natural mutex).
type WorkerPool struct {
	numWorkers        int
	schedulerInterval time.Duration
	store             UpdateStore
	nsmgr             *nvswitchmanager.NVSwitchManager
	packages          *packages.Registry

	// Work dispatch channel - scheduler sends, workers receive
	workChan chan WorkItem

	// Track active jobs for cancellation
	activeJobs map[uuid.UUID]context.CancelFunc
	mu         sync.RWMutex

	// Lifecycle management
	wg sync.WaitGroup
	// WorkerPool owns its lifecycle via context for goroutine shutdown
	ctx    context.Context //nolint:containedctx
	cancel context.CancelFunc
}

// NewWorkerPool creates a new scheduler-based worker pool.
func NewWorkerPool(
	numWorkers int,
	schedulerInterval time.Duration,
	store UpdateStore,
	nsmgr *nvswitchmanager.NVSwitchManager,
	packages *packages.Registry,
) *WorkerPool {
	ctx, cancel := context.WithCancel(context.Background())

	if numWorkers <= 0 {
		numWorkers = DefaultNumWorkers
	}
	if schedulerInterval <= 0 {
		schedulerInterval = DefaultSchedulerInterval
	}

	return &WorkerPool{
		numWorkers:        numWorkers,
		schedulerInterval: schedulerInterval,
		store:             store,
		nsmgr:             nsmgr,
		packages:          packages,
		workChan:          make(chan WorkItem, numWorkers),
		activeJobs:        make(map[uuid.UUID]context.CancelFunc),
		ctx:               ctx,
		cancel:            cancel,
	}
}

// Start launches the scheduler and worker goroutines.
func (p *WorkerPool) Start() {
	log.Infof("Starting worker pool with %d workers (scheduler interval: %v)", p.numWorkers, p.schedulerInterval)

	// Start workers
	for i := range p.numWorkers {
		p.wg.Add(1)
		go p.worker(i)
	}

	// Start scheduler
	p.wg.Add(1)
	go p.scheduler()
}

// Stop gracefully shuts down the worker pool.
func (p *WorkerPool) Stop() {
	log.Info("Stopping worker pool...")

	// Signal shutdown
	p.cancel()

	// Cancel all active jobs
	p.mu.Lock()
	for id, cancelFn := range p.activeJobs {
		log.Infof("Cancelling active job: %s", id)
		cancelFn()
	}
	p.mu.Unlock()

	// Wait for all goroutines to finish
	// NOTE: workChan is closed by the scheduler goroutine (the sole writer)
	// when it observes context cancellation, which unblocks the workers.
	p.wg.Wait()
	log.Info("Worker pool stopped")
}

// scheduler is the main scheduling loop that queries DB and dispatches work.
// It implements a batch-and-wait model: dispatch N updates, wait for all to complete, repeat.
func (p *WorkerPool) scheduler() {
	defer p.wg.Done()
	defer close(p.workChan)
	log.Info("Scheduler started")

	ticker := time.NewTicker(p.schedulerInterval)
	defer ticker.Stop()

	for {
		select {
		case <-p.ctx.Done():
			log.Info("Scheduler stopping")
			return

		case <-ticker.C:
			p.runSchedulerCycle()
		}
	}
}

// runSchedulerCycle queries for pending updates and dispatches them to workers.
// It waits for all dispatched work to complete before returning.
func (p *WorkerPool) runSchedulerCycle() {
	// Query DB for pending updates (up to numWorkers)
	updates, err := p.store.GetPendingUpdates(p.ctx, p.numWorkers)
	if err != nil {
		log.Errorf("Scheduler: failed to get pending updates: %v", err)
		return
	}

	if len(updates) == 0 {
		log.Debug("Scheduler: no pending updates")
		return
	}

	log.Debugf("Scheduler: dispatching %d updates to workers", len(updates))

	// WaitGroup to track completion of this batch
	var batchWg sync.WaitGroup

	// Dispatch all updates to workers
	for _, update := range updates {
		batchWg.Add(1)

		// Create done callback for this work item
		done := func() {
			batchWg.Done()
		}

		// Send to work channel (blocks if channel is full, providing backpressure)
		select {
		case p.workChan <- WorkItem{Update: update, Done: done}:
			// Successfully dispatched
		case <-p.ctx.Done():
			// Shutdown requested
			batchWg.Done()
			return
		}
	}

	// Wait for all dispatched work to complete before next cycle
	batchWg.Wait()
	log.Debugf("Scheduler: batch of %d updates completed", len(updates))
}

// worker is the main loop for each worker goroutine.
// Workers read from workChan and process updates.
func (p *WorkerPool) worker(id int) {
	defer p.wg.Done()
	log.Debugf("Worker %d started", id)

	for item := range p.workChan {
		p.processUpdate(id, item.Update)
		item.Done() // Signal completion to scheduler
	}

	log.Debugf("Worker %d stopped", id)
}

// processUpdate executes a single step of the state machine for an update.
func (p *WorkerPool) processUpdate(workerID int, update *FirmwareUpdate) {
	// Create cancellable context for this step
	ctx, cancel := context.WithCancel(p.ctx)

	// Track active job
	p.mu.Lock()
	p.activeJobs[update.ID] = cancel
	p.mu.Unlock()

	defer func() {
		p.mu.Lock()
		delete(p.activeJobs, update.ID)
		p.mu.Unlock()
		cancel()
	}()

	// Check for context cancellation before starting
	select {
	case <-ctx.Done():
		log.Warnf("Worker %d: Context cancelled before processing update %s", workerID, update.ID)
		return
	default:
	}

	// Skip if already in terminal state (shouldn't happen, but defensive)
	if update.State.IsTerminal() {
		log.Debugf("Worker %d: Update %s already in terminal state %s, skipping", workerID, update.ID, update.State)
		return
	}

	// Transition QUEUED updates to their first active state
	if update.State == StateQueued {
		firstState := GetFirstState(update)
		log.Infof("Worker %d: Transitioning update %s from QUEUED to %s", workerID, update.ID, firstState)
		update.SetState(firstState)
		if err := p.store.Save(ctx, update); err != nil {
			log.Errorf("Worker %d: Failed to transition update %s: %v", workerID, update.ID, err)
			return
		}
	}

	log.Infof("Worker %d: Processing update %s [switch=%s, component=%s, strategy=%s, state=%s]",
		workerID, update.ID, update.SwitchUUID, update.Component, update.Strategy, update.State)

	// Load package
	pkg, err := p.packages.Get(update.BundleVersion)
	if err != nil {
		p.failUpdate(ctx, update, fmt.Sprintf("bundle not found: %v", err))
		return
	}

	// Load switch with credentials
	tray, err := p.nsmgr.Get(ctx, update.SwitchUUID)
	if err != nil {
		p.failUpdate(ctx, update, fmt.Sprintf("switch not found: %v", err))
		return
	}

	// Get firmware path (component names in YAML are lowercase)
	componentName := strings.ToLower(string(update.Component))
	firmwarePath, err := p.packages.GetFirmwarePath(pkg, componentName)
	if err != nil {
		p.failUpdate(ctx, update, fmt.Sprintf("firmware path error: %v", err))
		return
	}

	// Get component definition for script name
	compDef := pkg.GetComponent(componentName)
	if compDef == nil {
		p.failUpdate(ctx, update, fmt.Sprintf("component %s not found in bundle", componentName))
		return
	}

	// Create strategy
	strategy := p.createStrategy(update.Strategy, pkg, firmwarePath, compDef.Script, compDef.ScriptArgs)
	if strategy == nil {
		p.failUpdate(ctx, update, fmt.Sprintf("unknown strategy: %s", update.Strategy))
		return
	}

	// Get current version before update (if not already set)
	if update.VersionFrom == "" && update.ExecContext == nil {
		currentVersion, err := strategy.GetCurrentVersion(ctx, tray, update.Component)
		if err != nil {
			if isTransientError(err) {
				log.Warnf("Worker %d: [%s] Transient error getting version: %v", workerID, update.ID, err)
				// Don't fail, just continue without version
			} else {
				log.Warnf("Worker %d: Failed to get current version: %v", workerID, err)
			}
		} else {
			update.VersionFrom = currentVersion
			if saveErr := p.store.Save(ctx, update); saveErr != nil {
				log.Warnf("Worker %d: Failed to save version_from: %v", workerID, saveErr)
			}
		}
	}

	// Validate current state is in the strategy's steps
	steps := strategy.Steps(update)
	currentIdx := -1
	for i, step := range steps {
		if step == update.State {
			currentIdx = i
			break
		}
	}

	if currentIdx == -1 {
		p.failUpdate(ctx, update, fmt.Sprintf("unexpected state %s not in strategy steps %v", update.State, steps))
		return
	}

	// Execute the current step
	log.Infof("Worker %d: [%s] Executing step %d/%d: %s", workerID, update.ID, currentIdx+1, len(steps), update.State)
	startTime := time.Now()
	outcome := strategy.ExecuteStep(ctx, update, tray)
	duration := time.Since(startTime)

	// Handle the outcome
	p.handleStepOutcome(ctx, workerID, update, outcome, duration)
}

// handleStepOutcome processes the result of executing a step.
func (p *WorkerPool) handleStepOutcome(ctx context.Context, workerID int, update *FirmwareUpdate, outcome StepOutcome, duration time.Duration) {
	switch outcome.Type {
	case OutcomeWait:
		// Async operation in progress - save exec context
		log.Debugf("Worker %d: [%s] Step %s returned Wait after %v (will be picked up next cycle)",
			workerID, update.ID, update.State, duration)

		update.ExecContext = outcome.ExecContext
		update.UpdatedAt = time.Now()

		if err := p.store.Save(ctx, update); err != nil {
			log.Errorf("Worker %d: [%s] Failed to persist wait state: %v", workerID, update.ID, err)
		}

	case OutcomeTransition:
		// Step completed successfully - advance to next state
		log.Infof("Worker %d: [%s] Step %s completed in %v, transitioning to %s",
			workerID, update.ID, update.State, duration, outcome.NextState)

		// Clear exec context on transition
		update.ExecContext = nil
		update.SetState(outcome.NextState)

		if err := p.store.Save(ctx, update); err != nil {
			log.Errorf("Worker %d: [%s] Failed to persist transition: %v", workerID, update.ID, err)
		}

		// Log if we reached a terminal state
		if outcome.NextState.IsTerminal() {
			if outcome.NextState == StateCompleted {
				log.Infof("Worker %d: [%s] Update completed successfully", workerID, update.ID)
			} else {
				log.Infof("Worker %d: [%s] Update reached terminal state: %s", workerID, update.ID, outcome.NextState)
			}
		}

	case OutcomeFailed:
		// Step failed - mark update as failed
		log.Errorf("Worker %d: [%s] Step %s failed after %v: %v",
			workerID, update.ID, update.State, duration, outcome.Error)

		errMsg := "unknown error"
		if outcome.Error != nil {
			errMsg = outcome.Error.Error()
		}
		p.failUpdate(ctx, update, errMsg)

	default:
		log.Errorf("Worker %d: [%s] Unknown outcome type %d from step %s",
			workerID, update.ID, outcome.Type, update.State)
		p.failUpdate(ctx, update, fmt.Sprintf("unknown outcome type: %d", outcome.Type))
	}
}

// createStrategy creates the appropriate strategy for the update.
func (p *WorkerPool) createStrategy(strategyType Strategy, pkg *packages.FirmwarePackage, firmwarePath, scriptName string, scriptArgs []string) UpdateStrategy {
	var strategy UpdateStrategy

	switch strategyType {
	case StrategyRedfish:
		s := NewRedfishStrategy(pkg.StrategyConfig.Redfish)
		s.SetFirmwarePath(firmwarePath)
		strategy = s
	case StrategySSH:
		s := NewSSHStrategy(pkg.StrategyConfig.SSH)
		s.SetFirmwarePath(firmwarePath)
		strategy = s
	case StrategyScript:
		s := NewScriptStrategy(pkg.StrategyConfig.Script)
		s.SetFirmwarePath(firmwarePath)
		s.SetScriptName(scriptName)
		s.SetScriptArgs(scriptArgs)
		strategy = s
	default:
		return nil
	}

	return strategy
}

// failUpdate marks an update as failed and persists the state.
func (p *WorkerPool) failUpdate(ctx context.Context, update *FirmwareUpdate, reason string) {
	log.Errorf("Update %s failed: %s", update.ID, reason)
	update.ErrorMessage = reason
	update.SetState(StateFailed)
	_ = p.store.Save(ctx, update)

	// Cancel remaining updates in the bundle if this is part of a multi-component update
	if update.BundleUpdateID != nil {
		cancelled, err := p.store.CancelRemainingInBundle(ctx, *update.BundleUpdateID, update.SequenceOrder, update.Component)
		if err != nil {
			log.Errorf("Failed to cancel remaining bundle updates: %v", err)
		} else if cancelled > 0 {
			log.Infof("Cancelled %d remaining updates in bundle %s due to %s failure",
				cancelled, update.BundleUpdateID, update.Component)
		}
	}
}

// CancelJob cancels a specific running job.
func (p *WorkerPool) CancelJob(updateID uuid.UUID) bool {
	p.mu.RLock()
	cancelFn, exists := p.activeJobs[updateID]
	p.mu.RUnlock()

	if exists {
		cancelFn()
		return true
	}
	return false
}

// ActiveJobCount returns the number of currently running jobs.
func (p *WorkerPool) ActiveJobCount() int {
	p.mu.RLock()
	defer p.mu.RUnlock()
	return len(p.activeJobs)
}

// IsJobActive returns true if the given job is currently being processed.
func (p *WorkerPool) IsJobActive(updateID uuid.UUID) bool {
	p.mu.RLock()
	defer p.mu.RUnlock()
	_, exists := p.activeJobs[updateID]
	return exists
}
