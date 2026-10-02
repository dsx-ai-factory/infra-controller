// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"fmt"
	"sort"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/executor"
)

// ExecutorRegistry resolves the executor for a persisted plan type.
type ExecutorRegistry interface {
	Executor(eventrule.ActionType) (executor.Executor, error)
}

// Dependencies contains the services used by a Scheduler.
type Dependencies struct {
	Store     eventrule.ExecutionStore
	Executors ExecutorRegistry
}

// Validate checks the scheduler dependencies.
func (d Dependencies) Validate() error {
	if d.Store == nil {
		return fmt.Errorf("scheduler store is required")
	}

	if d.Executors == nil {
		return fmt.Errorf("executor registry is required")
	}

	return nil
}

// LaneConfig bounds one scheduler lane's worker and claim capacity.
type LaneConfig struct {
	Workers   int
	ScanLimit int
}

func (c LaneConfig) validate(name string) error {
	if c.Workers <= 0 {
		return fmt.Errorf("%s worker count must be positive", name)
	}

	if c.ScanLimit <= 0 {
		return fmt.Errorf("%s scan limit must be positive", name)
	}

	return nil
}

// RuntimeConfig bounds the scheduler's runtime mechanics.
type RuntimeConfig struct {
	PollInterval   time.Duration
	PersistTimeout time.Duration
	// ClaimDuration is the fixed interval before an initial running execution
	// becomes eligible for recovery. Reclaimed executions receive twice this
	// interval as a bounded safeguard against an underestimated duration. Current
	// actions do not renew claims. If a future action can legitimately run longer
	// without increasing the acceptable recovery delay, add an explicit renewable
	// mode for that action instead of renewing every claim.
	ClaimDuration time.Duration
	// Lanes overrides capacity by supported lane name. Omitted lanes use their
	// defaults; lane priority remains defined by the scheduler.
	Lanes map[string]LaneConfig
}

// DefaultRuntimeConfig returns the default scheduler runtime configuration.
func DefaultRuntimeConfig() RuntimeConfig {
	lanes := make(map[string]LaneConfig, len(laneDefinitions))
	for _, definition := range laneDefinitions {
		lanes[definition.name] = definition.defaultConfig
	}

	return RuntimeConfig{
		PollInterval:   time.Minute,
		PersistTimeout: time.Second,
		ClaimDuration:  30 * time.Second,
		Lanes:          lanes,
	}
}

func (c RuntimeConfig) laneConfig(definition laneDefinition) LaneConfig {
	if configured, ok := c.Lanes[definition.name]; ok {
		return configured
	}

	return definition.defaultConfig
}

// Validate checks scheduler timing and explicit lane-capacity overrides.
func (c RuntimeConfig) Validate() error {
	if c.PollInterval <= 0 {
		return fmt.Errorf("scheduler poll interval must be positive")
	}

	if c.PersistTimeout <= 0 {
		return fmt.Errorf("execution persist timeout must be positive")
	}
	if c.ClaimDuration <= 0 {
		return fmt.Errorf("execution claim duration must be positive")
	}

	supportedLanes := make(map[string]struct{}, len(laneDefinitions))
	for _, definition := range laneDefinitions {
		supportedLanes[definition.name] = struct{}{}
	}

	configuredLanes := make([]string, 0, len(c.Lanes))
	for name := range c.Lanes {
		configuredLanes = append(configuredLanes, name)
	}
	sort.Strings(configuredLanes)

	var unknownLanes []string
	for _, name := range configuredLanes {
		if _, ok := supportedLanes[name]; !ok {
			unknownLanes = append(unknownLanes, name)
			continue
		}

		if err := c.Lanes[name].validate(name); err != nil {
			return err
		}
	}

	if len(unknownLanes) > 0 {
		return fmt.Errorf("scheduler lane %q is not supported", unknownLanes[0])
	}

	return nil
}

// Config identifies a scheduler and contains its dependencies and bounded
// scheduling behavior.
type Config struct {
	// InstanceID uniquely identifies this scheduler among concurrently running
	// instances and remains stable for its lifetime.
	InstanceID   string
	Dependencies Dependencies
	Runtime      RuntimeConfig
	Policy       PolicyConfig
}

// Validate checks all scheduler dependencies and limits.
func (c Config) Validate() error {
	if err := c.Dependencies.Validate(); err != nil {
		return err
	}

	if err := eventrule.ValidateExecutionClaimOwner(c.InstanceID); err != nil {
		return err
	}

	if err := c.Runtime.Validate(); err != nil {
		return err
	}

	return c.Policy.Validate()
}

func (c Config) runtime() runtime {
	workerCount := 0
	for _, definition := range laneDefinitions {
		workerCount += c.Runtime.laneConfig(definition).Workers
	}

	return runtime{
		store:             c.Dependencies.Store,
		executors:         c.Dependencies.Executors,
		policy:            c.Policy,
		pollInterval:      c.Runtime.PollInterval,
		persistTimeout:    c.Runtime.PersistTimeout,
		claimDuration:     c.Runtime.ClaimDuration,
		wakeCh:            make(chan struct{}, 1),
		fatalWorkerErrors: make(chan error, workerCount),
	}
}
