// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"net/netip"
	"slices"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
)

const (
	// SiteExplorerEndpointActionClearError clears the endpoint's last exploration error.
	SiteExplorerEndpointActionClearError = "ClearError"
	// SiteExplorerEndpointActionReExplore schedules endpoint re-exploration.
	SiteExplorerEndpointActionReExplore = "ReExplore"

	// SiteExplorerEndpointTargetAll targets every explored endpoint at the site.
	SiteExplorerEndpointTargetAll = "All"
	// SiteExplorerEndpointTargetEndpointIDs targets the explicit endpointIds list.
	SiteExplorerEndpointTargetEndpointIDs = "EndpointIds"
)

// APISiteExplorerEndpointActionRequest triggers a site-explorer action for explored endpoints.
type APISiteExplorerEndpointActionRequest struct {
	// SiteID is the ID of the Site whose explored endpoints are targeted.
	SiteID string `json:"siteId"`
	// Action selects the site-explorer operation to run: "ClearError" or "ReExplore".
	Action string `json:"action"`
	// Target selects the endpoint set: "All" or "EndpointIds".
	Target string `json:"target"`
	// EndpointIDs is required when target is "EndpointIds"; endpoint IDs are BMC IP addresses.
	EndpointIDs []string `json:"endpointIds,omitempty"`
}

// APISiteExplorerEndpointAction is the accepted site-explorer action response.
type APISiteExplorerEndpointAction struct {
	// SiteID is the ID of the Site whose explored endpoints were targeted.
	SiteID string `json:"siteId"`
	// Action is the site-explorer operation that completed.
	Action string `json:"action"`
	// Target is the endpoint set that was selected.
	Target string `json:"target"`
	// EndpointIDs is the set of endpoint IDs that completed in Core.
	EndpointIDs []string `json:"endpointIds"`
}

// Validate checks the request shape before it is converted to Core protos.
func (r *APISiteExplorerEndpointActionRequest) Validate() error {
	if err := validation.ValidateStruct(r,
		validation.Field(&r.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&r.Action,
			validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&r.Target,
			validation.Required.Error(validationErrorValueRequired)),
	); err != nil {
		return err
	}

	switch r.Action {
	case SiteExplorerEndpointActionClearError, SiteExplorerEndpointActionReExplore:
	default:
		return fmt.Errorf("invalid action %q (expected %q or %q)", r.Action, SiteExplorerEndpointActionClearError, SiteExplorerEndpointActionReExplore)
	}

	switch r.Target {
	case SiteExplorerEndpointTargetAll:
		if len(r.EndpointIDs) > 0 {
			return fmt.Errorf("endpointIds must be empty when target is %q", SiteExplorerEndpointTargetAll)
		}
	case SiteExplorerEndpointTargetEndpointIDs:
		if len(r.EndpointIDs) == 0 {
			return fmt.Errorf("endpointIds is required when target is %q", SiteExplorerEndpointTargetEndpointIDs)
		}
		for _, endpointID := range r.EndpointIDs {
			if _, err := netip.ParseAddr(endpointID); err != nil {
				return fmt.Errorf("invalid endpointId %q: %s", endpointID, validationErrorInvalidIPAddress)
			}
		}
	default:
		return fmt.Errorf("invalid target %q (expected %q or %q)", r.Target, SiteExplorerEndpointTargetAll, SiteExplorerEndpointTargetEndpointIDs)
	}

	return nil
}

// ToResponse returns the completed action without Core transport details.
func (r *APISiteExplorerEndpointActionRequest) ToResponse(endpointIDs []string) *APISiteExplorerEndpointAction {
	respEndpointIDs := slices.Clone(endpointIDs)
	if respEndpointIDs == nil {
		respEndpointIDs = []string{}
	}
	return &APISiteExplorerEndpointAction{
		SiteID:      r.SiteID,
		Action:      r.Action,
		Target:      r.Target,
		EndpointIDs: respEndpointIDs,
	}
}
