// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
)

const NameNVOSResetCompleted = "NVOSResetCompleted"

func (a *Activities) NVOSResetCompleted(ctx context.Context, target common.Target) (bool, error) {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return false, err
	}
	return manager.NVOSResetCompleted(ctx, target)
}

const NameSDNResetCompleted = "SDNResetCompleted"

func (a *Activities) SDNResetCompleted(ctx context.Context, target common.Target) (bool, error) {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return false, err
	}
	return manager.SDNResetCompleted(ctx, target)
}

const NamePrepareFactoryReset = "PrepareFactoryReset"
const NameSubmitFactoryReset = "SubmitFactoryReset"

func (a *Activities) PrepareFactoryReset(ctx context.Context, target common.Target) error {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return err
	}
	return manager.PrepareFactoryReset(ctx, target)
}

func (a *Activities) SubmitFactoryReset(ctx context.Context, target common.Target, scope string) error {
	manager, err := requireCapableManager[componentmanager.ComponentResetter](a.registry, target, capability.CapabilityComponentReset)
	if err != nil {
		return err
	}
	return manager.SubmitFactoryReset(ctx, target, scope)
}
