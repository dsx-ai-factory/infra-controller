// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package firmwareversion selects per-tray firmware inputs consistently during
// task admission and execution.
package firmwareversion

import (
	"encoding/json"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
	"strings"
)

// knownComponentTypeKeys are the JSON keys recognised in a layered
// TargetVersion object. Used to distinguish the new per-component-type
// format from the legacy flat format.
var knownComponentTypeKeys = []string{"compute", "nvswitch", "powershelf"}

// Extract extracts the component-specific section from
// a layered TargetVersion JSON string. The expected top-level structure is:
//
//	{
//	  "compute":    {"bmc": "7.10.30", "uefi": "2.22.1"},
//	  "nvswitch":  "1.3.1",
//	  "powershelf": "r1.3.9"
//	}
//
// If the key for componentType is present, the corresponding value is
// returned. String scalars are unquoted so component managers receive the
// plain value (e.g. "1.3.1" → 1.3.1); object values are returned as raw
// JSON for component managers that parse multi-field version payloads.
// If the key is absent but the document contains another known
// component-type key, selected is false to exclude that type's rule steps.
// If the document does not look layered, the original string is returned for
// backward compatibility with single-component updates.
func Extract(rawVersion string, componentType devicetypes.ComponentType) (string, bool) {
	if rawVersion == "" {
		return "", true
	}

	var layered map[string]json.RawMessage
	if err := json.Unmarshal([]byte(rawVersion), &layered); err != nil {
		return rawVersion, true
	}

	key := strings.ToLower(devicetypes.ComponentTypeToString(componentType))
	if section, ok := layered[key]; ok {
		if len(section) > 0 && section[0] == '"' {
			var s string
			if err := json.Unmarshal(section, &s); err == nil {
				return s, true
			}
		}
		return string(section), true
	}

	for _, known := range knownComponentTypeKeys {
		if _, found := layered[known]; found {
			return "", false
		}
	}

	return rawVersion, true
}
