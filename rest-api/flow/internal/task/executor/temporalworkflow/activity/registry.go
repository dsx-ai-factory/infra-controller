// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
)

// Activities holds the per-manager-instance dependencies for all Temporal
// activities. Construct one via New and pass its methods to each Temporal
// worker via RegisterActivityWithOptions. Because each Activities instance is
// independent, multiple managers can coexist in the same process without
// sharing mutable state.
type Activities struct {
	updater       task.TaskStatusUpdater
	reportUpdater task.TaskReportUpdater
	registry      *componentmanager.Registry
	dataCipher    *secret.Cipher
}

// New creates an Activities instance. Any argument may be nil; activity
// calls that need a missing dependency return an error at invocation time.
func New(
	updater task.TaskStatusUpdater,
	reportUpdater task.TaskReportUpdater,
	registry *componentmanager.Registry,
	dataCipher *secret.Cipher,
) *Activities {
	return &Activities{
		updater:       updater,
		reportUpdater: reportUpdater,
		registry:      registry,
		dataCipher:    dataCipher,
	}
}

// All returns a map of Temporal activity name to bound method for worker
// registration via RegisterActivityWithOptions. Each entry is a bound method
// that captures this Activities instance, so its dependencies are isolated
// from other Activities instances.
func (a *Activities) All() map[string]any {
	return map[string]any{
		NameInjectExpectation:         a.InjectExpectation,
		NamePowerControl:              a.PowerControl,
		NameGetPowerStatus:            a.GetPowerStatus,
		NameUpdateTaskStatus:          a.UpdateTaskStatus,
		NameUpdateTaskReport:          a.UpdateTaskReport,
		NameFirmwareControl:           a.FirmwareControl,
		NameGetFirmwareStatus:         a.GetFirmwareStatus,
		NameBringUpControl:            a.BringUpControl,
		NameGetBringUpStatus:          a.GetBringUpStatus,
		NameVerifyFirmwareConsistency: a.VerifyFirmwareConsistency,
		NameDecommissionControl:       a.DecommissionControl,
		NameGetDecommissionStatus:     a.GetDecommissionStatus,
	}
}
