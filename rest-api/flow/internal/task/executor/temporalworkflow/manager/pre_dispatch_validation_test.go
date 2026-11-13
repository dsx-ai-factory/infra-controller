// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package manager

import (
	"context"
	"encoding/json"
	"errors"
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/task"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

func TestValidateRequiredCapabilitiesRejectsUnsupportedCapability(t *testing.T) {
	registry := newPreflightCapabilityRegistry(
		t,
		capability.CapabilityPowerStatus,
	)

	err := validateRequiredCapabilities(
		powerControlExecutionInfo(t),
		registry,
	)

	require.Error(t, err)
	require.True(t, errors.Is(err, componentmanager.ErrUnsupportedCapability))

	var capabilityErr componentmanager.UnsupportedCapabilityError
	require.True(t, errors.As(err, &capabilityErr))
	require.Equal(t, devicetypes.ComponentTypeCompute, capabilityErr.ComponentType)
	require.Equal(t, capability.CapabilityPowerControl, capabilityErr.Capability)
}

func TestExecuteValidatesCapabilitiesBeforeDispatch(t *testing.T) {
	registry := newPreflightCapabilityRegistry(
		t,
		capability.CapabilityPowerStatus,
	)
	manager := &Manager{
		conf: Config{
			ComponentManagerRegistry: registry,
		},
	}

	resp, err := manager.Execute(
		context.Background(),
		&task.ExecutionRequest{
			Info:  powerControlExecutionInfo(t),
			Async: true,
		},
	)

	require.Nil(t, resp)
	require.Error(t, err)
	require.True(t, errors.Is(err, componentmanager.ErrUnsupportedCapability))
}

func TestValidateRejectsUnsupportedFirmwareOptionsBeforeDispatch(t *testing.T) {
	validationErr := errors.New("firmware version-check override is not supported")
	registry := newPreflightFirmwareRegistry(t, validationErr)
	manager := &Manager{conf: Config{ComponentManagerRegistry: registry}}

	err := manager.Validate(&task.ExecutionRequest{
		Info:  firmwareControlExecutionInfo(t),
		Async: true,
	})

	require.ErrorIs(t, err, validationErr)
}

func TestExecutionComponentTypesDeduplicatesAndSorts(t *testing.T) {
	got := executionComponentTypes([]task.WorkflowComponent{
		{Type: devicetypes.ComponentTypePowerShelf},
		{Type: devicetypes.ComponentTypeCompute},
		{Type: devicetypes.ComponentTypePowerShelf},
		{Type: devicetypes.ComponentTypeNVSwitch},
		{Type: devicetypes.ComponentTypeCompute},
	})

	require.Equal(
		t,
		[]devicetypes.ComponentType{
			devicetypes.ComponentTypeCompute,
			devicetypes.ComponentTypeNVSwitch,
			devicetypes.ComponentTypePowerShelf,
		},
		got,
	)
}

func powerControlExecutionInfo(t *testing.T) task.ExecutionInfo {
	t.Helper()

	data, err := json.Marshal(operations.PowerControlTaskInfo{
		Operation: operations.PowerOperationPowerOn,
	})
	require.NoError(t, err)

	return task.ExecutionInfo{
		TaskID: uuid.New(),
		Components: []task.WorkflowComponent{
			{Type: devicetypes.ComponentTypeCompute, ComponentID: "machine-1"},
		},
		RuleDefinition: &operationrules.RuleDefinition{
			Steps: []operationrules.SequenceStep{
				{
					ComponentType: devicetypes.ComponentTypeCompute,
					MainOperation: operationrules.ActionConfig{
						Name: operationrules.ActionPowerControl,
					},
				},
			},
		},
		OperationType: taskcommon.TaskTypePowerControl,
		OperationInfo: data,
	}
}

type preflightCapabilityManager struct {
	descriptor            cmcatalog.Descriptor
	firmwareValidationErr error
}

func (m preflightCapabilityManager) Descriptor() cmcatalog.Descriptor {
	return m.descriptor
}

func (m preflightCapabilityManager) ValidateFirmwareControl(
	operations.FirmwareControlTaskInfo,
) error {
	return m.firmwareValidationErr
}

func firmwareControlExecutionInfo(t *testing.T) task.ExecutionInfo {
	t.Helper()

	data, err := json.Marshal(operations.FirmwareControlTaskInfo{
		Operation:            operations.FirmwareOperationUpgrade,
		OverrideVersionCheck: true,
	})
	require.NoError(t, err)

	return task.ExecutionInfo{
		TaskID: uuid.New(),
		Components: []task.WorkflowComponent{
			{Type: devicetypes.ComponentTypeCompute, ComponentID: "machine-1"},
		},
		RuleDefinition: &operationrules.RuleDefinition{
			Steps: []operationrules.SequenceStep{{
				ComponentType: devicetypes.ComponentTypeCompute,
				MainOperation: operationrules.ActionConfig{
					Name: operationrules.ActionFirmwareControl,
				},
			}},
		},
		OperationType: taskcommon.TaskTypeFirmwareControl,
		OperationInfo: data,
	}
}

func newPreflightFirmwareRegistry(
	t *testing.T,
	validationErr error,
) *componentmanager.Registry {
	t.Helper()

	descriptor := cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "limited-firmware",
		},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityFirmwareControl,
			capability.CapabilityFirmwareStatus,
		},
	}
	registry, err := componentmanager.NewRegistry(
		[]componentmanager.FactorySpec{{
			Descriptor: descriptor,
			Factory: func(*providerapi.ProviderRegistry) (componentmanager.ComponentManager, error) {
				return preflightCapabilityManager{
					descriptor:            descriptor,
					firmwareValidationErr: validationErr,
				}, nil
			},
		}},
		cmconfig.Config{ComponentManagers: map[devicetypes.ComponentType]string{
			devicetypes.ComponentTypeCompute: "limited-firmware",
		}},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)
	return registry
}

func newPreflightCapabilityRegistry(
	t *testing.T,
	capabilities ...capability.Capability,
) *componentmanager.Registry {
	t.Helper()

	descriptor := cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           devicetypes.ComponentTypeCompute,
			Implementation: "limited",
		},
		Capabilities: capability.CapabilitySet(capabilities),
	}

	registry, err := componentmanager.NewRegistry(
		[]componentmanager.FactorySpec{
			{
				Descriptor: descriptor,
				Factory: func(*providerapi.ProviderRegistry) (componentmanager.ComponentManager, error) {
					return preflightCapabilityManager{descriptor: descriptor}, nil
				},
			},
		},
		cmconfig.Config{
			ComponentManagers: map[devicetypes.ComponentType]string{
				devicetypes.ComponentTypeCompute: "limited",
			},
		},
		providerapi.NewProviderRegistry(),
	)
	require.NoError(t, err)

	return registry
}

func TestValidateFirmwareSelection(t *testing.T) {
	compute, sw, ps := devicetypes.ComponentTypeCompute, devicetypes.ComponentTypeNVSwitch, devicetypes.ComponentTypePowerShelf
	cases := []struct {
		name          string
		targets       []string
		rejectCompute bool
		want          []devicetypes.ComponentType
		wantSubsets   map[devicetypes.ComponentType][]string
		wantError     bool
	}{
		{name: "mixed names validate only each tray subset", targets: []string{"bmc", "nvos"}, want: []devicetypes.ComponentType{compute, sw}, wantSubsets: map[devicetypes.ComponentType][]string{compute: {"bmc"}, sw: {"bmc", "nvos"}}},

		{name: "bios skips unconfigured powershelf", targets: []string{"bios"}, want: []devicetypes.ComponentType{compute, sw}},
		{name: "nvos skips compute validator", targets: []string{"nvos"}, rejectCompute: true, want: []devicetypes.ComponentType{sw}},
		{name: "applicable backend rejection is not treated as a skip", targets: []string{"bios"}, rejectCompute: true, wantError: true},
		{name: "unknown name rejects before backend validation", targets: []string{"bios", "typo"}, wantError: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			var calls []devicetypes.ComponentType
			var specs []componentmanager.FactorySpec
			implementations := map[devicetypes.ComponentType]string{}
			for _, typ := range []devicetypes.ComponentType{compute, sw} {
				descriptor := cmcatalog.Descriptor{DescriptorIdentity: cmcatalog.DescriptorIdentity{Type: typ, Implementation: "selection-test"}, Capabilities: capability.CapabilitySet{capability.CapabilityFirmwareControl, capability.CapabilityFirmwareStatus}}
				specs = append(specs, componentmanager.FactorySpec{Descriptor: descriptor, Factory: func(*providerapi.ProviderRegistry) (componentmanager.ComponentManager, error) {
					return selectionValidationManager{descriptor: descriptor, validate: func(info operations.FirmwareControlTaskInfo) error {
						calls = append(calls, typ)
						expected := tc.targets
						if tc.wantSubsets != nil {
							expected = tc.wantSubsets[typ]
						}
						require.Equal(t, expected, info.SubTargets)
						if typ == compute && tc.rejectCompute {
							return errors.New("unsupported compute firmware option")
						}
						return nil
					}}, nil
				}})
				implementations[typ] = "selection-test"
			}
			registry, err := componentmanager.NewRegistry(specs, cmconfig.Config{ComponentManagers: implementations}, providerapi.NewProviderRegistry())
			require.NoError(t, err)
			info := firmwareControlExecutionInfo(t)
			data, err := json.Marshal(operations.FirmwareControlTaskInfo{Operation: operations.FirmwareOperationUpgrade, SubTargets: tc.targets})
			require.NoError(t, err)
			info.OperationInfo = data
			for _, typ := range []devicetypes.ComponentType{sw, ps} {
				info.Components = append(info.Components, task.WorkflowComponent{Type: typ, ComponentID: "device"})
				info.RuleDefinition.Steps = append(info.RuleDefinition.Steps, operationrules.SequenceStep{ComponentType: typ, MainOperation: operationrules.ActionConfig{Name: operationrules.ActionFirmwareControl}})
			}
			manager := &Manager{conf: Config{ComponentManagerRegistry: registry}}
			err = manager.Validate(&task.ExecutionRequest{Info: info, Async: true})
			if tc.wantError {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
				require.Equal(t, tc.want, calls)
			}
			if tc.name == "unknown name rejects before backend validation" {
				require.Empty(t, calls)
			}
			require.Len(t, info.RuleDefinition.Steps, 3)
		})
	}
}

type selectionValidationManager struct {
	descriptor cmcatalog.Descriptor
	validate   func(operations.FirmwareControlTaskInfo) error
}

func (m selectionValidationManager) Descriptor() cmcatalog.Descriptor { return m.descriptor }
func (m selectionValidationManager) ValidateFirmwareControl(info operations.FirmwareControlTaskInfo) error {
	return m.validate(info)
}
