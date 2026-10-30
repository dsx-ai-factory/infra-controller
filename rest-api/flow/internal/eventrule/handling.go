// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"encoding/base64"
	"encoding/json"
	"fmt"
	"time"

	"github.com/google/uuid"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// HandlingTask identifies one Task created while handling an event.
type HandlingTask struct {
	ID        uuid.UUID
	Status    taskcommon.TaskStatus
	CreatedAt time.Time
}

// Handling is one application of an event rule and its associated work.
type Handling struct {
	ID           uuid.UUID
	EventType    Type
	ResourceID   string
	ResourceType ResourceKind
	Tasks        []HandlingTask
	HandledAt    time.Time
}

// HandlingCursor identifies the last item returned by a handling page.
type HandlingCursor struct {
	HandledAt time.Time
	ID        uuid.UUID
}

// Validate checks that the cursor contains a complete pagination key.
func (c HandlingCursor) Validate() error {
	if c.HandledAt.IsZero() {
		return fmt.Errorf("handling cursor handled time is required")
	}

	if c.ID == uuid.Nil {
		return fmt.Errorf("handling cursor ID is required")
	}

	return nil
}

const handlingCursorVersion = 1

type encodedHandlingCursor struct {
	Version   int       `json:"v"`
	HandledAt time.Time `json:"handledAt"`
	ID        uuid.UUID `json:"id"`
}

// EncodeHandlingCursor returns an opaque, URL-safe continuation cursor.
func EncodeHandlingCursor(cursor HandlingCursor) (string, error) {
	if err := cursor.Validate(); err != nil {
		return "", fmt.Errorf("encode handling cursor: %w", err)
	}

	payload, err := json.Marshal(
		encodedHandlingCursor{
			Version:   handlingCursorVersion,
			HandledAt: cursor.HandledAt.UTC(),
			ID:        cursor.ID,
		},
	)
	if err != nil {
		return "", fmt.Errorf("encode handling cursor: %w", err)
	}

	return base64.RawURLEncoding.EncodeToString(payload), nil
}

// DecodeHandlingCursor validates and decodes an opaque continuation cursor.
func DecodeHandlingCursor(value string) (*HandlingCursor, error) {
	if value == "" {
		return nil, nil
	}

	payload, err := base64.RawURLEncoding.DecodeString(value)
	if err != nil {
		return nil, fmt.Errorf("decode handling cursor: %w", err)
	}

	var encoded encodedHandlingCursor
	if err := json.Unmarshal(payload, &encoded); err != nil {
		return nil, fmt.Errorf("decode handling cursor: %w", err)
	}

	if encoded.Version != handlingCursorVersion {
		return nil, fmt.Errorf("handling cursor is invalid")
	}

	cursor := &HandlingCursor{
		HandledAt: encoded.HandledAt.UTC(),
		ID:        encoded.ID,
	}

	if err := cursor.Validate(); err != nil {
		return nil, fmt.Errorf("decode handling cursor: %w", err)
	}

	return cursor, nil
}
