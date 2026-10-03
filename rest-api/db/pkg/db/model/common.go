// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"strconv"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
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

// expectedComponentMetadata maps the shared inventory columns to Core Metadata.
// Reserved labels with valid values become dedicated inventory columns.
type expectedComponentMetadata struct {
	Name         *string
	Description  *string
	Manufacturer *string
	Model        *string
	SlotID       *int32
	TrayIdx      *int32
	HostID       *int32
	Labels       Labels
}

// ToProto merges user labels with the device and rack fields. Dedicated fields
// take precedence over colliding user labels.
func (in expectedComponentMetadata) ToProto() *corev1.Metadata {
	merged := make(Labels, len(in.Labels)+5)
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
		merged = nil
	}
	return &corev1.Metadata{
		Name:        cutil.GetValueOrZero(in.Name),
		Description: cutil.GetValueOrZero(in.Description),
		Labels:      merged.ToProto(),
	}
}

// FromProto replaces the inventory fields with Core's Metadata. Missing or empty
// strings and missing or invalid int32 labels clear their optional fields.
// Preserve labels we can't represent so later writes don't drop their values.
// Remaining labels follow Labels.FromProto's nil and empty collection behavior.
func (in *expectedComponentMetadata) FromProto(metadata *corev1.Metadata) {
	in.Name = cutil.GetPtrIfNotZero(metadata.GetName())
	in.Description = cutil.GetPtrIfNotZero(metadata.GetDescription())
	in.Labels.FromProto(metadata.GetLabels())
	in.Manufacturer = cutil.GetPtrIfNotZero(in.Labels[ExpectedComponentLabelManufacturer])
	in.Model = cutil.GetPtrIfNotZero(in.Labels[ExpectedComponentLabelModel])
	if in.Manufacturer != nil {
		delete(in.Labels, ExpectedComponentLabelManufacturer)
	}
	if in.Model != nil {
		delete(in.Labels, ExpectedComponentLabelModel)
	}

	position := func(key string) *int32 {
		value, err := strconv.ParseInt(in.Labels[key], 10, 32)
		if err != nil {
			return nil
		}
		delete(in.Labels, key)
		return cutil.GetPtr(int32(value))
	}
	in.SlotID = position(ExpectedComponentLabelSlotID)
	in.TrayIdx = position(ExpectedComponentLabelTrayIdx)
	in.HostID = position(ExpectedComponentLabelHostID)
}
