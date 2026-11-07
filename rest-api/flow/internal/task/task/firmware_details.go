// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package task

import (
	"encoding/json"
	"net/url"
	"strings"
	"time"
	"unicode"

	"github.com/google/uuid"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// FirmwareTaskDetails contains stored firmware intent without authentication,
// reports, or raw manifest input. `Attributes` retain selected Flow UUIDs even
// when inventory no longer resolves them; groups are sorted and deduplicated.
// Any fields added to `TaskAttributes` must be safe for details readers.
type FirmwareTaskDetails struct {
	ID               uuid.UUID
	RackID           uuid.UUID
	Operation        operations.FirmwareOperation
	RequestedTargets FirmwareRequestedTargets
	Attributes       taskcommon.TaskAttributes
	// `SubTargets` preserves stored component selection; empty keeps the backend
	// default. It does not opt into the explicit `dpu` firmware component.
	SubTargets             []string
	OverrideReadinessCheck bool
	OverrideVersionCheck   bool
	// `StartTime` and `EndTime` are stored Unix seconds; zero means omitted.
	StartTime int64
	EndTime   int64
	Status    taskcommon.TaskStatus
	CreatedAt time.Time
	UpdatedAt time.Time
	StartedAt *time.Time
}

// FirmwareRequestedTargets describes shared input or the compute, NVSwitch and
// power-shelf sections recorded in `TargetVersion`. A missing map entry means no
// explicit input for that type, distinct from a present configured default. It
// does not establish execution membership, including for older workflow histories.
// `FromTargetVersion` sets exactly one of `Shared` and `ByType`; callers must check
// map presence separately from unavailable metadata. `Attributes` stays unchanged.
type FirmwareRequestedTargets struct {
	Shared *FirmwareTargetMetadata
	ByType map[devicetypes.ComponentType]FirmwareTargetMetadata
}

// FirmwareTargetKind identifies which safe metadata is available for a request.
type FirmwareTargetKind int

const (
	// FirmwareTargetUnavailable leaves the task listed without display metadata.
	FirmwareTargetUnavailable FirmwareTargetKind = iota
	// FirmwareTargetConfiguredDefault records an omitted target for execution-time resolution.
	FirmwareTargetConfiguredDefault
	// FirmwareTargetVersion preserves scalar input, including literal null or whitespace;
	// it does not validate a version or report installed or effective firmware.
	FirmwareTargetVersion
	// FirmwareTargetManifest identifies a supplied manifest by `Id` or `object_id` only.
	FirmwareTargetManifest
)

// FirmwareTargetMetadata describes requested input without a manifest body or
// artifact URL. Value is exact for a scalar version or manifest identity, and
// empty for configured defaults and unavailable metadata.
type FirmwareTargetMetadata struct {
	Kind  FirmwareTargetKind
	Value string
}

// FromTargetVersion replaces metadata using the supported keys and string-unwrapping
// rules from workflow.extractComponentTargetVersion in workflow/actions.go. This
// display-only conversion leaves backend input and recorded selections unchanged.
func (t *FirmwareRequestedTargets) FromTargetVersion(raw string) {
	var object map[string]json.RawMessage
	err := json.Unmarshal([]byte(raw), &object)
	if err == nil {
		byType := make(map[devicetypes.ComponentType]FirmwareTargetMetadata)
		for _, component := range []struct {
			key string
			typ devicetypes.ComponentType
		}{
			{key: "compute", typ: devicetypes.ComponentTypeCompute},
			{key: "nvswitch", typ: devicetypes.ComponentTypeNVSwitch},
			{key: "powershelf", typ: devicetypes.ComponentTypePowerShelf},
		} {
			section, exists := object[component.key]
			if !exists {
				continue
			}
			value := string(section)
			if len(section) > 0 && section[0] == '"' {
				var scalar string
				err = json.Unmarshal(section, &scalar)
				if err == nil {
					value = scalar
				}
			}
			var metadata FirmwareTargetMetadata
			metadata.fromInput(value)
			byType[component.typ] = metadata
		}
		if len(byType) > 0 {
			*t = FirmwareRequestedTargets{ByType: byType}
			return
		}
	}
	var shared FirmwareTargetMetadata
	shared.fromInput(raw)
	*t = FirmwareRequestedTargets{Shared: &shared}
}

// fromInput extracts only the case-sensitive manifest identity or a displayable
// scalar. Unsupported input clears earlier metadata without rejecting its task.
func (t *FirmwareTargetMetadata) fromInput(raw string) {
	*t = FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}
	if raw == "" {
		t.Kind = FirmwareTargetConfiguredDefault
		return
	}
	var object map[string]json.RawMessage
	err := json.Unmarshal([]byte(raw), &object)
	if err == nil && object != nil {
		identity, exists := object["Id"]
		if !exists {
			identity, exists = object["object_id"]
		}
		if !exists {
			return
		}
		var id string
		err = json.Unmarshal(identity, &id)
		if err == nil && isDisplayableFirmwareValue(id) {
			*t = FirmwareTargetMetadata{Kind: FirmwareTargetManifest, Value: id}
		}
		return
	}
	if isDisplayableFirmwareValue(raw) {
		*t = FirmwareTargetMetadata{Kind: FirmwareTargetVersion, Value: raw}
	}
}

// isDisplayableFirmwareValue conservatively hides structured input, locations and
// URI-shaped values, including scheme-shaped version IDs. This controls display,
// not backend admission. Inspection may unquote once, but returned bytes stay exact.
func isDisplayableFirmwareValue(value string) bool {
	if value == "" {
		return false
	}
	inspected := strings.TrimSpace(value)
	if strings.HasPrefix(inspected, `"`) {
		var scalar string
		err := json.Unmarshal([]byte(inspected), &scalar)
		if err != nil {
			return false
		}
		inspected = strings.TrimSpace(scalar)
	}
	// Check the whole value: a BOM or other prefix must not disguise inline JSON.
	// Paths, query/userinfo delimiters and fragments can contain artifact secrets.
	if strings.ContainsAny(inspected, "{}[]\"/\\?@&#=") {
		return false
	}
	for _, r := range inspected {
		if unicode.Is(unicode.Cf, r) || (unicode.IsControl(r) && !unicode.IsSpace(r)) {
			return false
		}
	}
	parsed, err := url.Parse(inspected)
	if err != nil {
		return false
	}
	return parsed.Scheme == "" && parsed.Host == ""
}
