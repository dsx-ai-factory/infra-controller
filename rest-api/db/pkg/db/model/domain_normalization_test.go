// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestNormalizeForwardDomainName(t *testing.T) {
	for input, expected := range map[string]string{
		"LAB.Example.":   "lab.example",
		"LAB.Example...": "lab.example",
		"lab.example":    "lab.example",
		"\tLAB.Example":  "\tlab.example", // invalid DNS input is rejected by Core, not silently sanitized
	} {
		t.Run(input, func(t *testing.T) {
			require.Equal(t, expected, NormalizeForwardDomainName(input))
		})
	}
}
