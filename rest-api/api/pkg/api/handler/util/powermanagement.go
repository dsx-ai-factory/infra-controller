// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strings"

	"github.com/google/uuid"

	dpsclient "github.com/NVIDIA/infra-controller/rest-api/api/pkg/client/dps"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

// MachinePowerAssignment describes one machine's DPS policy assignment.
type MachinePowerAssignment struct {
	MachineID    string
	PowerProfile string
}

// PowerChange contains the compensation and completion actions for a prepared
// DPS resource-group change.
type PowerChange struct {
	rollback func() error
	complete func() error
}

// Rollback compensates a prepared DPS resource-group change.
func (change *PowerChange) Rollback() error {
	if change == nil || change.rollback == nil {
		return nil
	}
	return change.rollback()
}

// Complete performs cleanup after a prepared DPS resource-group change commits.
func (change *PowerChange) Complete() error {
	if change == nil || change.complete == nil {
		return nil
	}
	return change.complete()
}

// AcquireVPCPowerLock serializes DPS operations for one VPC transaction.
func AcquireVPCPowerLock(ctx context.Context, tx *cdb.Tx, vpcID uuid.UUID) error {
	return tx.TryAcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString("dps-vpc:"+vpcID.String()), nil)
}

// ProvisionMachinePower authorizes and assigns one machine to a DPS resource group.
func ProvisionMachinePower(ctx context.Context, provisioner dpsclient.PowerProvisioner, resourceGroup string, assignment MachinePowerAssignment) (func() error, error) {
	if provisioner == nil {
		return nil, fmt.Errorf("DPS power provisioner is unavailable")
	}
	resourceGroup = strings.TrimSpace(resourceGroup)
	if resourceGroup == "" {
		return nil, fmt.Errorf("VPC power resource group is required")
	}
	assignment.MachineID = strings.TrimSpace(assignment.MachineID)
	assignment.PowerProfile = strings.TrimSpace(assignment.PowerProfile)

	if assignment.PowerProfile != "" {
		err := provisioner.ValidateAllocation(ctx, []string{assignment.MachineID}, assignment.PowerProfile)
		if err != nil {
			return nil, fmt.Errorf("validate DPS allocation: %w", err)
		}
	}
	err := provisioner.AddMachine(ctx, resourceGroup, assignment.MachineID, assignment.PowerProfile)
	if err != nil {
		return nil, fmt.Errorf("add machine to DPS resource group: %w", err)
	}
	err = provisioner.ActivateResourceGroup(ctx, resourceGroup)
	if err != nil {
		cleanupErr := provisioner.RemoveMachine(context.WithoutCancel(ctx), resourceGroup, assignment.MachineID)
		return nil, errors.Join(fmt.Errorf("activate DPS resource group: %w", err), cleanupErr)
	}

	return func() error {
		return provisioner.RemoveMachine(context.WithoutCancel(ctx), resourceGroup, assignment.MachineID)
	}, nil
}

// ProvisionMachineBatchPower authorizes and assigns machines to a DPS resource group.
func ProvisionMachineBatchPower(ctx context.Context, provisioner dpsclient.PowerProvisioner, resourceGroup string, assignments []MachinePowerAssignment) (func() error, error) {
	if provisioner == nil {
		return nil, fmt.Errorf("DPS power provisioner is unavailable")
	}
	resourceGroup = strings.TrimSpace(resourceGroup)
	if resourceGroup == "" {
		return nil, fmt.Errorf("VPC power resource group is required")
	}
	if len(assignments) == 0 {
		return nil, nil
	}

	machineIDs := make([]string, 0, len(assignments))
	powerProfile := strings.TrimSpace(assignments[0].PowerProfile)
	for i := range assignments {
		assignments[i].MachineID = strings.TrimSpace(assignments[i].MachineID)
		assignments[i].PowerProfile = strings.TrimSpace(assignments[i].PowerProfile)
		if assignments[i].PowerProfile != powerProfile {
			return nil, fmt.Errorf("DPS batch assignments must use one power profile")
		}
		machineIDs = append(machineIDs, assignments[i].MachineID)
	}
	if powerProfile != "" {
		err := provisioner.ValidateAllocation(ctx, machineIDs, powerProfile)
		if err != nil {
			return nil, fmt.Errorf("validate DPS batch allocation: %w", err)
		}
	}

	added := false
	rollback := func() error {
		if !added {
			return nil
		}
		return provisioner.RemoveMachines(context.WithoutCancel(ctx), resourceGroup, machineIDs)
	}
	err := provisioner.AddMachines(ctx, resourceGroup, machineIDs, powerProfile)
	if err != nil {
		cleanupErr := provisioner.RemoveMachines(context.WithoutCancel(ctx), resourceGroup, machineIDs)
		return nil, errors.Join(fmt.Errorf("add machines to DPS resource group: %w", err), cleanupErr)
	}
	added = true
	err = provisioner.ActivateResourceGroup(ctx, resourceGroup)
	if err != nil {
		return nil, errors.Join(fmt.Errorf("activate DPS resource group: %w", err), rollback())
	}
	return rollback, nil
}

// UpdateMachinePower authorizes and updates one machine's DPS power profile.
func UpdateMachinePower(ctx context.Context, provisioner dpsclient.PowerProvisioner, resourceGroup string, assignment MachinePowerAssignment, previousProfile string) (func() error, error) {
	if provisioner == nil {
		return nil, fmt.Errorf("DPS power provisioner is unavailable")
	}
	resourceGroup = strings.TrimSpace(resourceGroup)
	if resourceGroup == "" {
		return nil, fmt.Errorf("VPC power resource group is required")
	}
	assignment.MachineID = strings.TrimSpace(assignment.MachineID)
	assignment.PowerProfile = strings.TrimSpace(assignment.PowerProfile)
	previousProfile = strings.TrimSpace(previousProfile)

	if assignment.PowerProfile != "" {
		err := provisioner.ValidateAllocation(ctx, []string{assignment.MachineID}, assignment.PowerProfile)
		if err != nil {
			return nil, fmt.Errorf("validate DPS allocation: %w", err)
		}
	}
	err := provisioner.UpdateMachineProfile(ctx, resourceGroup, assignment.MachineID, assignment.PowerProfile)
	if err != nil {
		return nil, fmt.Errorf("update DPS machine profile: %w", err)
	}

	return func() error {
		return provisioner.UpdateMachineProfile(context.WithoutCancel(ctx), resourceGroup, assignment.MachineID, previousProfile)
	}, nil
}

// PreparePowerResourceGroupChange prepares a VPC's DPS resource-group migration.
func PreparePowerResourceGroupChange(ctx context.Context, provisioner dpsclient.PowerProvisioner, externalID int64, oldGroup, newGroup string, assignments []MachinePowerAssignment) (*PowerChange, error) {
	if provisioner == nil {
		return nil, fmt.Errorf("DPS power provisioner is unavailable")
	}
	oldGroup = strings.TrimSpace(oldGroup)
	newGroup = strings.TrimSpace(newGroup)
	if oldGroup == newGroup {
		return &PowerChange{}, nil
	}

	profiles := make(map[string][]string)
	for i := range assignments {
		assignments[i].MachineID = strings.TrimSpace(assignments[i].MachineID)
		assignments[i].PowerProfile = strings.TrimSpace(assignments[i].PowerProfile)
		if newGroup != "" && assignments[i].PowerProfile != "" {
			profiles[assignments[i].PowerProfile] = append(profiles[assignments[i].PowerProfile], assignments[i].MachineID)
		}
	}
	profileNames := make([]string, 0, len(profiles))
	for profile := range profiles {
		profileNames = append(profileNames, profile)
	}
	slices.Sort(profileNames)
	for _, profile := range profileNames {
		machineIDs := profiles[profile]
		err := provisioner.ValidateAllocation(ctx, machineIDs, profile)
		if err != nil {
			return nil, fmt.Errorf("validate DPS resource-group migration: %w", err)
		}
	}

	createdNewGroup := false
	if newGroup != "" {
		err := provisioner.CreateResourceGroup(ctx, newGroup, externalID)
		if err != nil {
			return nil, fmt.Errorf("create replacement DPS resource group: %w", err)
		}
		createdNewGroup = true
	}

	moved := make([]MachinePowerAssignment, 0, len(assignments))
	restoredToOldGroup := false
	rollback := func() error {
		var rollbackErr error
		rollbackCtx := context.WithoutCancel(ctx)
		for _, assignment := range slices.Backward(moved) {
			if newGroup != "" {
				rollbackErr = errors.Join(rollbackErr, provisioner.RemoveMachine(rollbackCtx, newGroup, assignment.MachineID))
			}
			if oldGroup != "" {
				rollbackErr = errors.Join(rollbackErr, provisioner.AddMachine(rollbackCtx, oldGroup, assignment.MachineID, assignment.PowerProfile))
			}
		}
		if oldGroup != "" && (len(moved) > 0 || restoredToOldGroup) {
			rollbackErr = errors.Join(rollbackErr, provisioner.ActivateResourceGroup(rollbackCtx, oldGroup))
		}
		if createdNewGroup {
			rollbackErr = errors.Join(rollbackErr, provisioner.DeleteResourceGroup(rollbackCtx, newGroup))
		}
		return rollbackErr
	}

	for _, assignment := range assignments {
		if oldGroup != "" {
			err := provisioner.RemoveMachine(ctx, oldGroup, assignment.MachineID)
			if err != nil {
				return nil, errors.Join(fmt.Errorf("remove machine from previous DPS resource group: %w", err), rollback())
			}
		}
		if newGroup != "" {
			err := provisioner.AddMachine(ctx, newGroup, assignment.MachineID, assignment.PowerProfile)
			if err != nil {
				var restoreErr error
				if oldGroup != "" {
					restoreErr = provisioner.AddMachine(context.WithoutCancel(ctx), oldGroup, assignment.MachineID, assignment.PowerProfile)
					restoredToOldGroup = restoreErr == nil
				}
				return nil, errors.Join(fmt.Errorf("add machine to replacement DPS resource group: %w", err), restoreErr, rollback())
			}
		}
		moved = append(moved, assignment)
	}

	if newGroup != "" && len(assignments) > 0 {
		err := provisioner.ActivateResourceGroup(ctx, newGroup)
		if err != nil {
			return nil, errors.Join(fmt.Errorf("activate replacement DPS resource group: %w", err), rollback())
		}
	}

	complete := func() error {
		if oldGroup == "" {
			return nil
		}
		return provisioner.DeleteResourceGroup(context.WithoutCancel(ctx), oldGroup)
	}
	return &PowerChange{rollback: rollback, complete: complete}, nil
}
