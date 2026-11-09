// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package task

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// TestFirmwareRequestedTargets_FromTargetVersion protects display metadata without introducing
// validation rules for the backend's original firmware input.
func TestFirmwareRequestedTargets_FromTargetVersion(t *testing.T) {
	for _, tc := range []struct {
		name string
		raw  string
		want FirmwareRequestedTargets
	}{
		// Inspection must not rewrite the stored bytes returned for a safe version.
		{
			name: "scalar version bytes remain exact",
			raw:  " 2.0.0 ",
			want: FirmwareRequestedTargets{
				Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetVersion, Value: " 2.0.0 "}}},
		// Only an actually empty stored value requests the execution-time default.
		{
			name: "whitespace does not become configured default",
			raw:  " \t ",
			want: FirmwareRequestedTargets{
				Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetVersion, Value: " \t "}}},
		// A present Id owns precedence even when it supplies no usable identity.
		{
			name: "present empty Id blocks object_id fallback",
			raw:  `{"Id":"","object_id":"ignored","Locations":["https://artifact/private-target-data"],"auth":"private-target-data"}`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// The three supported types keep their distinct target forms in one input.
		{
			name: "per type empty differs from missing and scalar is unquoted",
			raw:  `{"compute":"","nvswitch":{"object_id":"switch-fw","auth":"private-target-data"},"powershelf":"r1.3.9"}`,
			want: FirmwareRequestedTargets{ByType: map[devicetypes.ComponentType]FirmwareTargetMetadata{
				devicetypes.ComponentTypeCompute:    {Kind: FirmwareTargetConfiguredDefault},
				devicetypes.ComponentTypeNVSwitch:   {Kind: FirmwareTargetManifest, Value: "switch-fw"},
				devicetypes.ComponentTypePowerShelf: {Kind: FirmwareTargetVersion, Value: "r1.3.9"},
			}}},
		// Do not synthesize a default for a section the task never supplied.
		{
			name: "missing per type input has no metadata",
			raw:  `{"nvswitch":{"Id":"switch-fw"}}`,
			want: FirmwareRequestedTargets{ByType: map[devicetypes.ComponentType]FirmwareTargetMetadata{
				devicetypes.ComponentTypeNVSwitch: {Kind: FirmwareTargetManifest, Value: "switch-fw"},
			}}},
		// Dispatch forwards null literally; this reader does not resolve backend meaning.
		{
			name: "per type null stays literal rather than default",
			raw:  `{"compute":null}`,
			want: FirmwareRequestedTargets{ByType: map[devicetypes.ComponentType]FirmwareTargetMetadata{
				devicetypes.ComponentTypeCompute: {Kind: FirmwareTargetVersion, Value: "null"},
			}}},
		// Only the workflow's exact reserved keys switch away from shared input.
		{
			name: "differently cased key is not a per type selection",
			raw:  `{"Compute":"private-target-data","Id":"fw-safe"}`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetManifest, Value: "fw-safe"}}},
		// Uninterpretable metadata must not copy other manifest members.
		{
			name: "SOT without identity remains unavailable",
			raw:  `{"ProductName":"private-target-data","Milestones":[]}`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Backend version maps are private input rather than a displayable identity.
		{
			name: "legacy map is not copied as public metadata",
			raw:  `{"bmc":"7.10.30","uefi":"2.22.1","private-target-data":"https://artifact/private-target-data"}`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Malformed structured text cannot become an opaque public version.
		{
			name: "unrecognized inline syntax is not default or scalar",
			raw:  `{"compute":"private-target-data","unfinished":`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Safety inspection may unquote text but must still reject nested payloads.
		{
			name: "quoted inline payload is not a display version",
			raw:  `"{\"auth\":\"private-target-data\"}"`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Requested input can contain credentials as part of an artifact location.
		{
			name: "scalar artifact URI is unavailable",
			raw:  "https://user:private-target-data@artifact/file",
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// A location does not need an explicit URI scheme.
		{
			name: "network path artifact URL is unavailable",
			raw:  "//artifact/private-target-data",
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Manifest identities use the same privacy rule as scalar input.
		{
			name: "identity cannot expose an artifact URI",
			raw:  `{"Id":"https://artifact/private-target-data"}`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Per-type string unwrapping must not expose whitespace-prefixed JSON.
		{
			name: "layered JSON payload is unavailable",
			raw:  `{"compute":" [\"private-target-data\"]"}`,
			want: FirmwareRequestedTargets{ByType: map[devicetypes.ComponentType]FirmwareTargetMetadata{
				devicetypes.ComponentTypeCompute: {Kind: FirmwareTargetUnavailable},
			}}},
		// A prefix must not turn a private inline object into an opaque version.
		{
			name: "BOM prefixed manifest remains unavailable",
			raw:  "\ufeff" + `{"Id":"fw-safe","auth":"private-target-data"}`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// A failed URL parse must not bypass artifact privacy.
		{
			name: "malformed URI remains unavailable",
			raw:  "s3:/bucket/private-target-data%zz?sig=private-target-data",
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Locations can carry signed query data without a URL scheme.
		{
			name: "schemeless artifact location remains unavailable",
			raw:  "artifacts.example/fw?token=private-target-data",
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// This URI has no path delimiter, so the scheme check must reject it.
		{
			name: "scheme without slashes remains unavailable",
			raw:  "mailto:private-target-data",
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetUnavailable}}},
		// Shared strings keep their quotes; only per-type strings are unwrapped by dispatch.
		{
			name: "quoted shared scalar keeps dispatch bytes",
			raw:  `"2.0.0"`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetVersion, Value: `"2.0.0"`}}},
		// Struct-tag decoding would incorrectly accept ID instead of the exact Id key.
		{
			name: "identity keys are case sensitive",
			raw:  `{"ID":"private-target-data","object_id":"fw-safe"}`,
			want: FirmwareRequestedTargets{Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetManifest, Value: "fw-safe"}}},
		// Returning to the default form must clear every earlier value.
		{
			name: "only empty input means configured default",
			want: FirmwareRequestedTargets{
				Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetConfiguredDefault}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			// Each subtest proves replacement independently, including when run alone.
			got := FirmwareRequestedTargets{
				Shared: &FirmwareTargetMetadata{Kind: FirmwareTargetVersion, Value: "stale"},
				ByType: map[devicetypes.ComponentType]FirmwareTargetMetadata{
					devicetypes.ComponentTypeCompute: {Kind: FirmwareTargetVersion, Value: "stale"},
				},
			}

			got.FromTargetVersion(tc.raw)

			assert.Equal(t, tc.want, got)
			encoded, err := json.Marshal(got)
			require.NoError(t, err)
			assert.NotContains(t, string(encoded), "private-target-data")
		})
	}
}
