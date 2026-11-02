// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"errors"
	"fmt"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// IdentifierType describes the identifiers accepted by the component manager API.
type IdentifierType string

const (
	// IdentifierTypeLegacy preserves manager IDs in pre-existing Temporal payloads.
	IdentifierTypeLegacy     IdentifierType = ""
	IdentifierTypeManagerID  IdentifierType = "manager_id"
	IdentifierTypeMACAddress IdentifierType = "mac_address"
)

// Target represents one homogeneous identifier batch for activity execution.
type Target struct {
	Type devicetypes.ComponentType
	// Keep the JSON names stable for persisted Temporal activity payloads.
	IdentifierType IdentifierType `json:"ComponentIDType,omitempty"`
	Identifiers    []string       `json:"ComponentIDs"`
}

// Validate returns an error if the Target has an unknown component type or
// cannot be expressed as one complete component-ID or MAC-address batch.
func (t *Target) Validate() error {
	if t.Type == devicetypes.ComponentTypeUnknown {
		return errors.New("component type is unknown")
	}
	switch t.IdentifierType {
	case IdentifierTypeLegacy, IdentifierTypeManagerID, IdentifierTypeMACAddress:
	default:
		return fmt.Errorf("unknown component ID type: %q", t.IdentifierType)
	}

	identifiers := t.Identifiers
	if len(identifiers) == 0 {
		return errors.New("component IDs or MAC addresses are required")
	}
	for _, identifier := range identifiers {
		if identifier == "" {
			return errors.New("component target identifiers must not be empty")
		}
	}

	return nil
}

// UsesMACAddresses reports the explicit identifier type; absent means manager ID.
func (t *Target) UsesMACAddresses() bool {
	return t.IdentifierType == IdentifierTypeMACAddress
}

// Len returns the number of selected components.
func (t *Target) Len() int {
	return len(t.Identifiers)
}

// String returns a human-readable representation for logging.
func (t *Target) String() string {
	identifierName := "component_ids"
	if t.UsesMACAddresses() {
		identifierName = "mac_addresses"
	}
	return fmt.Sprintf(
		"[type: %s, %s: %s]",
		devicetypes.ComponentTypeToString(t.Type),
		identifierName,
		strings.Join(t.Identifiers, ","),
	)
}
