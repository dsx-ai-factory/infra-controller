// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"errors"
	"fmt"

	cmcatalog "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/catalog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/componentmanager/providerapi"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

var (
	// ErrConfigNotConfigured reports that a nil component manager config was
	// provided where a Config value is required.
	ErrConfigNotConfigured = errors.New("component manager config is not configured")

	// ErrUnknownComponentType reports an unrecognized component type in config.
	ErrUnknownComponentType = cmcatalog.ErrUnknownComponentType

	// ErrComponentManagerImplementationNameEmpty reports that a component type
	// was configured without an implementation name.
	ErrComponentManagerImplementationNameEmpty = cmcatalog.ErrComponentManagerImplementationNameEmpty

	// ErrComponentManagersNotConfigured reports that the service config has no
	// component manager entries.
	ErrComponentManagersNotConfigured = errors.New("component managers are not configured")

	// ErrDuplicateProviderConfig reports duplicate provider configuration after
	// provider names are normalized.
	ErrDuplicateProviderConfig = errors.New("duplicate provider config")

	// ErrDuplicateManagerConfig reports duplicate manager configuration after
	// descriptor identities are normalized.
	ErrDuplicateManagerConfig = errors.New("duplicate manager config")

	// ErrProviderConfigDecoderNotRegistered reports that a provider is required
	// but no config decoder is registered for it.
	ErrProviderConfigDecoderNotRegistered = errors.New("provider config decoder is not registered")

	// ErrManagerConfigDecoderNotRegistered reports that a manager config was
	// provided but no decoder is registered for that descriptor identity.
	ErrManagerConfigDecoderNotRegistered = errors.New("manager config decoder is not registered")

	// ErrProviderConfigDecoderRegistryRequired reports that a config operation
	// requires a provider config decoder registry argument.
	ErrProviderConfigDecoderRegistryRequired = errors.New("provider config decoder registry is required")

	// ErrManagerConfigDecoderRegistryRequired reports that manager YAML config
	// was provided without a manager config decoder registry.
	ErrManagerConfigDecoderRegistryRequired = errors.New("manager config decoder registry is required")

	// ErrManagerConfigDecoderRegistryNotConfigured reports that a decoder
	// registry was required but not configured.
	ErrManagerConfigDecoderRegistryNotConfigured = errors.New("manager config decoder registry is not configured")

	// ErrManagerConfigDecoderNotConfigured reports that a nil manager config
	// decoder was provided for registration.
	ErrManagerConfigDecoderNotConfigured = errors.New("manager config decoder is not configured")

	// ErrManagerConfigDecoderAlreadyRegistered reports a duplicate manager
	// config decoder registration.
	ErrManagerConfigDecoderAlreadyRegistered = errors.New("manager config decoder already registered")

	// ErrManagerConfigNotConfigured reports that a nil manager config was
	// provided where a typed config value is required.
	ErrManagerConfigNotConfigured = errors.New("manager config is not configured")

	// ErrManagerConfigIdentityMismatch reports that a manager config value was
	// used with a descriptor identity it does not support.
	ErrManagerConfigIdentityMismatch = errors.New("manager config identity mismatch")

	// ErrManagerConfigNotSelected reports that manager-specific config was
	// provided for a manager that is not selected by component_managers.
	ErrManagerConfigNotSelected = errors.New("manager config is not selected")

	// ErrInvalidManagerConfig reports that manager-specific YAML was invalid.
	ErrInvalidManagerConfig = errors.New("invalid manager config")

	// ErrInvalidManagerConfigField reports that a manager config field value was
	// invalid.
	ErrInvalidManagerConfigField = errors.New("invalid manager config field")
)

// UnknownComponentTypeError includes the unrecognized component type string.
type UnknownComponentTypeError = cmcatalog.UnknownComponentTypeError

// ComponentManagerImplementationNameEmptyError includes the component type
// whose configured implementation name is empty.
type ComponentManagerImplementationNameEmptyError = cmcatalog.ComponentManagerImplementationNameEmptyError

// DuplicateProviderConfigError includes the normalized duplicate provider name.
type DuplicateProviderConfigError struct {
	// Name is the duplicate provider name after trimming whitespace.
	Name string
}

func (e DuplicateProviderConfigError) Error() string {
	return fmt.Sprintf("duplicate provider config for %q", e.Name)
}

func (e DuplicateProviderConfigError) Is(target error) bool {
	return target == ErrDuplicateProviderConfig
}

// DuplicateManagerConfigError includes the normalized duplicate descriptor
// identity.
type DuplicateManagerConfigError struct {
	Identity cmcatalog.DescriptorIdentity
}

func (e DuplicateManagerConfigError) Error() string {
	return fmt.Sprintf(
		"duplicate manager config for %q",
		e.Identity.String(),
	)
}

func (e DuplicateManagerConfigError) Is(target error) bool {
	return target == ErrDuplicateManagerConfig
}

// ProviderConfigDecoderNotRegisteredError includes the provider name with no
// registered config decoder.
type ProviderConfigDecoderNotRegisteredError struct {
	// Name is the provider name that has no registered config decoder.
	Name string
	// ComponentType is the component manager type requiring the provider.
	ComponentType devicetypes.ComponentType
	// Implementation is the component manager implementation requiring the
	// provider.
	Implementation string
}

func (e ProviderConfigDecoderNotRegisteredError) Error() string {
	if e.ComponentType != devicetypes.ComponentTypeUnknown || e.Implementation != "" {
		return fmt.Sprintf(
			"provider config decoder %q required by component manager %s/%s is not registered",
			e.Name,
			devicetypes.ComponentTypeToString(e.ComponentType),
			e.Implementation,
		)
	}
	return fmt.Sprintf("provider config decoder %q is not registered", e.Name)
}

func (e ProviderConfigDecoderNotRegisteredError) Is(target error) bool {
	return target == ErrProviderConfigDecoderNotRegistered
}

// ManagerConfigDecoderNotRegisteredError includes the manager identity with no
// registered config decoder.
type ManagerConfigDecoderNotRegisteredError struct {
	Identity cmcatalog.DescriptorIdentity
}

func (e ManagerConfigDecoderNotRegisteredError) Error() string {
	return fmt.Sprintf(
		"manager config decoder for %q is not registered",
		e.Identity.String(),
	)
}

func (e ManagerConfigDecoderNotRegisteredError) Is(target error) bool {
	return target == ErrManagerConfigDecoderNotRegistered
}

// ManagerConfigDecoderAlreadyRegisteredError includes the duplicate descriptor
// identity.
type ManagerConfigDecoderAlreadyRegisteredError struct {
	Identity cmcatalog.DescriptorIdentity
}

func (e ManagerConfigDecoderAlreadyRegisteredError) Error() string {
	return fmt.Sprintf(
		"manager config decoder for %q already registered",
		e.Identity.String(),
	)
}

func (e ManagerConfigDecoderAlreadyRegisteredError) Is(target error) bool {
	return target == ErrManagerConfigDecoderAlreadyRegistered
}

// ManagerConfigNotConfiguredError includes the manager identity whose config
// was required but nil.
type ManagerConfigNotConfiguredError struct {
	Identity cmcatalog.DescriptorIdentity
}

func (e ManagerConfigNotConfiguredError) Error() string {
	if e.Identity == (cmcatalog.DescriptorIdentity{}) {
		return ErrManagerConfigNotConfigured.Error()
	}
	return fmt.Sprintf(
		"%s: %s",
		ErrManagerConfigNotConfigured,
		e.Identity.String(),
	)
}

func (e ManagerConfigNotConfiguredError) Is(target error) bool {
	return target == ErrManagerConfigNotConfigured
}

// ManagerConfigIdentityMismatchError includes the expected descriptor identity
// and the identity supported by the manager config value.
type ManagerConfigIdentityMismatchError struct {
	Expected cmcatalog.DescriptorIdentity
	Actual   cmcatalog.DescriptorIdentity
}

func (e ManagerConfigIdentityMismatchError) Error() string {
	return fmt.Sprintf(
		"manager config identity mismatch: expected %s, got %s",
		e.Expected.String(),
		e.Actual.String(),
	)
}

func (e ManagerConfigIdentityMismatchError) Is(target error) bool {
	return target == ErrManagerConfigIdentityMismatch
}

// ManagerConfigNotSelectedError includes a manager config identity that is not
// selected by component_managers.
type ManagerConfigNotSelectedError struct {
	Identity               cmcatalog.DescriptorIdentity
	SelectedImplementation string
}

func (e ManagerConfigNotSelectedError) Error() string {
	if e.SelectedImplementation == "" {
		return fmt.Sprintf(
			"manager config for %s is not selected by component_managers",
			e.Identity.String(),
		)
	}
	return fmt.Sprintf(
		"manager config for %s is not selected by component_managers; selected implementation is %q",
		e.Identity.String(),
		e.SelectedImplementation,
	)
}

func (e ManagerConfigNotSelectedError) Is(target error) bool {
	return target == ErrManagerConfigNotSelected
}

// InvalidManagerConfigError wraps manager-specific YAML decode errors.
type InvalidManagerConfigError struct {
	Identity cmcatalog.DescriptorIdentity
	Err      error
}

func (e InvalidManagerConfigError) Error() string {
	msg := fmt.Sprintf(
		"invalid component manager %s config",
		e.Identity.String(),
	)
	if e.Err == nil {
		return msg
	}
	return fmt.Sprintf("%s: %v", msg, e.Err)
}

func (e InvalidManagerConfigError) Unwrap() error {
	return e.Err
}

func (e InvalidManagerConfigError) Is(target error) bool {
	return target == ErrInvalidManagerConfig
}

// InvalidManagerConfigFieldError wraps invalid manager config field values.
type InvalidManagerConfigFieldError struct {
	Identity cmcatalog.DescriptorIdentity
	Field    string
	Err      error
}

func (e InvalidManagerConfigFieldError) Error() string {
	msg := fmt.Sprintf(
		"invalid component manager %s %s",
		e.Identity.String(),
		e.Field,
	)
	if e.Err == nil {
		return msg
	}
	return fmt.Sprintf("%s: %v", msg, e.Err)
}

func (e InvalidManagerConfigFieldError) Unwrap() error {
	return e.Err
}

func (e InvalidManagerConfigFieldError) Is(target error) bool {
	return target == ErrInvalidManagerConfigField
}

// RequiredProviderNotConfiguredError includes the required provider and the
// component manager identity that requires it.
type RequiredProviderNotConfiguredError struct {
	// Provider is the provider name required by the component manager.
	Provider string
	// ComponentType is the component manager type requiring the provider.
	ComponentType devicetypes.ComponentType
	// Implementation is the component manager implementation requiring the
	// provider.
	Implementation string
}

func (e RequiredProviderNotConfiguredError) Error() string {
	return fmt.Sprintf(
		"provider %q required by component manager %s/%s is not configured",
		e.Provider,
		devicetypes.ComponentTypeToString(e.ComponentType),
		e.Implementation,
	)
}

func (e RequiredProviderNotConfiguredError) Is(target error) bool {
	return target == providerapi.ErrProviderNotConfigured
}
