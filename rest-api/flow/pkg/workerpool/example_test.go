// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workerpool

import (
	"context"
	"fmt"
	"log"
	"time"
)

// ExampleWorkerPool demonstrates basic usage of the worker pool.
func ExampleWorkerPool() {
	// Create a worker pool configuration
	config := DefaultConfig()
	config.MaxWorkers = 3
	config.QueueSize = 100
	config.TaskTimeout = 30 * time.Second

	// Create and start the worker pool
	pool := New(config)
	err := pool.Start()
	if err != nil {
		log.Fatalf("Failed to start worker pool: %v", err)
	}
	defer func() {
		err := pool.Stop()
		if err != nil {
			log.Printf("Failed to stop worker pool: %v", err)
		}
	}()

	// Submit tasks using TaskFunc for simple operations
	for i := 0; i < 5; i++ {
		taskID := fmt.Sprintf("task-%d", i)
		task := NewTaskFunc(taskID, func(ctx context.Context) error {
			fmt.Printf("Executing %s\n", taskID)
			time.Sleep(100 * time.Millisecond) // Simulate work
			fmt.Printf("Completed %s\n", taskID)
			return nil
		})

		err := pool.Submit(task)
		if err != nil {
			log.Printf("Failed to submit %s: %v", taskID, err)
		}
	}

	// Wait for tasks to complete
	time.Sleep(500 * time.Millisecond)

	// Print metrics
	metrics := pool.GetMetrics()
	fmt.Printf("Jobs submitted: %d\n", metrics.JobsSubmitted)
	fmt.Printf("Jobs completed: %d\n", metrics.JobsCompleted)
	fmt.Printf("Jobs failed: %d\n", metrics.JobsFailed)
	fmt.Printf("Average execution time: %v\n", metrics.AverageExecTime)
}

// ExampleWorkerPool_withResults demonstrates using result channels.
func ExampleWorkerPool_withResults() {
	config := DefaultConfig()
	config.MaxWorkers = 2

	pool := New(config)
	err := pool.Start()
	if err != nil {
		log.Fatalf("Failed to start worker pool: %v", err)
	}
	defer func() {
		err := pool.Stop()
		if err != nil {
			log.Printf("Failed to stop worker pool: %v", err)
		}
	}()

	// Create a result channel
	results := make(chan JobResult, 5)

	// Submit tasks with result reporting
	for i := 0; i < 5; i++ {
		taskID := fmt.Sprintf("result-task-%d", i)
		task := NewTaskFunc(taskID, func(ctx context.Context) error {
			time.Sleep(50 * time.Millisecond) // Simulate work
			return nil
		})

		err := pool.SubmitWithResult(task, results)
		if err != nil {
			log.Printf("Failed to submit %s: %v", taskID, err)
		}
	}

	// Collect results
CollectResults:
	for i := 0; i < 5; i++ {
		select {
		case result := <-results:
			if result.Error != nil {
				fmt.Printf("Task %s failed: %v\n", result.JobID, result.Error)
			} else {
				fmt.Printf("Task %s completed in %v\n", result.JobID, result.Duration)
			}
		case <-time.After(2 * time.Second):
			fmt.Printf("Timeout waiting for result %d\n", i)
			break CollectResults
		}
	}
}

// DataProcessingTask is an example of a custom task implementation
type DataProcessingTask struct {
	id   string
	data []byte
}

func NewDataProcessingTask(id string, data []byte) *DataProcessingTask {
	return &DataProcessingTask{
		id:   id,
		data: data,
	}
}

func (dpt *DataProcessingTask) ID() string {
	return dpt.id
}

func (dpt *DataProcessingTask) Execute(ctx context.Context) error {
	// Simulate data processing
	fmt.Printf("Processing data for %s (size: %d bytes)\n", dpt.id, len(dpt.data))

	// Check for cancellation periodically
	select {
	case <-ctx.Done():
		return ctx.Err()
	default:
	}

	// Simulate processing time
	time.Sleep(100 * time.Millisecond)

	fmt.Printf("Finished processing %s\n", dpt.id)
	return nil
}

// ExampleWorkerPool_customTasks demonstrates using custom task types.
func ExampleWorkerPool_customTasks() {
	config := DefaultConfig()
	config.MaxWorkers = 2
	config.QueueSize = 50

	pool := New(config)
	err := pool.Start()
	if err != nil {
		log.Fatalf("Failed to start worker pool: %v", err)
	}
	defer func() {
		err := pool.Stop()
		if err != nil {
			log.Printf("Failed to stop worker pool: %v", err)
		}
	}()

	// Submit data processing tasks
	for i := 0; i < 3; i++ {
		data := make([]byte, (i+1)*100) // Different data sizes
		task := NewDataProcessingTask(fmt.Sprintf("data-%d", i), data)

		err := pool.Submit(task)
		if err != nil {
			log.Printf("Failed to submit data processing task: %v", err)
		}
	}

	// Wait for processing to complete
	time.Sleep(500 * time.Millisecond)
}
