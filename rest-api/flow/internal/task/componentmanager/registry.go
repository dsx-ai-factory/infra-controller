// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package componentmanager

import (
	"slices"
	"sync"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	cmconfig "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/config"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Registry maintains the active component managers selected from factory specs.
type Registry struct {
	// active is protected by mu because remote managers may be registered at
	// runtime after the initial config-based registry construction.
	mu     sync.RWMutex
	active map[devicetypes.ComponentType]ComponentManager
}

// NewRegistry creates and initializes a Registry from the supplied factory
// specs and component manager configuration.
func NewRegistry(
	factorySpecs []FactorySpec,
	config cmconfig.Config,
	providers *providerapi.ProviderRegistry,
) (*Registry, error) {
	descriptors, factories, err := selectFactorySpecs(
		factorySpecs,
		config.ComponentManagers,
	)
	if err != nil {
		return nil, err
	}

	registry := Registry{
		active: make(
			map[devicetypes.ComponentType]ComponentManager,
			len(config.ComponentManagers),
		),
	}

	for _, descriptor := range descriptors {
		factory, ok := factories[descriptorKeyOf(descriptor)]
		if !ok {
			return nil, ComponentManagerFactoryNotRegisteredError{
				ComponentType: descriptor.Type,
			}
		}

		manager, err := createManager(descriptor, factory, providers)
		if err != nil {
			return nil, err
		}

		registry.active[descriptor.Type] = manager
	}

	return &registry, nil
}

func createManager(
	descriptor cmcatalog.Descriptor,
	factory ManagerFactory,
	providers *providerapi.ProviderRegistry,
) (ComponentManager, error) {
	manager, err := factory(providers)
	if err != nil {
		return nil, ManagerCreationError{
			ComponentType:  descriptor.Type,
			Implementation: descriptor.Implementation,
			Err:            err,
		}
	}
	if manager == nil {
		return nil, ManagerNotCreatedError{
			ComponentType:  descriptor.Type,
			Implementation: descriptor.Implementation,
		}
	}

	// Normalize once at the activation boundary. After this point registry read
	// paths trust the active manager descriptor and only clone it before
	// returning mutable fields.
	managerDescriptor, err := manager.Descriptor().Normalize()
	if err != nil {
		return nil, err
	}

	if !descriptor.Equal(managerDescriptor) {
		return nil, ManagerDescriptorMismatchError{
			Expected: descriptor,
			Actual:   managerDescriptor,
		}
	}

	return manager, nil
}

// FindManager returns the active manager for the specified component type.
// It returns nil when the registry is nil or when no manager is active for the
// type. Use GetManager when the caller needs a descriptive configuration error.
func (r *Registry) FindManager(
	componentType devicetypes.ComponentType,
) ComponentManager {
	if r == nil {
		return nil
	}

	r.mu.RLock()
	defer r.mu.RUnlock()
	return r.active[componentType]
}

// GetManager returns the active manager for the specified component type.
// It returns a descriptive error when the registry is nil or when no manager is
// active for the type.
func (r *Registry) GetManager(
	componentType devicetypes.ComponentType,
) (ComponentManager, error) {
	if r == nil {
		return nil, ErrRegistryNotConfigured
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	manager := r.active[componentType]
	if manager == nil {
		return nil, ManagerNotConfiguredError{ComponentType: componentType}
	}

	return manager, nil
}

// GetCapableManager returns the active manager for componentType after
// verifying its descriptor declares capability.
func (r *Registry) GetCapableManager(
	componentType devicetypes.ComponentType,
	requiredCapability capability.Capability,
) (ComponentManager, error) {
	if r == nil {
		return nil, ErrRegistryNotConfigured
	}

	requiredCapability, err := capability.Parse(requiredCapability.String())
	if err != nil {
		return nil, err
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	manager := r.active[componentType]
	if manager == nil {
		return nil, ManagerNotConfiguredError{ComponentType: componentType}
	}

	descriptor := manager.Descriptor()

	if !descriptor.Capabilities.Contains(requiredCapability) {
		return nil, UnsupportedCapabilityError{
			ComponentType:  descriptor.Type,
			Implementation: descriptor.Implementation,
			Capability:     requiredCapability,
			Available:      descriptor.Capabilities.Clone(),
		}
	}

	return manager, nil
}

// CheckCapability verifies that the active manager for componentType declares
// requiredCapability.
func (r *Registry) CheckCapability(
	componentType devicetypes.ComponentType,
	requiredCapability capability.Capability,
) error {
	_, err := r.GetCapableManager(componentType, requiredCapability)
	return err
}

// GetDescriptor returns the descriptor reported by the active manager for the
// specified component type.
func (r *Registry) GetDescriptor(
	componentType devicetypes.ComponentType,
) (cmcatalog.Descriptor, error) {
	if r == nil {
		return cmcatalog.Descriptor{}, ErrRegistryNotConfigured
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	manager := r.active[componentType]
	if manager == nil {
		return cmcatalog.Descriptor{}, ManagerNotConfiguredError{ComponentType: componentType}
	}

	return manager.Descriptor().Clone(), nil
}

// ComponentTypes returns the component types with active managers, sorted by
// component type.
func (r *Registry) ComponentTypes() []devicetypes.ComponentType {
	if r == nil {
		return nil
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	return r.componentTypesLocked()
}

// Descriptors returns descriptor copies for all active managers, sorted by
// component type.
func (r *Registry) Descriptors() ([]cmcatalog.Descriptor, error) {
	if r == nil {
		return nil, ErrRegistryNotConfigured
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	componentTypes := r.componentTypesLocked()
	descriptors := make([]cmcatalog.Descriptor, 0, len(componentTypes))
	for _, componentType := range componentTypes {
		descriptors = append(
			descriptors,
			r.active[componentType].Descriptor().Clone(),
		)
	}

	return descriptors, nil
}

// ComponentManagers returns all active managers, sorted by component type.
func (r *Registry) ComponentManagers() []ComponentManager {
	if r == nil {
		return nil
	}

	r.mu.RLock()
	defer r.mu.RUnlock()

	managers := make([]ComponentManager, 0, len(r.active))
	for _, componentType := range r.componentTypesLocked() {
		managers = append(managers, r.active[componentType])
	}
	return managers
}

func (r *Registry) componentTypesLocked() []devicetypes.ComponentType {
	componentTypes := make([]devicetypes.ComponentType, 0, len(r.active))
	for componentType := range r.active {
		componentTypes = append(componentTypes, componentType)
	}
	slices.Sort(componentTypes)
	return componentTypes
}
