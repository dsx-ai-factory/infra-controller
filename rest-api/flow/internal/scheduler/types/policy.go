// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

// Policy controls what happens when a trigger fires while a job is running.
type Policy int

const (
	// Skip drops the new event if the worker is busy. Default.
	Skip Policy = iota
	// Queue keeps only the latest pending event; delivers it when the worker
	// is free. Earlier events are discarded.
	Queue
	// QueueAll buffers every event and delivers them in FIFO order.
	// Use this when each event carries unique data that must not be dropped.
	QueueAll
	// Replace cancels the current job and starts a new one.
	Replace
)
