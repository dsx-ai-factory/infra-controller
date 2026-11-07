// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

// EventType identifies the kind of event.
type EventType string

const (
	EventLeakDetected EventType = "leak.detected"
)

// Event carries a type and an arbitrary payload.
type Event struct {
	Type    EventType
	Payload any
}
