// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package mock

import (
	"context"
	"time"

	"github.com/rs/zerolog/log"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/executor/temporalworkflow/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

const (
	// ImplementationName is the name used to identify mock implementations.
	ImplementationName = "mock"

	// DefaultDelay is the simulated delay for mock operations.
	DefaultDelay = time.Second
)

// Manager is a mock component manager for testing and development.
type Manager struct {
	componentType devicetypes.ComponentType
	delay         time.Duration
}

// New creates a new mock Manager for the specified component type.
func New(componentType devicetypes.ComponentType) *Manager {
	return &Manager{
		componentType: componentType,
		delay:         DefaultDelay,
	}
}

// NewWithDelay creates a new mock Manager with a custom delay.
func NewWithDelay(componentType devicetypes.ComponentType, delay time.Duration) *Manager {
	return &Manager{
		componentType: componentType,
		delay:         delay,
	}
}

// FactoryFor creates a factory function for the specified component type.
func FactoryFor(componentType devicetypes.ComponentType) componentmanager.ManagerFactory {
	return func(providers *providerapi.ProviderRegistry) (componentmanager.ComponentManager, error) {
		return New(componentType), nil
	}
}

// DescriptorFor returns a mock manager descriptor for the specified component
// type.
func DescriptorFor(componentType devicetypes.ComponentType) cmcatalog.Descriptor {
	return cmcatalog.Descriptor{
		DescriptorIdentity: cmcatalog.DescriptorIdentity{
			Type:           componentType,
			Implementation: ImplementationName,
		},
		Capabilities: capability.CapabilitySet{
			capability.CapabilityBringUpControl,
			capability.CapabilityBringUpStatus,
			capability.CapabilityFirmwareControl,
			capability.CapabilityFirmwareStatus,
			capability.CapabilityInjectExpectation,
			capability.CapabilityPowerControl,
			capability.CapabilityPowerStatus,
		},
	}
}

// FactorySpecFor returns a mock manager runtime factory spec for the specified
// component type.
func FactorySpecFor(componentType devicetypes.ComponentType) componentmanager.FactorySpec {
	return componentmanager.FactorySpec{
		Descriptor: DescriptorFor(componentType),
		Factory:    FactoryFor(componentType),
	}
}

// Descriptors returns mock descriptors for all component types currently
// supported by the Flow service.
func Descriptors() []cmcatalog.Descriptor {
	descriptors := make([]cmcatalog.Descriptor, 0, 3)
	for _, ct := range []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypeNVSwitch,
		devicetypes.ComponentTypePowerShelf,
	} {
		descriptors = append(descriptors, DescriptorFor(ct))
	}
	return descriptors
}

// FactorySpecs returns mock runtime factory specs for all component types
// currently supported by the Flow service.
func FactorySpecs() []componentmanager.FactorySpec {
	factorySpecs := make([]componentmanager.FactorySpec, 0, 3)
	for _, ct := range []devicetypes.ComponentType{
		devicetypes.ComponentTypeCompute,
		devicetypes.ComponentTypeNVSwitch,
		devicetypes.ComponentTypePowerShelf,
	} {
		factorySpecs = append(factorySpecs, FactorySpecFor(ct))
	}
	return factorySpecs
}

// Descriptor returns the mock manager descriptor.
func (m *Manager) Descriptor() cmcatalog.Descriptor {
	return DescriptorFor(m.componentType)
}

// InjectExpectation simulates injecting expected configuration.
func (m *Manager) InjectExpectation(
	ctx context.Context,
	target common.Target,
	info operations.InjectExpectationTaskInfo,
) error {
	log.Debug().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Msg("Mock: InjectExpectation")

	time.Sleep(m.delay)
	return nil
}

// PowerControl simulates power operations.
func (m *Manager) PowerControl(
	ctx context.Context,
	target common.Target,
	info operations.PowerControlTaskInfo,
) error {
	log.Debug().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Str("operation", info.Operation.String()).
		Msg("Mock: PowerControl")

	time.Sleep(m.delay)

	log.Info().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Str("operation", info.Operation.String()).
		Msg("Mock: PowerControl completed")

	return nil
}

// GetPowerStatus simulates getting power status.
func (m *Manager) GetPowerStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.PowerStatus, error) {
	log.Debug().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Msg("Mock: GetPowerStatus")

	time.Sleep(m.delay)

	result := make(map[string]operations.PowerStatus)
	for _, componentID := range target.Identifiers {
		result[componentID] = operations.PowerStatusOn
	}

	log.Info().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Int("component_count", len(result)).
		Msg("Mock: GetPowerStatus completed")

	return result, nil
}

// FirmwareControl simulates initiating firmware update without waiting for completion.
func (m *Manager) FirmwareControl(
	ctx context.Context,
	target common.Target,
	info operations.FirmwareControlTaskInfo,
) error {
	log.Debug().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Str("target_version", info.TargetVersion).
		Msg("Mock: FirmwareControl")

	time.Sleep(m.delay)

	log.Info().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Msg("Mock: FirmwareControl completed")

	return nil
}

// BringUpControl simulates opening the bring-up gate. The info argument is
// accepted to satisfy the BringUpController interface; the mock ignores its
// contents.
func (m *Manager) BringUpControl(
	ctx context.Context,
	target common.Target,
	_ operations.BringUpTaskInfo,
) error {
	log.Debug().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Msg("Mock: BringUpControl")
	time.Sleep(m.delay)
	return nil
}

// GetBringUpStatus simulates getting bring-up status.
func (m *Manager) GetBringUpStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.MachineBringUpState, error) {
	log.Debug().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Msg("Mock: GetBringUpStatus")
	time.Sleep(m.delay)

	result := make(
		map[string]operations.MachineBringUpState,
		len(target.Identifiers),
	)
	for _, id := range target.Identifiers {
		result[id] = operations.MachineBringUpStateMachineCreated
	}
	return result, nil
}

// GetFirmwareStatus simulates getting firmware update status.
func (m *Manager) GetFirmwareStatus(
	ctx context.Context,
	target common.Target,
) (map[string]operations.FirmwareUpdateStatus, error) {
	log.Debug().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Msg("Mock: GetFirmwareStatus")

	time.Sleep(m.delay)

	result := make(map[string]operations.FirmwareUpdateStatus)
	for _, componentID := range target.Identifiers {
		result[componentID] = operations.FirmwareUpdateStatus{
			ComponentID: componentID,
			State:       operations.FirmwareUpdateStateCompleted,
			Error:       "",
		}
	}

	log.Info().
		Str("component_type", m.componentType.String()).
		Str("target", target.String()).
		Int("component_count", len(result)).
		Msg("Mock: GetFirmwareStatus completed")

	return result, nil
}
