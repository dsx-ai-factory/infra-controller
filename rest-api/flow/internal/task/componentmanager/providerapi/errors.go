// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package providerapi

import (
	"errors"
	"fmt"
)

var (
	// ErrProviderConfigDecoderRegistryNotConfigured reports that a decoder
	// registry was required but not configured.
	ErrProviderConfigDecoderRegistryNotConfigured = errors.New("provider config decoder registry is not configured")

	// ErrProviderConfigDecoderNotConfigured reports that a nil decoder was
	// provided for registration.
	ErrProviderConfigDecoderNotConfigured = errors.New("provider config decoder is not configured")

	// ErrProviderConfigDecoderNameEmpty reports that a decoder returned an empty
	// provider name.
	ErrProviderConfigDecoderNameEmpty = errors.New("provider config decoder name is empty")

	// ErrProviderConfigDecoderAlreadyRegistered reports a duplicate decoder
	// registration.
	ErrProviderConfigDecoderAlreadyRegistered = errors.New("provider config decoder already registered")

	// ErrInvalidProviderConfig reports that provider-specific YAML was invalid.
	ErrInvalidProviderConfig = errors.New("invalid provider config")

	// ErrInvalidProviderConfigField reports that a provider config field value
	// was invalid.
	ErrInvalidProviderConfigField = errors.New("invalid provider config field")

	// ErrProviderRegistryNotConfigured reports that the provider registry is not
	// available.
	ErrProviderRegistryNotConfigured = errors.New("provider registry is not configured")

	// ErrProviderNotConfigured reports that a provider or provider config is not
	// available.
	ErrProviderNotConfigured = errors.New("provider is not configured")

	// ErrUnknownProvider reports that a provider name is not known in the
	// current provider context.
	ErrUnknownProvider = errors.New("unknown provider")

	// ErrProviderTypeMismatch reports that a provider exists but has a different
	// concrete type than the caller requested.
	ErrProviderTypeMismatch = errors.New("provider type mismatch")

	// ErrProviderNameEmpty reports an empty provider name.
	ErrProviderNameEmpty = errors.New("provider name is empty")

	// ErrDuplicateProvider reports that a provider is already registered.
	ErrDuplicateProvider = errors.New("duplicate provider")

	// ErrProviderConfigNameMismatch reports that a provider config's name does
	// not match the name it was registered under.
	ErrProviderConfigNameMismatch = errors.New("provider config name mismatch")

	// ErrProviderNameMismatch reports that a created provider's name does not
	// match the provider config name.
	ErrProviderNameMismatch = errors.New("provider name mismatch")
)

// ProviderConfigDecoderAlreadyRegisteredError includes the duplicate provider
// decoder name.
type ProviderConfigDecoderAlreadyRegisteredError struct {
	Name string
}

func (e ProviderConfigDecoderAlreadyRegisteredError) Error() string {
	return fmt.Sprintf("provider config decoder %q already registered", e.Name)
}

func (e ProviderConfigDecoderAlreadyRegisteredError) Is(target error) bool {
	return target == ErrProviderConfigDecoderAlreadyRegistered
}

// InvalidProviderConfigError wraps provider-specific YAML decode errors.
type InvalidProviderConfigError struct {
	Provider string
	Err      error
}

func (e InvalidProviderConfigError) Error() string {
	msg := fmt.Sprintf("invalid %s config", e.Provider)
	if e.Err == nil {
		return msg
	}
	return fmt.Sprintf("%s: %v", msg, e.Err)
}

func (e InvalidProviderConfigError) Unwrap() error {
	return e.Err
}

func (e InvalidProviderConfigError) Is(target error) bool {
	return target == ErrInvalidProviderConfig
}

// InvalidProviderConfigFieldError wraps invalid provider config field values.
type InvalidProviderConfigFieldError struct {
	Provider string
	Field    string
	Err      error
}

func (e InvalidProviderConfigFieldError) Error() string {
	msg := fmt.Sprintf("invalid %s %s", e.Provider, e.Field)
	if e.Err == nil {
		return msg
	}
	return fmt.Sprintf("%s: %v", msg, e.Err)
}

func (e InvalidProviderConfigFieldError) Unwrap() error {
	return e.Err
}

func (e InvalidProviderConfigFieldError) Is(target error) bool {
	return target == ErrInvalidProviderConfigField
}

// UnknownProviderError includes the unknown provider name.
type UnknownProviderError struct {
	Name string
}

func (e UnknownProviderError) Error() string {
	return fmt.Sprintf("%s: %s", ErrUnknownProvider, e.Name)
}

func (e UnknownProviderError) Is(target error) bool {
	return target == ErrUnknownProvider
}

// ProviderNotConfiguredError includes the provider name that is required but
// not configured.
type ProviderNotConfiguredError struct {
	Name string
}

func (e ProviderNotConfiguredError) Error() string {
	if e.Name == "" {
		return ErrProviderNotConfigured.Error()
	}
	return fmt.Sprintf("%s: %s", ErrProviderNotConfigured, e.Name)
}

func (e ProviderNotConfiguredError) Is(target error) bool {
	return target == ErrProviderNotConfigured
}

// ProviderTypeMismatchError includes the provider name with the unexpected
// concrete type.
type ProviderTypeMismatchError struct {
	Name string
}

func (e ProviderTypeMismatchError) Error() string {
	return fmt.Sprintf("provider '%s' is not of expected type", e.Name)
}

func (e ProviderTypeMismatchError) Is(target error) bool {
	return target == ErrProviderTypeMismatch
}

// DuplicateProviderError includes the duplicate provider name.
type DuplicateProviderError struct {
	Name string
}

func (e DuplicateProviderError) Error() string {
	return fmt.Sprintf("duplicate provider: %s", e.Name)
}

func (e DuplicateProviderError) Is(target error) bool {
	return target == ErrDuplicateProvider
}

// ProviderConfigNameMismatchError includes the provider config map key and the
// name returned by the config.
type ProviderConfigNameMismatchError struct {
	Name       string
	ConfigName string
}

func (e ProviderConfigNameMismatchError) Error() string {
	return fmt.Sprintf(
		"provider config name mismatch: configured as %q but config returned %q",
		e.Name,
		e.ConfigName,
	)
}

func (e ProviderConfigNameMismatchError) Is(target error) bool {
	return target == ErrProviderConfigNameMismatch
}

// ProviderNameMismatchError includes the expected provider name and the name
// returned by the created provider.
type ProviderNameMismatchError struct {
	Name         string
	ProviderName string
}

func (e ProviderNameMismatchError) Error() string {
	return fmt.Sprintf(
		"provider name mismatch: expected %q but provider returned %q",
		e.Name,
		e.ProviderName,
	)
}

func (e ProviderNameMismatchError) Is(target error) bool {
	return target == ErrProviderNameMismatch
}
