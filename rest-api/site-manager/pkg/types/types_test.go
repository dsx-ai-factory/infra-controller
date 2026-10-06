// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package types

import (
	"encoding/json"
	"testing"

	"github.com/stretchr/testify/require"
)

func TestSiteGetResponse(t *testing.T) {
	testCases := []struct {
		name     string
		response SiteGetResponse
		wantJSON string
	}{
		{
			name:     "empty fields remain present",
			response: SiteGetResponse{},
			wantJSON: `{
				"siteuuid": "",
				"name": "",
				"provider": "",
				"fcorg": "",
				"bootstrapstate": "",
				"controlplanestatus": "",
				"otp": "",
				"otpexpiry": ""
			}`,
		},
		{
			name: "populated fields retain their values",
			response: SiteGetResponse{
				SiteUUID:           "e89a0525-193e-4e25-b88e-96fd90d2357f",
				Name:               "test-site",
				Provider:           "test-provider",
				FCOrg:              "test-org",
				BootstrapState:     "AwaitHandshake",
				ControlPlaneStatus: "Ready",
				OTP:                "test-otp",
				OTPExpiry:          "2026-10-06 00:00:00 +0000 UTC",
			},
			wantJSON: `{
				"siteuuid": "e89a0525-193e-4e25-b88e-96fd90d2357f",
				"name": "test-site",
				"provider": "test-provider",
				"fcorg": "test-org",
				"bootstrapstate": "AwaitHandshake",
				"controlplanestatus": "Ready",
				"otp": "test-otp",
				"otpexpiry": "2026-10-06 00:00:00 +0000 UTC"
			}`,
		},
	}

	for _, tc := range testCases {
		t.Run(tc.name, func(t *testing.T) {
			data, err := json.Marshal(tc.response)
			require.NoError(t, err)
			require.JSONEq(t, tc.wantJSON, string(data))
		})
	}
}
