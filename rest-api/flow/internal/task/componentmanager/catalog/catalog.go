// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package catalog

import (
	"cmp"
	"slices"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/capability"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Descriptor describes a component manager implementation registered in this
// process. DescriptorIdentity is Type plus Implementation; provider names stay
// separate because one manager can require multiple providers and one provider
// can serve multiple component manager implementations. Capabilities describe
// the operations this manager supports and are used to validate that active
// managers can execute a task before it is dispatched.
type Descriptor struct {
	DescriptorIdentity
	RequiredProviders []string
	Capabilities      capability.CapabilitySet
}

// DescriptorIdentity identifies a component manager implementation. It is the
// stable key used by catalogs, factory specs, and manager-specific config.
type DescriptorIdentity struct {
	Type           devicetypes.ComponentType
	Implementation string
}

// String returns the stable component-type/implementation form used in logs and
// error messages.
func (i DescriptorIdentity) String() string {
	return devicetypes.ComponentTypeToString(i.Type) + "/" + i.Implementation
}

// Normalize validates an identity and returns its normalized value.
func (i DescriptorIdentity) Normalize() (DescriptorIdentity, error) {
	if i.Type == devicetypes.ComponentTypeUnknown {
		return DescriptorIdentity{}, UnknownComponentTypeError{
			Name: devicetypes.ComponentTypeToString(i.Type),
		}
	}

	i.Implementation = strings.TrimSpace(i.Implementation)
	if i.Implementation == "" {
		return DescriptorIdentity{}, ComponentManagerImplementationNameEmptyError{
			ComponentType: i.Type,
		}
	}

	return i, nil
}

func compareDescriptorIdentities(a DescriptorIdentity, b DescriptorIdentity) int {
	if n := cmp.Compare(a.Type, b.Type); n != 0 {
		return n
	}
	return cmp.Compare(a.Implementation, b.Implementation)
}

// SortDescriptorIdentities sorts descriptor identities by component type and
// then implementation name.
func SortDescriptorIdentities(identities []DescriptorIdentity) {
	slices.SortFunc(identities, compareDescriptorIdentities)
}

// Catalog contains the component manager implementations supported by a
// particular binary. Service-specific packages such as builtin own the list of
// descriptors that goes into a catalog.
type Catalog struct {
	descriptors map[devicetypes.ComponentType]map[string]Descriptor // type -> impl_name -> descriptor
}

// New validates descriptors and indexes them by component type and
// implementation.
func New(descriptors []Descriptor) (Catalog, error) {
	catalog := Catalog{
		descriptors: make(map[devicetypes.ComponentType]map[string]Descriptor),
	}

	for _, descriptor := range descriptors {
		d, err := descriptor.Normalize()
		if err != nil {
			return Catalog{}, err
		}

		if _, ok := catalog.descriptors[d.Type]; !ok {
			catalog.descriptors[d.Type] = make(map[string]Descriptor)
		}

		if _, exists := catalog.descriptors[d.Type][d.Implementation]; exists {
			return Catalog{}, DuplicateDescriptorError{
				ComponentType:  d.Type,
				Implementation: d.Implementation,
			}
		}

		catalog.descriptors[d.Type][d.Implementation] = d
	}

	return catalog, nil
}

// Get returns the descriptor for a normalized descriptor identity. Get does
// not normalize identity; callers should normalize identities built from raw
// input before calling. Passing an unnormalized identity simply misses and
// returns false.
func (c Catalog) Get(identity DescriptorIdentity) (Descriptor, bool) {
	descriptors := c.descriptors[identity.Type]
	if descriptors == nil {
		return Descriptor{}, false
	}

	descriptor, ok := descriptors[identity.Implementation]
	if !ok {
		return Descriptor{}, false
	}

	return descriptor.Clone(), true
}

// Implementations returns the implementations registered for a component type.
func (c Catalog) Implementations(
	componentType devicetypes.ComponentType,
) []string {
	return descriptorImplementationNames(c.descriptors[componentType])
}

// ListImplementations returns all registered implementation names by component
// type.
func (c Catalog) ListImplementations() map[devicetypes.ComponentType][]string {
	result := make(map[devicetypes.ComponentType][]string)
	for componentType, descriptors := range c.descriptors {
		result[componentType] = descriptorImplementationNames(descriptors)
	}
	return result
}

// SelectedDescriptors returns descriptors for the component managers selected
// by config.
func (c Catalog) SelectedDescriptors(
	componentManagers map[devicetypes.ComponentType]string,
) ([]Descriptor, error) {
	descriptors := make([]Descriptor, 0, len(componentManagers))
	for componentType, implName := range componentManagers {
		descriptor, ok := c.Get(
			DescriptorIdentity{
				Type:           componentType,
				Implementation: implName,
			},
		)
		if !ok {
			available := c.Implementations(componentType)
			if len(available) == 0 {
				return nil, ComponentManagerFactoryNotRegisteredError{
					ComponentType: componentType,
				}
			}

			return nil, UnknownComponentManagerImplementationError{
				ComponentType:  componentType,
				Implementation: implName,
				Available:      available,
				RegisteredFor:  c.componentTypesForImplementation(implName),
			}
		}

		descriptors = append(descriptors, descriptor)
	}

	sortDescriptors(descriptors)
	return descriptors, nil
}

func (c Catalog) componentTypesForImplementation(
	implementation string,
) []devicetypes.ComponentType {
	types := make([]devicetypes.ComponentType, 0)
	for componentType, descriptors := range c.descriptors {
		if _, ok := descriptors[implementation]; ok {
			types = append(types, componentType)
		}
	}
	slices.Sort(types)
	return types
}

// Normalize validates a descriptor and returns its normalized value.
func (d Descriptor) Normalize() (Descriptor, error) {
	identity, err := d.DescriptorIdentity.Normalize()
	if err != nil {
		return Descriptor{}, err
	}

	d.DescriptorIdentity = identity

	requiredProviders := make([]string, 0, len(d.RequiredProviders))
	seen := make(map[string]struct{}, len(d.RequiredProviders))
	for _, name := range d.RequiredProviders {
		name = strings.TrimSpace(name)
		if name == "" {
			return Descriptor{}, providerapi.ErrProviderNameEmpty
		}
		if _, ok := seen[name]; ok {
			continue
		}
		seen[name] = struct{}{}
		requiredProviders = append(requiredProviders, name)
	}
	slices.Sort(requiredProviders)
	d.RequiredProviders = requiredProviders

	capabilities, err := d.Capabilities.Normalize()
	if err != nil {
		return Descriptor{}, err
	}
	d.Capabilities = capabilities

	return d, nil
}

// Identity returns the descriptor identity.
func (d Descriptor) Identity() DescriptorIdentity {
	return d.DescriptorIdentity
}

// Clone returns a descriptor copy whose mutable fields do not share storage
// with the source descriptor.
func (d Descriptor) Clone() Descriptor {
	d.RequiredProviders = slices.Clone(d.RequiredProviders)
	d.Capabilities = d.Capabilities.Clone()
	return d
}

// Equal reports whether two normalized descriptors describe the same component
// manager implementation, provider requirements, and capabilities.
func (d Descriptor) Equal(other Descriptor) bool {
	return d.Type == other.Type &&
		d.Implementation == other.Implementation &&
		slices.Equal(d.RequiredProviders, other.RequiredProviders) &&
		slices.Equal(d.Capabilities, other.Capabilities)
}

func sortDescriptors(descriptors []Descriptor) {
	slices.SortFunc(descriptors, func(a, b Descriptor) int {
		return compareDescriptorIdentities(a.Identity(), b.Identity())
	})
}

func descriptorImplementationNames(
	descriptors map[string]Descriptor,
) []string {
	names := make([]string, 0, len(descriptors))
	for name := range descriptors {
		names = append(names, name)
	}
	slices.Sort(names)
	return names
}
