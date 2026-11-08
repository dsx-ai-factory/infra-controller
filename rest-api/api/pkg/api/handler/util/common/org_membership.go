// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package common

import (
	"net/http"

	"github.com/rs/zerolog"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// OrgMembership holds the org details validated from the current request user.
// Its fields are private so callers cannot resolve roles for an unchecked org.
// Keep it within the request; it is not a cross-request authorization cache.
type OrgMembership struct {
	orgName string
	org     *cdbm.Org
}

// ValidateOrgMembership rejects non-members before handlers parse requests or
// load resources. The returned membership can resolve Provider/Tenant roles
// later, once any required resource-specific privilege scope is available.
func ValidateOrgMembership(logger zerolog.Logger, org string, user *cdbm.User) (OrgMembership, *cutil.APIError) {
	if user != nil {
		orgDetails, err := user.OrgData.GetOrgByName(org)
		if err == nil {
			return OrgMembership{orgName: org, org: orgDetails}, nil
		}
	}
	logger.Warn().Str("org", org).Msg("could not validate org membership for user, access denied")
	return OrgMembership{}, cutil.NewAPIError(http.StatusForbidden, "Failed to validate membership for org: "+org, nil)
}
