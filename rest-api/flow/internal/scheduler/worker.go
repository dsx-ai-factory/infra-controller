// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package scheduler

import (
	"context"
	"errors"
	"runtime/debug"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/scheduler/types"
)

// worker processes workItems one at a time until workCh is closed.
// Blocking receives from the relay provide natural backpressure.
type worker struct {
	job types.Job
}

// run reads workItems from workCh sequentially until the channel is closed.
func (w *worker) run(workCh <-chan workItem) {
	for item := range workCh {
		w.runJob(item)
	}
}

func (w *worker) runJob(item workItem) {
	defer item.cancel()
	// Skip the job if the context was already cancelled before we started.
	// This handles the race where forceStop cancels all contexts after the
	// dispatcher sent the workItem but before the worker begins execution.
	if item.ctx.Err() != nil {
		return
	}
	start := time.Now()
	log.Info().Str("job", w.job.Name()).Msg("job started")

	// Recover from panics inside the job.Run method.
	// Log the panic and stack trace, and continue with the next item.
	defer func() {
		if r := recover(); r != nil {
			log.Error().
				Str("job", w.job.Name()).
				Any("panic", r).
				Str("stack", string(debug.Stack())).
				Dur("duration", time.Since(start)).
				Msg("job panicked")
		}
	}()

	if err := w.job.Run(item.ctx, item.ev); err != nil {
		if errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
			log.Info().
				Str("job", w.job.Name()).
				Dur("duration", time.Since(start)).
				Msg("job canceled")
		} else {
			log.Error().
				Err(err).
				Str("job", w.job.Name()).
				Dur("duration", time.Since(start)).
				Msg("job failed")
		}
	} else {
		log.Info().
			Str("job", w.job.Name()).
			Dur("duration", time.Since(start)).
			Msg("job completed")
	}
}
