// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"strconv"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"google.golang.org/protobuf/encoding/protojson"
)

var protoJsonUnmarshalOptions = protojson.UnmarshalOptions{
	AllowPartial:   true,
	DiscardUnknown: true,
}

// Labels is the canonical entity-side representation of a workflow
// `Metadata.Labels` list — a key/value map that can be round-tripped
// through `ToProto` / `FromProtoMetadata` without losing the empty
// vs. nil distinction. Defining a named type lets us hang the proto
// conversion on it as a method (`labels.ToProto()`) rather than
// keeping a free function in this package.
type Labels map[string]string

const (
	// LabelKeyOrderByDefault is the default ordering field for label-key lists.
	LabelKeyOrderByDefault = "key"
	// LabelValueOrderByDefault is the default ordering field for label-value lists.
	LabelValueOrderByDefault = "value"
)

var (
	// LabelKeyOrderByFields contains the supported label-key ordering fields.
	LabelKeyOrderByFields = []string{LabelKeyOrderByDefault}
	// LabelValueOrderByFields contains the supported label-value ordering fields.
	LabelValueOrderByFields = []string{LabelValueOrderByDefault}
)

// ToProto converts the labels into the workflow proto repeated Label
// representation. Returns nil for a nil map; an empty map yields a
// non-nil empty slice so callers can distinguish "labels explicitly
// cleared" from "no labels at all".
func (l Labels) ToProto() []*corev1.Label {
	if l == nil {
		return nil
	}
	protoLabels := make([]*corev1.Label, 0, len(l))
	for k, v := range l {
		protoLabels = append(protoLabels, &corev1.Label{
			Key:   k,
			Value: &v,
		})
	}
	return protoLabels
}

// FromProto populates the receiver from a workflow proto repeated Label
// representation, mirroring `(Labels).ToProto()`. A nil input clears the
// receiver to nil; a non-nil but empty input yields a non-nil empty map,
// so callers can distinguish "no labels reported" from "labels explicitly
// cleared". Entries with an empty key are skipped; a label with a nil
// value resolves to an empty string.
func (l *Labels) FromProto(protoLabels []*corev1.Label) {
	if protoLabels == nil {
		*l = nil
		return
	}
	result := Labels{}
	for _, label := range protoLabels {
		if label == nil || label.Key == "" {
			continue
		}
		value := ""
		if label.Value != nil {
			value = *label.Value
		}
		result[label.Key] = value
	}
	*l = result
}

// Expected-inventory metadata label keys. These mirror the flat source field
// names from the REST API write path and are emitted as-is into Core's Metadata
// labels, so Core persists the full inventory dataset and Flow can read it back
// and structure it as it sees fit.
//
// TODO: replace this flat label passthrough with a structured InventoryData
// type (device + rack-position fields) carried explicitly, rather than packing
// the values into stringly-typed labels. Follow-up PR.
const (
	ExpectedComponentLabelManufacturer = "manufacturer"
	ExpectedComponentLabelModel        = "model"
	ExpectedComponentLabelSlotID       = "slot_id"
	ExpectedComponentLabelTrayIdx      = "tray_idx"
	ExpectedComponentLabelHostID       = "host_id"
)

// expectedComponentLabelsInput carries the flat device/rack columns that its
// ToProto method maps into an expected component's Metadata labels. Grouping
// them into a struct keeps the call sites self-documenting and avoids a long,
// transposition-prone arg list.
type expectedComponentLabelsInput struct {
	Manufacturer *string
	Model        *string
	SlotID       *int32
	TrayIdx      *int32
	HostID       *int32
	Labels       Labels
}

// ToProto merges the input's labels with the flat device/rack columns, each
// emitted as a label keyed by its source field name (see the
// ExpectedComponentLabel* constants). A system field takes precedence over a
// colliding user label, since the field value is the authoritative inventory
// data. Returns nil when there are no labels at all. Name and description are
// not labels -- callers set those on Metadata directly.
func (in expectedComponentLabelsInput) ToProto() []*corev1.Label {
	// Merge through a map so a system field overrides a colliding user label.
	merged := make(map[string]string, len(in.Labels)+5)
	for k, v := range in.Labels {
		merged[k] = v
	}
	if in.Manufacturer != nil {
		merged[ExpectedComponentLabelManufacturer] = *in.Manufacturer
	}
	if in.Model != nil {
		merged[ExpectedComponentLabelModel] = *in.Model
	}
	if in.SlotID != nil {
		merged[ExpectedComponentLabelSlotID] = strconv.FormatInt(int64(*in.SlotID), 10)
	}
	if in.TrayIdx != nil {
		merged[ExpectedComponentLabelTrayIdx] = strconv.FormatInt(int64(*in.TrayIdx), 10)
	}
	if in.HostID != nil {
		merged[ExpectedComponentLabelHostID] = strconv.FormatInt(int64(*in.HostID), 10)
	}
	if len(merged) == 0 {
		return nil
	}
	return Labels(merged).ToProto()
}
