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

func TestExpectedComponentLabelsInput_ToProto(t *testing.T) {
	t.Run("merges user labels with the flat device fields", func(t *testing.T) {
		got := labelsAsMap(expectedComponentLabelsInput{
			Manufacturer: cutil.GetPtr("NVIDIA"),
			Model:        cutil.GetPtr("MGX"),
			SlotID:       cutil.GetPtr(int32(3)),
			TrayIdx:      cutil.GetPtr(int32(0)), // zero is a valid position
			HostID:       cutil.GetPtr(int32(1)),
			Labels:       Labels{"environment": "prod", "team": "infra"},
		}.ToProto())
		assert.Equal(t, Labels{
			"environment":  "prod",
			"team":         "infra",
			"manufacturer": "NVIDIA",
			"model":        "MGX",
			"slot_id":      "3",
			"tray_idx":     "0",
			"host_id":      "1",
		}, got)
	})

	t.Run("returns nil when there are no labels", func(t *testing.T) {
		assert.Nil(t, expectedComponentLabelsInput{}.ToProto())
	})

	t.Run("device fields only, no user labels", func(t *testing.T) {
		got := labelsAsMap(expectedComponentLabelsInput{
			Manufacturer: cutil.GetPtr("NVIDIA"),
			SlotID:       cutil.GetPtr(int32(0)),
		}.ToProto())
		assert.Equal(t, Labels{
			"manufacturer": "NVIDIA",
			"slot_id":      "0",
		}, got)
	})

	t.Run("system field wins over a conflicting user label", func(t *testing.T) {
		got := labelsAsMap(expectedComponentLabelsInput{
			Manufacturer: cutil.GetPtr("NVIDIA"),
			Labels:       Labels{"manufacturer": "user-supplied", "extra": "kept"},
		}.ToProto())
		assert.Equal(t, Labels{
			"manufacturer": "NVIDIA", // system value wins
			"extra":        "kept",   // non-conflicting user label preserved
		}, got)
	})
}
