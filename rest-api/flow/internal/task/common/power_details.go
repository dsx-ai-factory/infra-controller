// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"fmt"

	"github.com/google/uuid"
)

// MaxPowerTasksPerRack limits the outstanding task records returned for power
// details. This is a row limit, not a limit on stored JSON or response bytes.
const MaxPowerTasksPerRack = 256

// PowerTaskLimitError means the rack has more outstanding power tasks than a
// complete power details response supports. The reader returns this error
// instead of any rows; RPC callers translate it to their response contract.
type PowerTaskLimitError struct {
	RackID uuid.UUID
}

// Error identifies the rack and supported record count without task payloads.
func (e *PowerTaskLimitError) Error() string {
	return fmt.Sprintf("rack %s exceeds the limit of %d outstanding power tasks", e.RackID, MaxPowerTasksPerRack)
}

// MaxTaskContextPerRack bounds metadata rows independently of the power-task
// count. Exceeding this row limit leaves primary power tasks available; the
// tray response byte limit removes context references before primary tasks.
const MaxTaskContextPerRack = 256

// TaskContextLimitError means no complete waiting context can be returned.
type TaskContextLimitError struct {
	RackID uuid.UUID
}

// Error identifies the rack without including stored task data.
func (e *TaskContextLimitError) Error() string {
	return fmt.Sprintf("rack %s exceeds the limit of %d tasks for waiting context", e.RackID, MaxTaskContextPerRack)
}
