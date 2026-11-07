// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workerpool

import (
	"time"
)

const (
	defaultMaxWorkers    = 10
	defaultQueueSize     = 1000
	defaultWorkerTimeout = 5 * time.Second
)

// Config defines the configuration for a worker pool.
type Config struct {
	// MaxWorkers is the maximum number of workers to spawn.
	// Must be greater than 0. Default is 10.
	MaxWorkers int

	// QueueSize is the maximum number of jobs that can be queued.
	// If 0, the queue is unbounded (not recommended for production).
	// Default is 1000.
	QueueSize int

	// WorkerTimeout is the maximum time a worker will wait for a new job
	// before checking if it should shut down. Default is 5 seconds.
	WorkerTimeout time.Duration

	// TaskTimeout is the maximum time allowed for a single task execution.
	// If 0, no timeout is applied. Default is 0 (no timeout).
	TaskTimeout time.Duration

	// EnableMetrics determines whether to collect execution metrics.
	// Default is true.
	EnableMetrics bool
}

// DefaultConfig returns a Config with sensible defaults.
func DefaultConfig() *Config {
	return &Config{
		MaxWorkers:    defaultMaxWorkers,
		QueueSize:     defaultQueueSize,
		WorkerTimeout: defaultWorkerTimeout,
		TaskTimeout:   0, // No timeout by default
		EnableMetrics: true,
	}
}

// Validate checks if the configuration is valid and applies defaults where needed.
func (c *Config) Validate() error {
	if c.MaxWorkers <= 0 {
		c.MaxWorkers = defaultMaxWorkers
	}

	if c.QueueSize < 0 {
		c.QueueSize = defaultQueueSize
	}

	if c.WorkerTimeout <= 0 {
		c.WorkerTimeout = defaultWorkerTimeout
	}

	return nil
}
