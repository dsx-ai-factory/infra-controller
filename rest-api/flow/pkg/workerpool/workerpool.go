// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workerpool

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"time"
)

// WorkerPool manages a pool of workers that execute tasks concurrently.
type WorkerPool struct {
	config  *Config
	jobs    chan *Job
	workers []*worker
	wg      sync.WaitGroup
	ctx     context.Context
	cancel  context.CancelFunc
	started int32
	stopped int32
	mu      sync.RWMutex // protects jobs channel close vs. concurrent sends

	// Metrics
	metrics *metricsCollector
}

// Metrics tracks worker pool statistics.
type Metrics struct {
	JobsSubmitted   int64
	JobsCompleted   int64
	JobsFailed      int64
	WorkersActive   int32
	TotalWorkers    int32
	AverageExecTime time.Duration
}

// metricsCollector is internal struct for collecting metrics data.
type metricsCollector struct {
	Metrics
	mu        sync.RWMutex
	execTimes []time.Duration
}

// worker represents a single worker goroutine.
type worker struct {
	id   int
	pool *WorkerPool
}

// New creates a new WorkerPool with the given configuration.
func New(config *Config) *WorkerPool {
	if config == nil {
		config = DefaultConfig()
	}

	if err := config.Validate(); err != nil {
		// This shouldn't happen with current validation, but defensive programming
		config = DefaultConfig()
	}

	ctx, cancel := context.WithCancel(context.Background())

	var jobQueue chan *Job
	if config.QueueSize > 0 {
		jobQueue = make(chan *Job, config.QueueSize)
	} else {
		jobQueue = make(chan *Job)
	}

	metrics := &metricsCollector{
		Metrics: Metrics{
			TotalWorkers: int32(config.MaxWorkers),
		},
		execTimes: make([]time.Duration, 0, 1000), // Pre-allocate for efficiency
	}

	return &WorkerPool{
		config:  config,
		jobs:    jobQueue,
		workers: make([]*worker, config.MaxWorkers),
		ctx:     ctx,
		cancel:  cancel,
		metrics: metrics,
	}
}

// Start initializes and starts all workers in the pool.
func (wp *WorkerPool) Start() error {
	if !atomic.CompareAndSwapInt32(&wp.started, 0, 1) {
		return fmt.Errorf("worker pool is already started")
	}

	// Create and start workers
	for i := range wp.config.MaxWorkers {
		worker := &worker{
			id:   i + 1,
			pool: wp,
		}
		wp.workers[i] = worker

		wp.wg.Add(1)
		go worker.run()
	}

	return nil
}

// Stop gracefully shuts down the worker pool.
// It waits for all running tasks to complete unless the context is cancelled.
func (wp *WorkerPool) Stop() error {
	if !atomic.CompareAndSwapInt32(&wp.stopped, 0, 1) {
		return fmt.Errorf("worker pool is already stopped")
	}

	// Cancel context to signal workers to stop
	wp.cancel()

	// Close job queue to prevent new submissions
	wp.mu.Lock()
	close(wp.jobs)
	wp.mu.Unlock()

	// Wait for all workers to finish
	wp.wg.Wait()

	return nil
}

// Submit adds a task to the worker pool for execution.
func (wp *WorkerPool) Submit(task Task) error {
	return wp.SubmitWithResult(task, nil)
}

// SubmitWithResult adds a task to the worker pool and optionally sends the result to the provided channel.
func (wp *WorkerPool) SubmitWithResult(task Task, resultCh chan<- JobResult) error {
	wp.mu.RLock()
	defer wp.mu.RUnlock()

	if atomic.LoadInt32(&wp.stopped) == 1 {
		return fmt.Errorf("worker pool is stopped")
	}

	if atomic.LoadInt32(&wp.started) == 0 {
		return fmt.Errorf("worker pool is not started")
	}

	job := &Job{
		Task:       task,
		SubmitTime: time.Now(),
		ResultCh:   resultCh,
	}

	select {
	case wp.jobs <- job:
		atomic.AddInt64(&wp.metrics.JobsSubmitted, 1)
		return nil
	case <-wp.ctx.Done():
		return fmt.Errorf("worker pool is shutting down")
	default:
		return fmt.Errorf("job queue is full")
	}
}

// GetMetrics returns a copy of the current metrics.
func (wp *WorkerPool) GetMetrics() Metrics {
	if !wp.config.EnableMetrics {
		return Metrics{}
	}

	metrics := Metrics{
		JobsSubmitted: atomic.LoadInt64(&wp.metrics.JobsSubmitted),
		JobsCompleted: atomic.LoadInt64(&wp.metrics.JobsCompleted),
		JobsFailed:    atomic.LoadInt64(&wp.metrics.JobsFailed),
		WorkersActive: atomic.LoadInt32(&wp.metrics.WorkersActive),
		TotalWorkers:  wp.metrics.TotalWorkers,
	}

	wp.metrics.mu.RLock()
	defer wp.metrics.mu.RUnlock()

	// Calculate average execution time
	if len(wp.metrics.execTimes) > 0 {
		var total time.Duration
		for _, d := range wp.metrics.execTimes {
			total += d
		}
		metrics.AverageExecTime = total /
			time.Duration(len(wp.metrics.execTimes))
	}

	return metrics
}

// IsRunning returns true if the worker pool is currently running.
func (wp *WorkerPool) IsRunning() bool {
	return atomic.LoadInt32(&wp.started) == 1 && atomic.LoadInt32(&wp.stopped) == 0
}

// run is the main worker loop.
func (w *worker) run() {
	defer w.pool.wg.Done()

	atomic.AddInt32(&w.pool.metrics.WorkersActive, 1)
	defer atomic.AddInt32(&w.pool.metrics.WorkersActive, -1)

	for {
		select {
		case job, ok := <-w.pool.jobs:
			if !ok {
				// Job queue is closed, worker should exit
				return
			}

			w.executeJob(job)

		case <-w.pool.ctx.Done():
			// Pool is shutting down
			return

		case <-time.After(w.pool.config.WorkerTimeout):
			// Timeout reached, check if we should continue
			if w.pool.ctx.Err() != nil {
				return
			}
		}
	}
}

// executeJob executes a single job.
func (w *worker) executeJob(job *Job) {
	job.StartTime = time.Now()

	// Create context for this job
	var ctx context.Context
	var cancel context.CancelFunc

	if w.pool.config.TaskTimeout > 0 {
		ctx, cancel = context.WithTimeout(w.pool.ctx, w.pool.config.TaskTimeout)
		defer cancel()
	} else {
		ctx = w.pool.ctx
	}

	// Execute the task
	job.Error = job.Task.Execute(ctx)
	job.EndTime = time.Now()

	duration := job.EndTime.Sub(job.StartTime)

	// Update metrics
	if w.pool.config.EnableMetrics {
		if job.Error != nil {
			atomic.AddInt64(&w.pool.metrics.JobsFailed, 1)
		} else {
			atomic.AddInt64(&w.pool.metrics.JobsCompleted, 1)
		}

		// Store execution time for average calculation
		w.pool.metrics.mu.Lock()
		if len(w.pool.metrics.execTimes) >= 1000 {
			// Rotate out old times to prevent unlimited growth
			w.pool.metrics.execTimes =
				w.pool.metrics.execTimes[100:]
		}
		w.pool.metrics.execTimes = append(
			w.pool.metrics.execTimes, duration,
		)
		w.pool.metrics.mu.Unlock()
	}

	// Send result if channel is provided
	if job.ResultCh != nil {
		result := JobResult{
			JobID:     job.Task.ID(),
			Task:      job.Task,
			Error:     job.Error,
			Duration:  duration,
			StartTime: job.StartTime,
			EndTime:   job.EndTime,
		}

		select {
		case job.ResultCh <- result:
		case <-w.pool.ctx.Done():
			// Pool is shutting down, don't block
		default:
			// Result channel is full or not ready, don't block
		}
	}
}
