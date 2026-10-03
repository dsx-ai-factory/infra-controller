// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/assert"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestLabels_FromProto(t *testing.T) {
	tests := []struct {
		name        string
		protoLabels []*corev1.Label
		want        Labels
	}{
		{
			name:        "nil slice clears receiver",
			protoLabels: nil,
			want:        nil,
		},
		{
			name:        "empty slice yields empty map",
			protoLabels: []*corev1.Label{},
			want:        Labels{},
		},
		{
			name: "single label with value",
			protoLabels: []*corev1.Label{
				{Key: "environment", Value: cutil.GetPtr("production")},
			},
			want: Labels{"environment": "production"},
		},
		{
			name: "multiple labels",
			protoLabels: []*corev1.Label{
				{Key: "environment", Value: cutil.GetPtr("production")},
				{Key: "rack", Value: cutil.GetPtr("rack-1")},
				{Key: "datacenter", Value: cutil.GetPtr("dc1")},
			},
			want: Labels{
				"environment": "production",
				"rack":        "rack-1",
				"datacenter":  "dc1",
			},
		},
		{
			name: "label with nil value yields empty string",
			protoLabels: []*corev1.Label{
				{Key: "flag", Value: nil},
			},
			want: Labels{"flag": ""},
		},
		{
			name: "label with empty key is skipped",
			protoLabels: []*corev1.Label{
				{Key: "", Value: cutil.GetPtr("value")},
				{Key: "valid", Value: cutil.GetPtr("data")},
			},
			want: Labels{"valid": "data"},
		},
		{
			name: "nil label entry is skipped",
			protoLabels: []*corev1.Label{
				nil,
				{Key: "valid", Value: cutil.GetPtr("data")},
			},
			want: Labels{"valid": "data"},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			var got Labels
			got.FromProto(tc.protoLabels)
			if tc.want == nil {
				assert.Nil(t, got)
			} else {
				assert.Equal(t, tc.want, got)
			}
		})
	}
}

// TestLabels_FromProto_OverwritesExistingReceiver verifies that the
// method replaces the receiver wholesale, mirroring `ToProto` semantics:
// pre-existing entries are not preserved across calls. The pointer
// receiver makes the nil-input case observable (existing labels become
// nil), which mirrors how the workflow `Metadata.Labels` round-trips a
// "labels explicitly cleared" signal.
func TestLabels_FromProto_OverwritesExistingReceiver(t *testing.T) {
	t.Run("populated input replaces existing entries", func(t *testing.T) {
		l := Labels{"stale": "value", "kept-key": "old"}
		l.FromProto([]*corev1.Label{
			{Key: "kept-key", Value: cutil.GetPtr("new")},
			{Key: "fresh", Value: cutil.GetPtr("data")},
		})
		assert.Equal(t, Labels{"kept-key": "new", "fresh": "data"}, l)
	})

	t.Run("nil input clears existing entries", func(t *testing.T) {
		l := Labels{"stale": "value"}
		l.FromProto(nil)
		assert.Nil(t, l)
	})
}

// labelsAsMap collapses a proto Label slice into a Labels map so assertions
// don't depend on slice ordering (user labels come from a map iteration).
func labelsAsMap(protoLabels []*corev1.Label) Labels {
	var l Labels
	l.FromProto(protoLabels)
	return l
}

func TestExpectedComponentMetadata_ToProto(t *testing.T) {
	tests := []struct {
		name  string
		input expectedComponentMetadata
		want  *corev1.Metadata
	}{
		{
			name: "merges inventory fields with user labels",
			input: expectedComponentMetadata{
				Name:         cutil.GetPtr("machine-1"),
				Description:  cutil.GetPtr("primary"),
				Manufacturer: cutil.GetPtr("NVIDIA"),
				Model:        cutil.GetPtr("MGX"),
				SlotID:       cutil.GetPtr(int32(3)),
				TrayIdx:      cutil.GetPtr(int32(0)),
				HostID:       cutil.GetPtr(int32(1)),
				Labels:       Labels{"environment": "prod", "manufacturer": "user-supplied"},
			},
			want: &corev1.Metadata{
				Name:        "machine-1",
				Description: "primary",
				Labels: []*corev1.Label{
					{Key: "environment", Value: cutil.GetPtr("prod")},
					{Key: "manufacturer", Value: cutil.GetPtr("NVIDIA")},
					{Key: "model", Value: cutil.GetPtr("MGX")},
					{Key: "slot_id", Value: cutil.GetPtr("3")},
					{Key: "tray_idx", Value: cutil.GetPtr("0")},
					{Key: "host_id", Value: cutil.GetPtr("1")},
				},
			},
		},
		{
			name: "no inventory fields or labels",
			want: &corev1.Metadata{},
		},
		{
			name: "inventory fields without user labels",
			input: expectedComponentMetadata{
				Manufacturer: cutil.GetPtr("NVIDIA"),
				SlotID:       cutil.GetPtr(int32(0)),
			},
			want: &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "manufacturer", Value: cutil.GetPtr("NVIDIA")},
					{Key: "slot_id", Value: cutil.GetPtr("0")},
				},
			},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := tc.input.ToProto()
			assert.Equal(t, tc.want.Name, got.Name)
			assert.Equal(t, tc.want.Description, got.Description)
			assert.Equal(t, labelsAsMap(tc.want.Labels), labelsAsMap(got.Labels))
		})
	}
}

func TestExpectedComponentMetadata_FromProto(t *testing.T) {
	tests := []struct {
		name  string
		proto *corev1.Metadata
		want  expectedComponentMetadata
	}{
		{
			name: "missing metadata clears fields",
		},
		{
			name:  "empty metadata preserves an explicit empty label collection",
			proto: &corev1.Metadata{Labels: []*corev1.Label{}},
			want:  expectedComponentMetadata{Labels: Labels{}},
		},
		{
			name: "extracts reserved labels and preserves signed int32 boundaries",
			proto: &corev1.Metadata{
				Name:        "machine-1",
				Description: "primary",
				Labels: []*corev1.Label{
					{Key: "manufacturer", Value: cutil.GetPtr("NVIDIA")},
					{Key: "model", Value: cutil.GetPtr("MGX")},
					{Key: "slot_id", Value: cutil.GetPtr("2147483647")},
					{Key: "tray_idx", Value: cutil.GetPtr("0")},
					{Key: "host_id", Value: cutil.GetPtr("-2147483648")},
					{Key: "environment", Value: cutil.GetPtr("prod")},
				},
			},
			want: expectedComponentMetadata{
				Name:         cutil.GetPtr("machine-1"),
				Description:  cutil.GetPtr("primary"),
				Manufacturer: cutil.GetPtr("NVIDIA"),
				Model:        cutil.GetPtr("MGX"),
				SlotID:       cutil.GetPtr(int32(2147483647)),
				TrayIdx:      cutil.GetPtr(int32(0)),
				HostID:       cutil.GetPtr(int32(-2147483648)),
				Labels:       Labels{"environment": "prod"},
			},
		},
		{
			name: "invalid and empty values clear fields but preserve labels",
			proto: &corev1.Metadata{
				Labels: []*corev1.Label{
					{Key: "manufacturer", Value: cutil.GetPtr("")},
					{Key: "model"},
					{Key: "slot_id", Value: cutil.GetPtr("not-a-number")},
					{Key: "tray_idx", Value: cutil.GetPtr("2147483648")},
					{Key: "host_id", Value: cutil.GetPtr("-2147483649")},
				},
			},
			want: expectedComponentMetadata{Labels: Labels{
				"manufacturer": "",
				"model":        "",
				"slot_id":      "not-a-number",
				"tray_idx":     "2147483648",
				"host_id":      "-2147483649",
			}},
		},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := expectedComponentMetadata{
				Name:         cutil.GetPtr("stale"),
				Description:  cutil.GetPtr("stale"),
				Manufacturer: cutil.GetPtr("stale"),
				Model:        cutil.GetPtr("stale"),
				SlotID:       cutil.GetPtr(int32(9)),
				TrayIdx:      cutil.GetPtr(int32(9)),
				HostID:       cutil.GetPtr(int32(9)),
				Labels:       Labels{"stale": "value"},
			}
			got.FromProto(tc.proto)
			assert.Equal(t, tc.want, got)
			if len(tc.proto.GetLabels()) > 0 {
				assert.Equal(t, labelsAsMap(tc.proto.Labels), labelsAsMap(got.ToProto().Labels))
			}
		})
	}
}
