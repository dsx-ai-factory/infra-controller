// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

// InventoryDefaultSchedule is the schedule inventory discovery runs on when the deployment does
// not configure one.
const InventoryDefaultSchedule = "@every 3m"

// EffectiveCronSchedule returns the schedule every inventory discovery cron runs on. The Site
// Agent reports the interval derived from this same value to Cloud, which uses it to decide when
// reported data is stale, so the two must not diverge.
func EffectiveCronSchedule() string {
	if ManagerAccess.Conf.EB.Temporal.TemporalInventorySchedule != "" {
		return ManagerAccess.Conf.EB.Temporal.TemporalInventorySchedule
	}
	return InventoryDefaultSchedule
}
