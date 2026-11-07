// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"fmt"

	"github.com/google/uuid"
)

// MaxFirmwareTasksPerRack limits a rack's Waiting, Pending, and Running firmware
// task read to 256 records. It does not bound stored JSON or response bytes.
const MaxFirmwareTasksPerRack = 256

// FirmwareTaskLimitError means the reader returned no tasks because the rack
// exceeds MaxFirmwareTasksPerRack. The reader returns a pointer to this internal
// error; callers exposing it through a transport must translate it.
type FirmwareTaskLimitError struct {
	RackID uuid.UUID
}

// Error identifies the rack and limit without including operation payloads.
func (e *FirmwareTaskLimitError) Error() string {
	return fmt.Sprintf("rack %s exceeds the limit of %d outstanding firmware tasks", e.RackID, MaxFirmwareTasksPerRack)
}
