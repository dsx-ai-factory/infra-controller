// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestAPIFactoryResetRequest_Validate(t *testing.T) {
	for _, tc := range []struct {
		name      string
		target    string
		wantError bool
	}{
		{name: "explicit NVOS scope", target: "nvos"},
		{name: "uppercase target rejected", target: "NVOS", wantError: true},
		{name: "missing target has no default", wantError: true},
		{name: "explicit SDN scope", target: "sdn"},
		{name: "unsupported BMC scope", target: "bmc", wantError: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			request := APIFactoryResetRequest{
				SiteID: "60189e9c-7d12-438c-b9ca-6998d9c364b1",
				Target: tc.target,
			}
			err := request.Validate()
			if tc.wantError {
				require.ErrorContains(t, err, "target")
				return
			}
			require.NoError(t, err)
		})
	}
}
