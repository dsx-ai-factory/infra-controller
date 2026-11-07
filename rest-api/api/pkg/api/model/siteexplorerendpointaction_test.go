// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPISiteExplorerEndpointActionRequestValidate(t *testing.T) {
	siteID := uuid.NewString()
	tests := []struct {
		name    string
		req     APISiteExplorerEndpointActionRequest
		wantErr bool
	}{
		{name: "all clear-error", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: SiteExplorerEndpointActionClearError, Target: SiteExplorerEndpointTargetAll}},
		{name: "selected re-explore", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: SiteExplorerEndpointActionReExplore, Target: SiteExplorerEndpointTargetEndpointIDs, EndpointIDs: []string{"10.0.0.1"}}},
		{name: "missing site ID", req: APISiteExplorerEndpointActionRequest{Action: SiteExplorerEndpointActionClearError, Target: SiteExplorerEndpointTargetAll}, wantErr: true},
		{name: "invalid site ID", req: APISiteExplorerEndpointActionRequest{SiteID: "bad-site-id", Action: SiteExplorerEndpointActionClearError, Target: SiteExplorerEndpointTargetAll}, wantErr: true},
		{name: "missing action", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Target: SiteExplorerEndpointTargetAll}, wantErr: true},
		{name: "invalid action", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: "clear-error", Target: SiteExplorerEndpointTargetAll}, wantErr: true},
		{name: "missing target", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: SiteExplorerEndpointActionClearError}, wantErr: true},
		{name: "invalid target", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: SiteExplorerEndpointActionClearError, Target: "all"}, wantErr: true},
		{name: "all with endpoint IDs", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: SiteExplorerEndpointActionClearError, Target: SiteExplorerEndpointTargetAll, EndpointIDs: []string{"10.0.0.1"}}, wantErr: true},
		{name: "selected without endpoint IDs", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: SiteExplorerEndpointActionClearError, Target: SiteExplorerEndpointTargetEndpointIDs}, wantErr: true},
		{name: "selected invalid endpoint ID", req: APISiteExplorerEndpointActionRequest{SiteID: siteID, Action: SiteExplorerEndpointActionClearError, Target: SiteExplorerEndpointTargetEndpointIDs, EndpointIDs: []string{"not-an-ip"}}, wantErr: true},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := tt.req.Validate()
			if tt.wantErr {
				assert.Error(t, err)
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestAPISiteExplorerEndpointActionRequestToResponse(t *testing.T) {
	req := APISiteExplorerEndpointActionRequest{
		SiteID: uuid.NewString(),
		Action: SiteExplorerEndpointActionClearError,
		Target: SiteExplorerEndpointTargetEndpointIDs,
	}

	endpointIDs := []string{"10.0.0.1"}
	resp := req.ToResponse(endpointIDs)
	endpointIDs[0] = "10.0.0.2"
	assert.Equal(t, []string{"10.0.0.1"}, resp.EndpointIDs)
}
