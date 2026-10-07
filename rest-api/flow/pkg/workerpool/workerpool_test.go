// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workerpool

import (
	"context"
	"errors"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

// mockTask is a test implementation of Task
type mockTask struct {
	id        string
	duration  time.Duration
	shouldErr bool
	executed  int32
}

func newMockTask(id string, duration time.Duration, shouldErr bool) *mockTask {
	return &mockTask{
		id:        id,
		duration:  duration,
		shouldErr: shouldErr,
	}
}

func (mt *mockTask) Execute(ctx context.Context) error {
	atomic.AddInt32(&mt.executed, 1)

	if mt.duration > 0 {
		select {
		case <-time.After(mt.duration):
		case <-ctx.Done():
			return ctx.Err()
		}
	}

	if mt.shouldErr {
		return errors.New("mock task error")
	}

	return nil
}

func (mt *mockTask) ID() string {
	return mt.id
}

func (mt *mockTask) wasExecuted() bool {
	return atomic.LoadInt32(&mt.executed) > 0
}

func TestWorkerPool_Basic(t *testing.T) {
	config := DefaultConfig()
	config.MaxWorkers = 2
	config.QueueSize = 10

	pool := New(config)

	if pool == nil {
		t.Fatal("Expected non-nil pool")
	}

	if pool.IsRunning() {
		t.Fatal("Pool should not be running before Start()")
	}

	// Start the pool
	err := pool.Start()
	if err != nil {
		t.Fatalf("Failed to start pool: %v", err)
	}

	if !pool.IsRunning() {
		t.Fatal("Pool should be running after Start()")
	}

	// Submit a task
	task := newMockTask("test-1", 10*time.Millisecond, false)
	err = pool.Submit(task)
	if err != nil {
		t.Fatalf("Failed to submit task: %v", err)
	}

	// Wait a bit for execution
	time.Sleep(50 * time.Millisecond)

	if !task.wasExecuted() {
		t.Fatal("Task should have been executed")
	}

	// Stop the pool
	err = pool.Stop()
	if err != nil {
		t.Fatalf("Failed to stop pool: %v", err)
	}

	if pool.IsRunning() {
		t.Fatal("Pool should not be running after Stop()")
	}
}

func TestWorkerPool_MultipleWorkers(t *testing.T) {
	config := DefaultConfig()
	config.MaxWorkers = 3
	config.QueueSize = 100

	pool := New(config)
	err := pool.Start()
	if err != nil {
		t.Fatalf("Failed to start pool: %v", err)
	}
	defer pool.Stop()

	// Submit multiple tasks
	numTasks := 10
	tasks := make([]*mockTask, numTasks)

	for i := 0; i < numTasks; i++ {
		tasks[i] = newMockTask(
			fmt.Sprintf("task-%d", i),
			20*time.Millisecond,
			false,
		)
		err := pool.Submit(tasks[i])
		if err != nil {
			t.Fatalf("Failed to submit task %d: %v", i, err)
		}
	}

	// Wait for all tasks to complete
	time.Sleep(200 * time.Millisecond)

	// Verify all tasks were executed
	for i, task := range tasks {
		if !task.wasExecuted() {
			t.Errorf("Task %d was not executed", i)
		}
	}

	// Check metrics
	metrics := pool.GetMetrics()
	if metrics.JobsSubmitted != int64(numTasks) {
		t.Errorf("Expected %d jobs submitted, got %d", numTasks, metrics.JobsSubmitted)
	}
	if metrics.JobsCompleted != int64(numTasks) {
		t.Errorf("Expected %d jobs completed, got %d", numTasks, metrics.JobsCompleted)
	}
	if metrics.JobsFailed != 0 {
		t.Errorf("Expected 0 jobs failed, got %d", metrics.JobsFailed)
	}
}

func TestWorkerPool_TaskErrors(t *testing.T) {
	config := DefaultConfig()
	config.MaxWorkers = 2

	pool := New(config)
	err := pool.Start()
	if err != nil {
		t.Fatalf("Failed to start pool: %v", err)
	}
	defer pool.Stop()

	// Submit tasks that will fail
	numFailingTasks := 3
	numSuccessfulTasks := 2

	for i := 0; i < numFailingTasks; i++ {
		task := newMockTask(fmt.Sprintf("fail-%d", i), 0, true)
		err := pool.Submit(task)
		if err != nil {
			t.Fatalf("Failed to submit failing task: %v", err)
		}
	}

	for i := 0; i < numSuccessfulTasks; i++ {
		task := newMockTask(fmt.Sprintf("success-%d", i), 0, false)
		err := pool.Submit(task)
		if err != nil {
			t.Fatalf("Failed to submit successful task: %v", err)
		}
	}

	// Wait for completion
	time.Sleep(100 * time.Millisecond)

	metrics := pool.GetMetrics()
	expectedTotal := numFailingTasks + numSuccessfulTasks

	if metrics.JobsSubmitted != int64(expectedTotal) {
		t.Errorf("Expected %d jobs submitted, got %d", expectedTotal, metrics.JobsSubmitted)
	}
	if metrics.JobsCompleted != int64(numSuccessfulTasks) {
		t.Errorf("Expected %d jobs completed, got %d", numSuccessfulTasks, metrics.JobsCompleted)
	}
	if metrics.JobsFailed != int64(numFailingTasks) {
		t.Errorf("Expected %d jobs failed, got %d", numFailingTasks, metrics.JobsFailed)
	}
}

func TestWorkerPool_SubmitWithResult(t *testing.T) {
	config := DefaultConfig()
	config.MaxWorkers = 1

	pool := New(config)
	err := pool.Start()
	if err != nil {
		t.Fatalf("Failed to start pool: %v", err)
	}
	defer pool.Stop()

	resultCh := make(chan JobResult, 1)
	task := newMockTask("result-test", 10*time.Millisecond, false)

	err = pool.SubmitWithResult(task, resultCh)
	if err != nil {
		t.Fatalf("Failed to submit task with result: %v", err)
	}

	select {
	case result := <-resultCh:
		if result.Error != nil {
			t.Errorf("Expected no error, got: %v", result.Error)
		}
		if result.JobID != "result-test" {
			t.Errorf("Expected job ID 'result-test', got '%s'", result.JobID)
		}
		if result.Duration <= 0 {
			t.Error("Expected positive duration")
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("Timeout waiting for result")
	}
}

func TestWorkerPool_QueueFull(t *testing.T) {
	config := DefaultConfig()
	config.MaxWorkers = 1
	config.QueueSize = 2

	pool := New(config)
	err := pool.Start()
	if err != nil {
		t.Fatalf("Failed to start pool: %v", err)
	}
	defer pool.Stop()

	// Submit tasks that will block worker
	blockingTask1 := newMockTask("block-1", 200*time.Millisecond, false)
	blockingTask2 := newMockTask("block-2", 200*time.Millisecond, false)

	// First task goes to worker immediately
	err = pool.Submit(blockingTask1)
	if err != nil {
		t.Fatalf("Failed to submit first task: %v", err)
	}

	// Give first task time to start executing
	time.Sleep(10 * time.Millisecond)

	// Second task goes to queue
	err = pool.Submit(blockingTask2)
	if err != nil {
		t.Fatalf("Failed to submit second task: %v", err)
	}

	// Third task should also go to queue (queue size is 2)
	blockingTask3 := newMockTask("block-3", 200*time.Millisecond, false)
	err = pool.Submit(blockingTask3)
	if err != nil {
		t.Fatalf("Failed to submit third task: %v", err)
	}

	// This should fail (queue is now full: 2 tasks in queue + 1 executing)
	extraTask := newMockTask("extra", 0, false)
	err = pool.Submit(extraTask)
	if err == nil {
		t.Fatal("Expected error when queue is full")
	}
}

func TestWorkerPool_TaskTimeout(t *testing.T) {
	config := DefaultConfig()
	config.MaxWorkers = 1
	config.TaskTimeout = 50 * time.Millisecond

	pool := New(config)
	err := pool.Start()
	if err != nil {
		t.Fatalf("Failed to start pool: %v", err)
	}
	defer pool.Stop()

	resultCh := make(chan JobResult, 1)

	// Submit a task that takes longer than the timeout
	longTask := newMockTask("long-task", 100*time.Millisecond, false)
	err = pool.SubmitWithResult(longTask, resultCh)
	if err != nil {
		t.Fatalf("Failed to submit long task: %v", err)
	}

	select {
	case result := <-resultCh:
		if result.Error == nil {
			t.Error("Expected timeout error")
		}
		if !errors.Is(result.Error, context.DeadlineExceeded) {
			t.Errorf("Expected context.DeadlineExceeded, got: %v", result.Error)
		}
	case <-time.After(200 * time.Millisecond):
		t.Fatal("Timeout waiting for result")
	}
}

func TestTaskFunc(t *testing.T) {
	executed := false
	taskFunc := NewTaskFunc("func-task", func(ctx context.Context) error {
		executed = true
		return nil
	})

	if taskFunc.ID() != "func-task" {
		t.Errorf("Expected ID 'func-task', got '%s'", taskFunc.ID())
	}

	err := taskFunc.Execute(context.Background())
	if err != nil {
		t.Errorf("Unexpected error: %v", err)
	}

	if !executed {
		t.Error("TaskFunc was not executed")
	}
}

func TestWorkerPool_ConcurrentSubmissions(t *testing.T) {
	config := DefaultConfig()
	config.MaxWorkers = 5
	config.QueueSize = 1000

	pool := New(config)
	err := pool.Start()
	if err != nil {
		t.Fatalf("Failed to start pool: %v", err)
	}
	defer pool.Stop()

	// Submit tasks concurrently
	numGoroutines := 10
	tasksPerGoroutine := 20
	var wg sync.WaitGroup
	var submissionErrors int32

	for i := 0; i < numGoroutines; i++ {
		wg.Add(1)
		go func(goroutineID int) {
			defer wg.Done()
			for j := 0; j < tasksPerGoroutine; j++ {
				task := newMockTask(
					fmt.Sprintf("task-%d-%d", goroutineID, j),
					time.Millisecond,
					false,
				)
				if err := pool.Submit(task); err != nil {
					atomic.AddInt32(&submissionErrors, 1)
				}
			}
		}(i)
	}

	wg.Wait()

	if submissionErrors > 0 {
		t.Errorf("Got %d submission errors", submissionErrors)
	}

	// Wait for all tasks to complete
	time.Sleep(500 * time.Millisecond)

	metrics := pool.GetMetrics()
	expectedTasks := int64(numGoroutines * tasksPerGoroutine)

	if metrics.JobsSubmitted != expectedTasks {
		t.Errorf("Expected %d jobs submitted, got %d", expectedTasks, metrics.JobsSubmitted)
	}
}

func TestConfig_Validation(t *testing.T) {
	tests := []struct {
		name   string
		config *Config
		want   *Config
	}{
		{
			name: "valid config",
			config: &Config{
				MaxWorkers:    5,
				QueueSize:     100,
				WorkerTimeout: time.Second,
			},
			want: &Config{
				MaxWorkers:    5,
				QueueSize:     100,
				WorkerTimeout: time.Second,
			},
		},
		{
			name: "invalid max workers",
			config: &Config{
				MaxWorkers: 0,
				QueueSize:  0, // 0 is valid (unbounded queue)
			},
			want: &Config{
				MaxWorkers:    10,
				QueueSize:     0, // Should remain 0 (unbounded)
				WorkerTimeout: 5 * time.Second,
			},
		},
		{
			name: "negative queue size",
			config: &Config{
				QueueSize: -1,
			},
			want: &Config{
				MaxWorkers:    10,
				QueueSize:     1000,
				WorkerTimeout: 5 * time.Second,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.config.Validate()
			if err != nil {
				t.Errorf("Validate() returned error: %v", err)
			}

			if tt.config.MaxWorkers != tt.want.MaxWorkers {
				t.Errorf("MaxWorkers = %d, want %d", tt.config.MaxWorkers, tt.want.MaxWorkers)
			}
			if tt.config.QueueSize != tt.want.QueueSize {
				t.Errorf("QueueSize = %d, want %d", tt.config.QueueSize, tt.want.QueueSize)
			}
			if tt.config.WorkerTimeout != tt.want.WorkerTimeout {
				t.Errorf("WorkerTimeout = %v, want %v", tt.config.WorkerTimeout, tt.want.WorkerTimeout)
			}
		})
	}
}

func TestWorkerPool_StressSubmitDuringStop(t *testing.T) {
	for iter := 0; iter < 200; iter++ {
		config := DefaultConfig()
		config.MaxWorkers = 4
		config.QueueSize = 10

		pool := New(config)
		if err := pool.Start(); err != nil {
			t.Fatalf("failed to start pool: %v", err)
		}

		const numGoroutines = 20
		var wg sync.WaitGroup
		var ready sync.WaitGroup
		ready.Add(numGoroutines)

		for g := 0; g < numGoroutines; g++ {
			wg.Add(1)
			go func(id int) {
				defer wg.Done()
				defer func() {
					if r := recover(); r != nil {
						t.Errorf("Submit panicked: %v", r)
					}
				}()
				signaledReady := false
				for j := 0; j < 50; j++ {
					task := newMockTask(fmt.Sprintf("stress-%d-%d-%d", iter, id, j), 0, false)
					_ = pool.Submit(task) // errors are expected once stopped; panics are not
					if !signaledReady {
						signaledReady = true
						ready.Done()
					}
				}
			}(g)
		}

		// Wait until every goroutine has made at least one submission
		// attempt, so Stop genuinely overlaps with active submissions
		// rather than potentially running before any of them start.
		ready.Wait()
		_ = pool.Stop()

		wg.Wait()
	}
}
