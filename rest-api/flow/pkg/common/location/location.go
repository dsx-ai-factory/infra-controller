// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package location

import (
	"encoding/json"

	"github.com/rs/zerolog/log"
)

// Location represents location information for devices
type Location struct {
	Region     string `json:"region"`      // Geographic region
	DataCenter string `json:"data_center"` // Data center identifier
	Room       string `json:"room"`        // Room identifier
	Position   string `json:"position"`    // Specific position within room
}

// New creates a Location from JSON bytes
// Returns a Location with "" values if unmarshaling fails
func New(b []byte) Location {
	var loc Location
	if err := json.Unmarshal(b, &loc); err != nil {
		log.Debug().Err(err).Msg("Failed to unmarshal location")

		// Return all fields as ""
		return Location{
			Region:     "",
			DataCenter: "",
			Room:       "",
			Position:   "",
		}
	}

	return loc
}

// ToMap converts the Location to a map[string]any, including only non-empty
// fields. Returns nil if loc is nil or all fields are empty.
func (loc *Location) ToMap() map[string]any {
	if loc == nil {
		return nil
	}

	m := make(map[string]any)
	if loc.Region != "" {
		m["region"] = loc.Region
	}
	if loc.DataCenter != "" {
		m["data_center"] = loc.DataCenter
	}
	if loc.Room != "" {
		m["room"] = loc.Room
	}
	if loc.Position != "" {
		m["position"] = loc.Position
	}

	if len(m) == 0 {
		return nil
	}

	return m
}

// Encode serializes the Location to a JSON string. It returns ""
// if marshaling fails
func (loc *Location) Encode() string {
	b, err := json.Marshal(loc)
	if err != nil {
		log.Debug().Err(err).Msg("Failed to marshal location")
		return ""
	}

	return string(b)
}

// BuildPatchedLocation builds a patched location from the current location
// and the input location. It goes through the patchable fields and builds
// the patched location. If there is no change on patchable fields, it returns
// nil.
func (loc *Location) BuildPatchedLocation(cur *Location) *Location {
	if loc == nil || cur == nil {
		return nil
	}

	// Make a copy fo the current location which serves as the base for the
	// patched location.
	patchedLoc := *cur
	patched := false

	if len(loc.Region) > 0 && patchedLoc.Region != loc.Region {
		patchedLoc.Region = loc.Region
		patched = true
	}

	if len(loc.DataCenter) > 0 && patchedLoc.DataCenter != loc.DataCenter {
		patchedLoc.DataCenter = loc.DataCenter
		patched = true
	}

	if len(loc.Room) > 0 && patchedLoc.Room != loc.Room {
		patchedLoc.Room = loc.Room
		patched = true
	}

	if len(loc.Position) > 0 && patchedLoc.Position != loc.Position {
		patchedLoc.Position = loc.Position
		patched = true
	}

	if !patched {
		return nil
	}

	return &patchedLoc
}
