// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"
	"errors"
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

const defaultMaxDeliveryAttempts = 3

// Config controls event delivery retries.
type Config struct {
	// RetryDelay is the pause between nonterminal delivery attempts.
	RetryDelay time.Duration
	// MaxAttempts includes the initial delivery and bounds how long a scheduled
	// collection job can retain its worker during a persistent sink failure.
	MaxAttempts int
}

// DefaultConfig returns the default event delivery behavior.
func DefaultConfig() Config {
	return Config{
		RetryDelay:  time.Second,
		MaxAttempts: defaultMaxDeliveryAttempts,
	}
}

// Validate checks the ingestion configuration.
func (c Config) Validate() error {
	if c.RetryDelay <= 0 {
		return fmt.Errorf("event delivery retry delay must be positive")
	}
	if c.MaxAttempts <= 0 {
		return fmt.Errorf("event delivery max attempts must be positive")
	}

	return nil
}

// Pipeline registers event sources and delivers their normalized events.
type Pipeline struct {
	registry *registry
	sink     Sink
	config   Config
}

// Source delivers events under one registered source name.
type Source struct {
	name     string
	pipeline *Pipeline
}

// New constructs an event ingestion pipeline.
func New(sink Sink, config Config) (*Pipeline, error) {
	if sink == nil {
		return nil, fmt.Errorf("event sink is required")
	}

	if err := config.Validate(); err != nil {
		return nil, err
	}

	return &Pipeline{
		registry: newRegistry(),
		sink:     sink,
		config:   config,
	}, nil
}

// RegisterSource returns an ingestion entry point bound to one stable source
// name.
func (p *Pipeline) RegisterSource(sourceName string) (*Source, error) {
	if p == nil {
		return nil, fmt.Errorf("event ingestion pipeline is nil")
	}

	if err := p.registry.register(sourceName); err != nil {
		return nil, err
	}

	return &Source{
		name:     sourceName,
		pipeline: p,
	}, nil
}

// Ingest applies the registered source name and delivers one event. A
// nonterminal sink failure is retried with the same EventKey up to the
// configured attempt limit. Terminal processing failures are not retried.
func (s *Source) Ingest(ctx context.Context, envelope eventrule.Envelope) error {
	if s == nil {
		return fmt.Errorf("event source is nil")
	}

	envelope.Key.SourceName = s.name
	if err := envelope.Validate(); err != nil {
		return fmt.Errorf("event source %q: %w", s.name, err)
	}

	return s.pipeline.deliver(ctx, envelope)
}

func (p *Pipeline) deliver(ctx context.Context, envelope eventrule.Envelope) error {
	for attempt := 1; ; attempt++ {
		err := p.sink.Process(ctx, envelope.Clone())
		if err == nil {
			return nil
		}

		// A dependency may return its own operational error after cancellation.
		// Prefer the context error so the scheduler recognizes a canceled job.
		if ctxErr := ctx.Err(); ctxErr != nil {
			return ctxErr
		}

		if errors.Is(err, eventrule.ErrTerminalProcessing) {
			return fmt.Errorf("deliver event %q: %w", envelope.Key.SourceKey, err)
		}

		if attempt == p.config.MaxAttempts {
			// Return control to the scheduled job. Its next run can redeliver the
			// same stable event key after the dependency recovers.
			return fmt.Errorf(
				"deliver event %q after %d attempts: %w",
				envelope.Key.SourceKey,
				attempt,
				err,
			)
		}

		timer := time.NewTimer(p.config.RetryDelay)
		select {
		case <-ctx.Done():
			timer.Stop()

			return ctx.Err()
		case <-timer.C:
		}
	}
}
