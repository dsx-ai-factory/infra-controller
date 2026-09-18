// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package vpcprefix

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

// TestIPFamily_MaximumPrefixLength verifies each family and address mode uses
// the limit required by Core's Interface prefix allocation rules.
func TestIPFamily_MaximumPrefixLength(t *testing.T) {
	tests := []struct {
		name         string
		family       IPFamily
		slaacEnabled bool
		wantMaximum  int
		wantKnown    bool
	}{
		{
			name:         "IPv4 keeps the /31 exception when SLAAC is enabled",
			family:       IPFamilyIPv4,
			slaacEnabled: true,
			wantMaximum:  IPv4PrefixLengthMaximum,
			wantKnown:    true,
		},
		{
			name:        "stateful IPv6 leaves room for a /127 child",
			family:      IPFamilyIPv6,
			wantMaximum: IPv6StatefulPrefixLengthMaximum,
			wantKnown:   true,
		},
		{
			name:         "SLAAC IPv6 leaves room for a /64 child",
			family:       IPFamilyIPv6,
			slaacEnabled: true,
			wantMaximum:  IPv6SLAACPrefixLengthMaximum,
			wantKnown:    true,
		},
		{
			name:        "unknown family uses the structural request limit",
			family:      IPFamily("unknown"),
			wantMaximum: PrefixLengthMaximum,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			maximum, known := test.family.MaximumPrefixLength(test.slaacEnabled)
			assert.Equal(t, test.wantMaximum, maximum)
			assert.Equal(t, test.wantKnown, known)
		})
	}
}
