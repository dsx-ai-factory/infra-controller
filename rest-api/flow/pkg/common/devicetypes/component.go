// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package devicetypes

import (
	"encoding/json"
	"fmt"
	"strings"
)

// Define component types
type ComponentType int

const (
	ComponentTypeUnknown ComponentType = iota
	ComponentTypeCompute
	ComponentTypeNVSwitch
	ComponentTypePowerShelf
	ComponentTypeToRSwitch
	ComponentTypeUMS
	ComponentTypeCDU
)

var (
	componentTypeStrings = map[ComponentType]string{
		ComponentTypeUnknown:    "Unknown",
		ComponentTypeCompute:    "Compute",
		ComponentTypeNVSwitch:   "NVSwitch",
		ComponentTypePowerShelf: "PowerShelf",
		ComponentTypeToRSwitch:  "ToRSwitch",
		ComponentTypeUMS:        "UMS",
		ComponentTypeCDU:        "CDU",
	}

	componentTypeStringMaxLen int
)

func init() {
	for _, str := range componentTypeStrings {
		if len(str) > componentTypeStringMaxLen {
			componentTypeStringMaxLen = len(str)
		}
	}
}

// ComponentTypes returns all the supported Component types
func ComponentTypes() []ComponentType {
	return []ComponentType{
		ComponentTypeUnknown,
		ComponentTypeCompute,
		ComponentTypeNVSwitch,
		ComponentTypePowerShelf,
		ComponentTypeToRSwitch,
		ComponentTypeUMS,
		ComponentTypeCDU,
	}
}

// ComponentTypeFromString returns the Component type from the given string.
func ComponentTypeFromString(str string) ComponentType {
	for ct, componentTypeStr := range componentTypeStrings {
		if strings.EqualFold(str, componentTypeStr) {
			return ct
		}
	}
	return ComponentTypeUnknown
}

// ComponentTypeToString returns the string representation for the given
// component type.
func ComponentTypeToString(ct ComponentType) string {
	return componentTypeStrings[ct]
}

// IsValidComponentTypeString reports whether str maps to a known, non-Unknown
// ComponentType.
func IsValidComponentTypeString(str string) bool {
	return ComponentTypeFromString(str) != ComponentTypeUnknown
}

// MarshalJSON serializes ComponentType as its string name (e.g. "Compute").
func (ct ComponentType) MarshalJSON() ([]byte, error) {
	return json.Marshal(ComponentTypeToString(ct))
}

// UnmarshalJSON parses a ComponentType from its string name (e.g. "compute").
// Returns an error only if the string is unrecognized (i.e. not a valid
// component type name and not the canonical "Unknown" string), so that
// round-trip serialization of ComponentTypeUnknown is preserved.
func (ct *ComponentType) UnmarshalJSON(data []byte) error {
	var s string
	if err := json.Unmarshal(data, &s); err != nil {
		return fmt.Errorf("component_type must be a string: %w", err)
	}

	result := ComponentTypeFromString(s)
	if result == ComponentTypeUnknown &&
		!strings.EqualFold(s, componentTypeStrings[ComponentTypeUnknown]) {
		return fmt.Errorf("unknown component type: %q", s)
	}

	*ct = result
	return nil
}

// MarshalText serializes ComponentType as its string name for use as a JSON
// map key (e.g. map[ComponentType]... → {"Compute": ...}).
func (ct ComponentType) MarshalText() ([]byte, error) {
	return []byte(ComponentTypeToString(ct)), nil
}

// UnmarshalText parses a ComponentType from its string name when used as a
// JSON map key. Returns an error only if the string is unrecognized (i.e. not
// a valid component type name and not the canonical "Unknown" string), so that
// round-trip serialization of ComponentTypeUnknown is preserved.
func (ct *ComponentType) UnmarshalText(data []byte) error {
	s := string(data)
	result := ComponentTypeFromString(s)
	if result == ComponentTypeUnknown &&
		!strings.EqualFold(s, componentTypeStrings[ComponentTypeUnknown]) {
		return fmt.Errorf("unknown component type: %q", s)
	}

	*ct = result
	return nil
}

// String return the aligned string representation for the given component
// type
func (ct ComponentType) String() string {
	spaces := componentTypeStringMaxLen - len(ComponentTypeToString(ct))
	return strings.Repeat(" ", spaces) + ComponentTypeToString(ct)
}
