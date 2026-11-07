// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package devicetypes

import "strings"

type BMCType int

const (
	BMCTypeUnknown BMCType = iota
	BMCTypeHost
	BMCTypeDPU
)

var (
	bmcTypeStrings = map[BMCType]string{
		BMCTypeUnknown: "Unknown",
		BMCTypeHost:    "Host",
		BMCTypeDPU:     "DPU",
	}

	bmcTypeStringMaxLen int
)

func init() {
	for _, str := range bmcTypeStrings {
		if len(str) > bmcTypeStringMaxLen {
			bmcTypeStringMaxLen = len(str)
		}
	}
}

// BMCTypes returns all the supported BMC types
func BMCTypes() []BMCType {
	return []BMCType{
		BMCTypeUnknown,
		BMCTypeHost,
		BMCTypeDPU,
	}
}

// BMCTypeFromString returns the BMC type from the given string.
func BMCTypeFromString(str string) BMCType {
	for bt, bmcTypeStr := range bmcTypeStrings {
		if strings.EqualFold(str, bmcTypeStr) {
			return bt
		}
	}
	return BMCTypeUnknown
}

// BMCTypeToString returns the string representation for the given BMC type.
func BMCTypeToString(bt BMCType) string {
	return bmcTypeStrings[bt]
}

func IsValidBMCTypeString(str string) bool {
	return BMCTypeFromString(str) != BMCTypeUnknown
}

// String return the aligned string representation for the given BMC type
func (bt BMCType) String() string {
	spaces := bmcTypeStringMaxLen - len(BMCTypeToString(bt))
	return strings.Repeat(" ", spaces) + BMCTypeToString(bt)
}
