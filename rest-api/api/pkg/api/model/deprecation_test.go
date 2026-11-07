// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
)

func TestNewAPIDeprecation(t *testing.T) {
	oldValue := "blockSize"
	newValue := "prefixLength"

	preEffectTime := time.Now().Add(24 * time.Hour)
	postEffectTime := time.Now().Add(-24 * time.Hour)

	tests := []struct {
		name         string
		oldValue     string
		newValue     string
		fieldType    string
		takeActionBy time.Time
		expect       APIDeprecation
	}{
		{
			name:         "test new API deprecation - pre-deprecation notice",
			oldValue:     oldValue,
			newValue:     newValue,
			fieldType:    DeprecationTypeAttribute,
			takeActionBy: preEffectTime,
			expect: APIDeprecation{
				Notice:       fmt.Sprintf(deprecationPreTemplate, oldValue, fmt.Sprintf(" in favor of '%s'", newValue)),
				Attribute:    &oldValue,
				TakeActionBy: preEffectTime,
				ReplacedBy:   &newValue,
			},
		},
		{
			name:         "test new API deprecation - post-deprecation notice",
			oldValue:     oldValue,
			newValue:     newValue,
			fieldType:    DeprecationTypeAttribute,
			takeActionBy: postEffectTime,
			expect: APIDeprecation{
				Notice:       fmt.Sprintf(deprecationPostTemplate, oldValue, fmt.Sprintf(" in favor of '%s'", newValue)),
				Attribute:    &oldValue,
				TakeActionBy: postEffectTime,
				ReplacedBy:   &newValue,
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := NewAPIDeprecation(DeprecatedEntity{
				OldValue:     tt.oldValue,
				NewValue:     &tt.newValue,
				Type:         tt.fieldType,
				TakeActionBy: tt.takeActionBy,
			})
			assert.EqualValues(t, tt.expect, got)
		})
	}
}
