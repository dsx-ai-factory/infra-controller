// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"fmt"
	"strings"
	"time"
)

// everySchedulePrefix is the Temporal cron descriptor that carries its period directly.
const everySchedulePrefix = "@every "

// InventoryIntervalFromSchedule reports how much time can pass between two inventory
// collections on the given Temporal cron schedule.
//
// Only the `@every` descriptor is accepted. A field-based expression carries no period, and its
// gaps can be uneven in ways that are not apparent from the first few fire times: `* 9 * * *`
// fires every minute inside one hour, so it looks like a 1 minute schedule while really leaving
// a 23 hour gap. Cloud decides when reported data is stale from this value, so reading it too
// low would have Cloud acting on inventory far older than it assumes. Requiring a fixed period
// makes the interval exact instead of estimated.
func InventoryIntervalFromSchedule(schedule string) (time.Duration, error) {
	period, found := strings.CutPrefix(schedule, everySchedulePrefix)
	if !found {
		return 0, fmt.Errorf("schedule %q must be an %q duration, such as %q", schedule, strings.TrimSpace(everySchedulePrefix), "@every 3m")
	}

	// Temporal parses this descriptor's remainder with time.ParseDuration, so accepting exactly
	// what that accepts keeps this from admitting a schedule Temporal would then reject.
	interval, err := time.ParseDuration(strings.TrimSpace(period))
	if err != nil {
		return 0, fmt.Errorf("schedule %q does not carry a valid duration: %w", schedule, err)
	}
	if interval <= 0 {
		return 0, fmt.Errorf("schedule %q must carry a positive duration", schedule)
	}

	return interval, nil
}
