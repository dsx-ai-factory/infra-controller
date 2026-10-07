// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package dispatcher advances operation runs by reconciling child task state
// and submitting runnable rack targets.
package dispatcher

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"time"

	"github.com/rs/zerolog/log"
)

// Dispatcher polls runnable operation runs and advances them.
type Dispatcher struct {
	deps Dependencies
	cfg  Config
	now  func() time.Time

	cancel    atomic.Pointer[context.CancelFunc]
	startOnce sync.Once
	done      chan struct{}
}

// New creates an operation-run dispatcher.
func New(deps Dependencies, cfg Config) (*Dispatcher, error) {
	if err := deps.validate(); err != nil {
		return nil, err
	}

	return &Dispatcher{
		deps: deps,
		cfg:  cfg.withDefaults(),
		now: func() time.Time {
			return time.Now().UTC()
		},
		done: make(chan struct{}),
	}, nil
}

// Start launches the background polling loop.
func (d *Dispatcher) Start(ctx context.Context) error {
	if err := d.validate(); err != nil {
		return err
	}

	d.startOnce.Do(
		func() {
			ctx, cancel := context.WithCancel(ctx)
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
func (d *Dispatcher) Stop() {
	cancelPtr := d.cancel.Load()
	if cancelPtr == nil {
		return
	}
	(*cancelPtr)()
	<-d.done
}

// DispatchOnce performs one poll cycle. It is exported for tests and manual
// service hooks; Start uses the same path.
func (d *Dispatcher) DispatchOnce(ctx context.Context) error {
	if err := d.validate(); err != nil {
		return err
	}

	ids, err := d.deps.Store.FetchRunnableIDs(ctx, d.cfg.FetchBatch)
	if err != nil {
		return fmt.Errorf("fetch runnable operation runs: %w", err)
	}

	var errs []error
	for _, id := range ids {
		if err := d.dispatchRun(ctx, id); err != nil {
			errs = append(errs, fmt.Errorf("dispatch operation run %s: %w", id, err))
		}
	}

	return errors.Join(errs...)
}

func (d *Dispatcher) validate() error {
	if d == nil {
		return fmt.Errorf("operation run dispatcher is required")
	}
	return d.deps.validate()
}

// run drives the background poll loop used by Start.
func (d *Dispatcher) run(ctx context.Context) {
	ticker := time.NewTicker(d.cfg.PollInterval)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			if err := d.DispatchOnce(ctx); err != nil {
				log.Error().
					Err(err).
					Msg("operation run dispatcher: poll cycle failed")
			}
		}
	}
}
