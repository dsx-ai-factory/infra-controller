// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// GetStrPtr returns a pointer to the string passed in
func GetStrPtr(s string) *string {
	return &s
}

func ProtobufUUIDListToStringList(ids []*corev1.UUID) []string {
	s := make([]string, len(ids))

	for i, u := range ids {
		if u == nil {
			s[i] = ""
		} else {
			s[i] = u.Value
		}
	}

	return s
}

func StringsToProtobufUUIDList(ids []string) []*corev1.UUID {
	s := make([]*corev1.UUID, len(ids))

	for i, u := range ids {
		s[i] = &corev1.UUID{Value: u}
	}

	return s
}
