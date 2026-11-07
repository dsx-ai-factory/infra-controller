// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// SummaryFromWrapper returns a short English phrase describing the operation
// and its key parameters (e.g. "Power Off (forced)", "Upgrade Firmware to v2.1.0").
func SummaryFromWrapper(w operation.Wrapper) (string, error) {
	switch w.Type {
	case taskcommon.TaskTypePowerControl:
		switch w.Code {
		case taskcommon.OpCodePowerControlPowerOn, taskcommon.OpCodePowerControlForcePowerOn:
			return "Power On", nil
		case taskcommon.OpCodePowerControlPowerOff:
			return "Power Off", nil
		case taskcommon.OpCodePowerControlForcePowerOff:
			return "Power Off (forced)", nil
		case taskcommon.OpCodePowerControlRestart, taskcommon.OpCodePowerControlWarmReset:
			return "Power Reset", nil
		case taskcommon.OpCodePowerControlForceRestart, taskcommon.OpCodePowerControlColdReset:
			return "Power Reset (forced)", nil
		default:
			if w.Code != "" {
				return humanizeCode(w.Code), nil
			}
			return "Power Control", nil
		}

	case taskcommon.TaskTypeBringUp:
		if w.Code == taskcommon.OpCodeIngest {
			return "Ingest", nil
		}
		return "Bring Up", nil

	case taskcommon.TaskTypeFirmwareControl:
		var info struct {
			TargetVersion string `json:"target_version"`
		}
		if len(w.Info) > 0 {
			err := json.Unmarshal(w.Info, &info)
			if err != nil {
				return "", fmt.Errorf("unmarshal firmware info: %w", err)
			}
		}
		switch w.Code {
		case taskcommon.OpCodeFirmwareControlUpgrade:
			if info.TargetVersion != "" {
				return "Upgrade Firmware to " + info.TargetVersion, nil
			}
			return "Upgrade Firmware", nil
		case taskcommon.OpCodeFirmwareControlDowngrade:
			if info.TargetVersion != "" {
				return "Downgrade Firmware to " + info.TargetVersion, nil
			}
			return "Downgrade Firmware", nil
		case taskcommon.OpCodeFirmwareControlRollback:
			return "Rollback Firmware", nil
		default:
			if w.Code != "" {
				return humanizeCode(w.Code), nil
			}
			return "Firmware Control", nil
		}

	case taskcommon.TaskTypeInjectExpectation:
		return "Inject Expectation", nil

	default:
		if w.Code != "" {
			return humanizeCode(w.Code), nil
		}
		return string(w.Type), nil
	}
}

// OperationTypeFromWrapper returns a stable SCREAMING_SNAKE_CASE operation type
// string suitable for schedule filtering (e.g. "POWER_ON", "BRING_UP").
func OperationTypeFromWrapper(w operation.Wrapper) string {
	switch w.Type {
	case taskcommon.TaskTypePowerControl:
		switch w.Code {
		case taskcommon.OpCodePowerControlPowerOn, taskcommon.OpCodePowerControlForcePowerOn:
			return "POWER_ON"
		case taskcommon.OpCodePowerControlPowerOff, taskcommon.OpCodePowerControlForcePowerOff:
			return "POWER_OFF"
		case taskcommon.OpCodePowerControlRestart, taskcommon.OpCodePowerControlWarmReset,
			taskcommon.OpCodePowerControlForceRestart, taskcommon.OpCodePowerControlColdReset:
			return "POWER_RESET"
		default:
			return "POWER_CONTROL"
		}
	case taskcommon.TaskTypeBringUp:
		if w.Code == taskcommon.OpCodeIngest {
			return "INGEST"
		}
		return "BRING_UP"
	case taskcommon.TaskTypeFirmwareControl:
		switch w.Code {
		case taskcommon.OpCodeFirmwareControlUpgrade:
			return "UPGRADE_FIRMWARE"
		case taskcommon.OpCodeFirmwareControlDowngrade:
			return "DOWNGRADE_FIRMWARE"
		case taskcommon.OpCodeFirmwareControlRollback:
			return "ROLLBACK_FIRMWARE"
		default:
			return "FIRMWARE_CONTROL"
		}
	default:
		return strings.ToUpper(string(w.Type))
	}
}

func humanizeCode(code string) string {
	return strings.ReplaceAll(strings.ReplaceAll(code, "_", " "), "-", " ")
}
