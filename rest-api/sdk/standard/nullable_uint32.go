// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package standard

import "encoding/json"

// NullableUint32 preserves the distinction between omitted, null, and set values.
type NullableUint32 struct {
	value *uint32
	isSet bool
}

// Get returns the stored value.
func (v NullableUint32) Get() *uint32 {
	return v.value
}

// Set stores val, including an explicit null.
func (v *NullableUint32) Set(val *uint32) {
	v.value = val
	v.isSet = true
}

// IsSet reports whether a value or explicit null was provided.
func (v NullableUint32) IsSet() bool {
	return v.isSet
}

// Unset removes the value and its presence marker.
func (v *NullableUint32) Unset() {
	v.value = nil
	v.isSet = false
}

// NewNullableUint32 returns a set nullable value.
func NewNullableUint32(val *uint32) *NullableUint32 {
	return &NullableUint32{value: val, isSet: true}
}

// MarshalJSON encodes the stored value or null.
func (v NullableUint32) MarshalJSON() ([]byte, error) {
	return json.Marshal(v.value)
}

// UnmarshalJSON records presence and decodes a value or null.
func (v *NullableUint32) UnmarshalJSON(src []byte) error {
	v.isSet = true
	return json.Unmarshal(src, &v.value)
}
