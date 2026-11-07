// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package capability

import (
	"errors"
	"fmt"
	"slices"
	"strings"
)

// Capability identifies an operation supported by a component manager
// implementation.
type Capability string

const (
	CapabilityInjectExpectation        Capability = "InjectExpectation"
	CapabilityPowerControl             Capability = "PowerControl"
	CapabilityPowerStatus              Capability = "PowerStatus"
	CapabilityFirmwareControl          Capability = "FirmwareControl"
	CapabilityFirmwareStatus           Capability = "FirmwareStatus"
	CapabilityBringUpControl           Capability = "BringUpControl"
	CapabilityBringUpStatus            Capability = "BringUpStatus"
	CapabilityFirmwareConsistencyCheck Capability = "FirmwareConsistencyCheck"
	CapabilityDecommissionControl      Capability = "DecommissionControl"
	CapabilityDecommissionStatus       Capability = "DecommissionStatus"
)

var (
	// ErrNameEmpty reports an empty capability name.
	ErrNameEmpty = errors.New("component manager capability name is empty")

	// ErrUnknown reports an unsupported capability name.
	ErrUnknown = errors.New("unknown component manager capability")
)

// Parse trims and validates a component manager capability name.
func Parse(name string) (Capability, error) {
	s := strings.TrimSpace(name)
	if s == "" {
		return "", NameEmptyError{}
	}

	c := Capability(s)
	if !c.Valid() {
		return "", UnknownError{Capability: c}
	}

	return c, nil
}

// NameEmptyError reports an empty capability name in descriptor metadata.
type NameEmptyError struct{}

func (e NameEmptyError) Error() string {
	return ErrNameEmpty.Error()
}

func (e NameEmptyError) Is(target error) bool {
	return target == ErrNameEmpty
}

// UnknownError includes the unsupported capability name.
type UnknownError struct {
	Capability Capability
}

func (e UnknownError) Error() string {
	return fmt.Sprintf("%s: %q", ErrUnknown, e.Capability)
}

func (e UnknownError) Is(target error) bool {
	return target == ErrUnknown
}

// String returns the capability name.
func (c Capability) String() string {
	return string(c)
}

// Valid reports whether c is one of the known component manager capabilities.
func (c Capability) Valid() bool {
	switch c {
	case CapabilityInjectExpectation,
		CapabilityPowerControl,
		CapabilityPowerStatus,
		CapabilityFirmwareControl,
		CapabilityFirmwareStatus,
		CapabilityBringUpControl,
		CapabilityBringUpStatus,
		CapabilityFirmwareConsistencyCheck,
		CapabilityDecommissionControl,
		CapabilityDecommissionStatus:
		return true
	default:
		return false
	}
}

// CapabilitySet is the normalized set of operations supported by a component
// manager implementation.
type CapabilitySet []Capability

// NewSet returns capabilities trimmed, deduplicated, and sorted.
func NewSet(capabilities ...Capability) (CapabilitySet, error) {
	return CapabilitySet(capabilities).Normalize()
}

// Contains reports whether s includes capability.
func (s CapabilitySet) Contains(capability Capability) bool {
	return slices.Contains(s, capability)
}

// Add returns a capability set with capability included once. The receiver must
// already be normalized; use NewSet or Normalize to build a CapabilitySet from
// arbitrary input.
func (s CapabilitySet) Add(capability Capability) (CapabilitySet, error) {
	capability, err := Parse(capability.String())
	if err != nil {
		return nil, err
	}

	if s.Contains(capability) {
		return s, nil
	}

	return append(s, capability).Sorted(), nil
}

// Normalize returns capabilities trimmed, deduplicated, and sorted.
func (s CapabilitySet) Normalize() (CapabilitySet, error) {
	if len(s) == 0 {
		return nil, nil
	}

	capabilities := make(CapabilitySet, 0, len(s))
	seen := make(map[Capability]struct{}, len(s))
	for _, capability := range s {
		capability, err := Parse(capability.String())
		if err != nil {
			return nil, err
		}
		if _, ok := seen[capability]; ok {
			continue
		}
		seen[capability] = struct{}{}
		capabilities = append(capabilities, capability)
	}

	slices.Sort(capabilities)

	return capabilities, nil
}

// Clone returns a capability set copy that does not share storage with the
// source set.
func (s CapabilitySet) Clone() CapabilitySet {
	return slices.Clone(s)
}

// Sorted returns a sorted copy of s.
func (s CapabilitySet) Sorted() CapabilitySet {
	sorted := s.Clone()
	slices.Sort(sorted)
	return sorted
}

// Strings returns the capability names as strings.
func (s CapabilitySet) Strings() []string {
	names := make([]string, 0, len(s))
	for _, c := range s {
		names = append(names, c.String())
	}
	return names
}
