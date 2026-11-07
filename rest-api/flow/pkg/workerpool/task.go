// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workerpool

import (
	"context"
	"time"
)

// Task represents a unit of work that can be executed by a worker.
// Users implement this interface to define their custom operations.
type Task interface {
	// Execute performs the actual work. It receives a context for cancellation
	// and should return an error if the operation fails.
	Execute(ctx context.Context) error

	// ID returns a unique identifier for this task (optional, for logging/tracking)
	ID() string
}

// Job wraps a Task with additional metadata for internal tracking.
type Job struct {
	Task       Task
	SubmitTime time.Time
	StartTime  time.Time
	EndTime    time.Time
	Error      error
	ResultCh   chan<- JobResult // Optional result channel
}

// JobResult contains the result of a job execution.
type JobResult struct {
	JobID     string
	Task      Task
	Error     error
	Duration  time.Duration
	StartTime time.Time
	EndTime   time.Time
}

// TaskFunc is a convenience type that allows functions to implement the Task interface.
type TaskFunc struct {
	id string
	fn func(ctx context.Context) error
}

// NewTaskFunc creates a new TaskFunc with the given ID and function.
func NewTaskFunc(id string, fn func(ctx context.Context) error) *TaskFunc {
	return &TaskFunc{
		id: id,
		fn: fn,
	}
}

// Execute implements the Task interface.
func (tf *TaskFunc) Execute(ctx context.Context) error {
	return tf.fn(ctx)
}

// ID implements the Task interface.
func (tf *TaskFunc) ID() string {
	return tf.id
}
