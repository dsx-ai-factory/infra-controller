// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
)

const NameResetNVOS = "ResetNVOS"
const NameNVOSResetCompleted = "NVOSResetCompleted"

func (a *Activities) ResetNVOS(ctx context.Context, target common.Target) error {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return err
	}
	return manager.ResetNVOS(ctx, target)
}
func (a *Activities) NVOSResetCompleted(ctx context.Context, target common.Target) (bool, error) {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return false, err
	}
	return manager.NVOSResetCompleted(ctx, target)
}

const NameResetSDN = "ResetSDN"
const NameSDNResetCompleted = "SDNResetCompleted"

func (a *Activities) ResetSDN(ctx context.Context, target common.Target) error {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return err
	}
	return manager.ResetSDN(ctx, target)
}
func (a *Activities) SDNResetCompleted(ctx context.Context, target common.Target) (bool, error) {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return false, err
	}
	return manager.SDNResetCompleted(ctx, target)
}
