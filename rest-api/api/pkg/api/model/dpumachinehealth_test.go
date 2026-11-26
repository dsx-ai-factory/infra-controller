// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestAPIGetAllDpuMachineHealthReportRequest_Validate(t *testing.T) {
	tests := []struct {
		name    string
		siteID  string
		wantErr bool
	}{
		{name: "accepts a UUID Site ID", siteID: "5a0c3f0e-6b0e-4d7c-9a54-1f3b8d1c2e77"},
		{name: "requires a Site ID", wantErr: true},
		{name: "requires a UUID Site ID", siteID: "site-1", wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := APIGetAllDpuMachineHealthReportRequest{SiteID: tt.siteID}
			err := req.Validate()
			if !tt.wantErr {
				assert.NoError(t, err)
				return
			}
			var validationErrs validation.Errors
			require.ErrorAs(t, err, &validationErrs)
			// The DTO binds a query parameter and has no JSON name, so the
			// error is keyed by the Go field name.
			assert.Contains(t, validationErrs, "SiteID")
		})
	}
}
