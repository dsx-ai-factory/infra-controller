// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package protobuf

import (
	"fmt"
	"slices"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	pb "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/proto/v1"
	"google.golang.org/protobuf/types/known/timestamppb"
)

// TaskOperationFrom converts a reusable typed task operation into its domain
// representation.
func TaskOperationFrom(spec *pb.TaskOperation) (operations.Operation, error) {
	if spec == nil || spec.GetOperation() == nil {
		return nil, fmt.Errorf("task operation is required")
	}

	var operation operations.Operation
	switch typed := spec.GetOperation().(type) {
	case *pb.TaskOperation_PowerControl:
		if typed.PowerControl == nil {
			return nil, fmt.Errorf("power control operation is required")
		}

		powerOperation, err := powerControlOperationFrom(
			typed.PowerControl.GetOperation(),
		)
		if err != nil {
			return nil, err
		}

		operation = &operations.PowerControlTaskInfo{
			Operation:              powerOperation,
			Forced:                 powerOperationIsForced(powerOperation),
			OverrideReadinessCheck: typed.PowerControl.GetOverrideReadinessCheck(),
		}
	case *pb.TaskOperation_FirmwareControl:
		if typed.FirmwareControl == nil {
			return nil, fmt.Errorf("firmware control operation is required")
		}

		firmwareOperation, err := firmwareControlOperationFrom(
			typed.FirmwareControl.GetOperation(),
		)
		if err != nil {
			return nil, err
		}

		startTime, err := pbTimestampUnixSeconds(typed.FirmwareControl.StartTime)
		if err != nil {
			return nil, fmt.Errorf("firmware control start time: %w", err)
		}

		endTime, err := pbTimestampUnixSeconds(typed.FirmwareControl.EndTime)
		if err != nil {
			return nil, fmt.Errorf("firmware control end time: %w", err)
		}

		if startTime != 0 && endTime != 0 && startTime >= endTime {
			return nil, fmt.Errorf("firmware control start time must be before end time")
		}

		operation = &operations.FirmwareControlTaskInfo{
			Operation:              firmwareOperation,
			TargetVersion:          typed.FirmwareControl.GetTargetVersion(),
			StartTime:              startTime,
			EndTime:                endTime,
			SubTargets:             slices.Clone(typed.FirmwareControl.GetSubTargets()),
			OverrideReadinessCheck: typed.FirmwareControl.GetOverrideReadinessCheck(),
		}
	default:
		return nil, fmt.Errorf("unsupported task operation %T", typed)
	}

	if err := operation.Validate(); err != nil {
		return nil, err
	}

	return operation, nil
}

// TaskOperationTo converts a domain task operation into the reusable typed
// protobuf representation.
func TaskOperationTo(operation operations.Operation) (*pb.TaskOperation, error) {
	if operation == nil {
		return nil, fmt.Errorf("task operation is required")
	}
	if err := operation.Validate(); err != nil {
		return nil, err
	}

	switch typed := operation.(type) {
	case *operations.PowerControlTaskInfo:
		powerOperation, err := powerControlOperationTo(typed.Operation)
		if err != nil {
			return nil, err
		}
		return &pb.TaskOperation{
			Operation: &pb.TaskOperation_PowerControl{
				PowerControl: &pb.PowerControlTaskOperation{
					Operation:              powerOperation,
					OverrideReadinessCheck: typed.OverrideReadinessCheck,
				},
			},
		}, nil
	case *operations.FirmwareControlTaskInfo:
		if typed.AuthenticationData != nil || typed.AccessToken != "" {
			return nil, fmt.Errorf("firmware credentials are not supported in task operations")
		}
		firmwareOperation, err := firmwareControlOperationTo(typed.Operation)
		if err != nil {
			return nil, err
		}
		converted := &pb.FirmwareControlTaskOperation{
			Operation:              firmwareOperation,
			SubTargets:             slices.Clone(typed.SubTargets),
			OverrideReadinessCheck: typed.OverrideReadinessCheck,
		}
		if typed.TargetVersion != "" {
			targetVersion := typed.TargetVersion
			converted.TargetVersion = &targetVersion
		}
		if typed.StartTime != 0 {
			startTime := timestamppb.New(time.Unix(typed.StartTime, 0))
			if err := startTime.CheckValid(); err != nil {
				return nil, fmt.Errorf("firmware control start time: %w", err)
			}
			converted.StartTime = startTime
		}
		if typed.EndTime != 0 {
			endTime := timestamppb.New(time.Unix(typed.EndTime, 0))
			if err := endTime.CheckValid(); err != nil {
				return nil, fmt.Errorf("firmware control end time: %w", err)
			}
			converted.EndTime = endTime
		}
		if typed.StartTime != 0 && typed.EndTime != 0 && typed.EndTime <= typed.StartTime {
			return nil, fmt.Errorf("firmware control start time must be before end time")
		}
		return &pb.TaskOperation{
			Operation: &pb.TaskOperation_FirmwareControl{
				FirmwareControl: converted,
			},
		}, nil
	default:
		return nil, fmt.Errorf("unsupported task operation %T", operation)
	}
}

// pbTimestampUnixSeconds converts an optional protobuf timestamp into the
// second-granularity representation used by domain operations. Fractional
// seconds are intentionally discarded. Zero is the domain representation's
// absence sentinel, so a supplied value cannot convert to Unix second zero.
func pbTimestampUnixSeconds(value *timestamppb.Timestamp) (int64, error) {
	if value == nil {
		return 0, nil
	}

	if err := value.CheckValid(); err != nil {
		return 0, err
	}

	seconds := value.AsTime().Unix()
	if seconds == 0 {
		return 0, fmt.Errorf("timestamp cannot convert to Unix second zero because zero represents omission")
	}

	return seconds, nil
}

func powerControlOperationFrom(
	operation pb.PowerControlOperation,
) (operations.PowerOperation, error) {
	switch operation {
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON:
		return operations.PowerOperationPowerOn, nil
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_ON:
		return operations.PowerOperationForcePowerOn, nil
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_OFF:
		return operations.PowerOperationPowerOff, nil
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF:
		return operations.PowerOperationForcePowerOff, nil
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_RESTART:
		return operations.PowerOperationRestart, nil
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_RESTART:
		return operations.PowerOperationForceRestart, nil
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_WARM_RESET:
		return operations.PowerOperationWarmReset, nil
	case pb.PowerControlOperation_POWER_CONTROL_OPERATION_COLD_RESET:
		return operations.PowerOperationColdReset, nil
	default:
		return operations.PowerOperationUnknown, fmt.Errorf(
			"unsupported power control operation %q",
			operation,
		)
	}
}

func powerControlOperationTo(
	operation operations.PowerOperation,
) (pb.PowerControlOperation, error) {
	switch operation {
	case operations.PowerOperationPowerOn:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_ON, nil
	case operations.PowerOperationForcePowerOn:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_ON, nil
	case operations.PowerOperationPowerOff:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_POWER_OFF, nil
	case operations.PowerOperationForcePowerOff:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF, nil
	case operations.PowerOperationRestart:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_RESTART, nil
	case operations.PowerOperationForceRestart:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_RESTART, nil
	case operations.PowerOperationWarmReset:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_WARM_RESET, nil
	case operations.PowerOperationColdReset:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_COLD_RESET, nil
	default:
		return pb.PowerControlOperation_POWER_CONTROL_OPERATION_UNSPECIFIED, fmt.Errorf(
			"unsupported power control operation %q",
			operation,
		)
	}
}

func firmwareControlOperationFrom(
	operation pb.FirmwareControlOperation,
) (operations.FirmwareOperation, error) {
	switch operation {
	case pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE:
		return operations.FirmwareOperationUpgrade, nil
	case pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_DOWNGRADE:
		return operations.FirmwareOperationDowngrade, nil
	case pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_ROLLBACK:
		return operations.FirmwareOperationRollback, nil
	default:
		return operations.FirmwareOperationUnknown, fmt.Errorf(
			"unsupported firmware control operation %q",
			operation,
		)
	}
}

func firmwareControlOperationTo(
	operation operations.FirmwareOperation,
) (pb.FirmwareControlOperation, error) {
	switch operation {
	case operations.FirmwareOperationUpgrade:
		return pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UPGRADE, nil
	case operations.FirmwareOperationDowngrade:
		return pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_DOWNGRADE, nil
	case operations.FirmwareOperationRollback:
		return pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_ROLLBACK, nil
	default:
		return pb.FirmwareControlOperation_FIRMWARE_CONTROL_OPERATION_UNSPECIFIED, fmt.Errorf(
			"unsupported firmware control operation %q",
			operation,
		)
	}
}

func powerOperationIsForced(operation operations.PowerOperation) bool {
	switch operation {
	case operations.PowerOperationForcePowerOn,
		operations.PowerOperationForcePowerOff,
		operations.PowerOperationForceRestart:
		return true
	default:
		return false
	}
}
