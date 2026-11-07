// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package catalog

import (
	"errors"
	"fmt"
	"sort"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

var (
	// ErrUnknownComponentType reports an unrecognized component type.
	ErrUnknownComponentType = errors.New("unknown component type")

	// ErrComponentManagerImplementationNameEmpty reports that a component type
	// was configured without an implementation name.
	ErrComponentManagerImplementationNameEmpty = errors.New("component manager implementation name is empty")

	// ErrComponentManagerFactoryNotRegistered reports that no component manager
	// implementation was registered for a component type.
	ErrComponentManagerFactoryNotRegistered = errors.New("component manager factory is not registered")

	// ErrDuplicateDescriptor reports duplicate descriptor registration for the
	// same component type and implementation.
	ErrDuplicateDescriptor = errors.New("duplicate component manager descriptor")

	// ErrUnknownComponentManagerImplementation reports that the configured
	// implementation name is not registered for a component type.
	ErrUnknownComponentManagerImplementation = errors.New("unknown component manager implementation")
)

// UnknownComponentTypeError includes the unrecognized component type string.
type UnknownComponentTypeError struct {
	// Name is the component type name read from config or descriptor metadata.
	Name string
}

func (e UnknownComponentTypeError) Error() string {
	return fmt.Sprintf("%s: %s", ErrUnknownComponentType, e.Name)
}

func (e UnknownComponentTypeError) Is(target error) bool {
	return target == ErrUnknownComponentType
}

// ComponentManagerImplementationNameEmptyError includes the component type
// whose configured implementation name is empty.
type ComponentManagerImplementationNameEmptyError struct {
	// ComponentType is the component type with an empty implementation name.
	ComponentType devicetypes.ComponentType
}

func (e ComponentManagerImplementationNameEmptyError) Error() string {
	return fmt.Sprintf(
		"%s for component type %s",
		ErrComponentManagerImplementationNameEmpty,
		devicetypes.ComponentTypeToString(e.ComponentType),
	)
}

func (e ComponentManagerImplementationNameEmptyError) Is(target error) bool {
	return target == ErrComponentManagerImplementationNameEmpty
}

// ComponentManagerFactoryNotRegisteredError includes the component type that
// has no registered implementations.
type ComponentManagerFactoryNotRegisteredError struct {
	ComponentType devicetypes.ComponentType
}

func (e ComponentManagerFactoryNotRegisteredError) Error() string {
	return fmt.Sprintf(
		"no factories registered for component type: %s",
		devicetypes.ComponentTypeToString(e.ComponentType),
	)
}

func (e ComponentManagerFactoryNotRegisteredError) Is(target error) bool {
	return target == ErrComponentManagerFactoryNotRegistered
}

// DuplicateDescriptorError includes the duplicate descriptor identity.
type DuplicateDescriptorError struct {
	ComponentType  devicetypes.ComponentType
	Implementation string
}

func (e DuplicateDescriptorError) Error() string {
	return fmt.Sprintf(
		"duplicate component manager descriptor for component type %s with implementation %q",
		devicetypes.ComponentTypeToString(e.ComponentType),
		e.Implementation,
	)
}

func (e DuplicateDescriptorError) Is(target error) bool {
	return target == ErrDuplicateDescriptor
}

// UnknownComponentManagerImplementationError includes the implementation name
// that was requested and the implementations that were available.
type UnknownComponentManagerImplementationError struct {
	ComponentType  devicetypes.ComponentType
	Implementation string
	Available      []string
	RegisteredFor  []devicetypes.ComponentType
}

func (e UnknownComponentManagerImplementationError) Error() string {
	available := append([]string(nil), e.Available...)
	sort.Strings(available)
	msg := fmt.Sprintf(
		"unknown implementation '%s' for component type %s, available: %v",
		e.Implementation,
		devicetypes.ComponentTypeToString(e.ComponentType),
		available,
	)
	if len(e.RegisteredFor) == 0 {
		return msg
	}

	registeredFor := make([]string, 0, len(e.RegisteredFor))
	for _, componentType := range e.RegisteredFor {
		registeredFor = append(
			registeredFor,
			devicetypes.ComponentTypeToString(componentType),
		)
	}
	sort.Strings(registeredFor)
	return fmt.Sprintf("%s; registered for component types: %v", msg, registeredFor)
}

func (e UnknownComponentManagerImplementationError) Is(target error) bool {
	return target == ErrUnknownComponentManagerImplementation
}
