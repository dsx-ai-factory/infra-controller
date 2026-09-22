// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package dao

import "time"

func timeFromPersistence(value time.Time) time.Time {
	if value.IsZero() {
		return value
	}

	return value.UTC()
}

func optionalTimeFromPersistence(value *time.Time) time.Time {
	if value == nil {
		return time.Time{}
	}

	return timeFromPersistence(*value)
}
