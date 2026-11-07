// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ingestion

import (
	"context"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
)

// Sink accepts normalized events. A nil error means the event was accepted as
// a no-op, duplicate, or complete persisted execution plan.
type Sink interface {
	Process(context.Context, eventrule.Envelope) error
}
