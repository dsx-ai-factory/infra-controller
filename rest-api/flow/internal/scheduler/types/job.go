// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import "context"

// Job is the unit of work executed by the scheduler.
type Job interface {
	Name() string
	Run(ctx context.Context, ev Event) error
}
