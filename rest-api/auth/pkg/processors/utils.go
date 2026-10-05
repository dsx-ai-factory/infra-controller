// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
	"encoding/base64"
	"net/http"
	"sort"
	"strings"
	"time"

	"github.com/dsx-ai-factory/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/dsx-ai-factory/infra-controller/rest-api/db/pkg/db/model"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
)

const (
	// NGC KAS Headers
	// Kas v2 NGC team header
	ngcTeamHeader = "NV-Ngc-Team"
	// Kas v2 NGC roles header
	ngcRolesHeader = "NV-Ngc-User-Roles"
	// Kas v2 NGC org display name header
	ngcOrgDisplayNameHeader = "NV-Ngc-Org-Display-Name"
	// Kas v2 NGC user name header
	ngcUserNameHeader = "NV-Ngc-User-Name"
	// Kas v2 NGC user email header
	ngcUserEmailHeader = "X-Ngc-Email-Id"

	// OrgDataStalePeriod is the duration after which an org's Updated field is considered stale
	OrgDataStalePeriod = time.Minute
)

// GetUserWithUpdatedOrgData merges the requested org from tokenOrgData into the existing user's OrgData.
// It only updates the specific org from the request, preserving other orgs.
// Returns a partial User with updated OrgData if update is needed, or nil if no update needed.
// Returns an error if the requested org is not found in token claims.
//
// Update is needed if:
// - Requested org doesn't exist in user's OrgData
// - Requested org data has changed
// - Requested org's Updated field is nil or stale (> OrgDataStalePeriod)
func GetUserWithUpdatedOrgData(existingUser cdbm.User, tokenOrgData cdbm.OrgData, reqOrgName string, logger zerolog.Logger) (*cdbm.User, *util.APIError) {
	// Start with existing org data
	mergedOrgData := existingUser.OrgData
	if mergedOrgData == nil {
		mergedOrgData = cdbm.OrgData{}
	}

	// Get the org from token claims for the requested org
	reqOrg, hasReqOrg := tokenOrgData[reqOrgName]
	if !hasReqOrg {
		logger.Warn().Str("requested_org", reqOrgName).Msg("Requested org not found in token claims")
		return nil, util.NewAPIError(http.StatusForbidden, "Requested organization not found in token claims", nil)
	}

	// Check if update is needed
	existingOrg, existsInDB := mergedOrgData[reqOrgName]
	isStale := !existsInDB || existingOrg.Updated == nil || time.Since(*existingOrg.Updated) > OrgDataStalePeriod
	needsUpdate := isStale || !existsInDB || !existingOrg.Equal(reqOrg)

	if !needsUpdate {
		return nil, nil
	}

	// Set the Updated timestamp on the requested org
	now := time.Now().UTC()
	reqOrg.Updated = &now
	mergedOrgData[reqOrgName] = reqOrg

	logger.Info().Str("org", reqOrgName).Msg("updating user org data")

	return &cdbm.User{
		OrgData: mergedOrgData,
	}, nil
}

// GetUpdatedUserFromHeaders extracts user information from headers sent by KAS
// Steps include
// 1. Extract NGC user name and email from headers
// 2. Extract NGC roles from headers
// 3. Extract NGC org display name from headers
// 4. Update user record if necessary
// 5. Return updated user record
// Returns updated user record and API error if any
func GetUpdatedUserFromHeaders(c echo.Context, existingUser cdbm.User, ngcOrgName string, logger zerolog.Logger) (*cdbm.User, *util.APIError) {
	// Update user record if necessary
	isUserUpdated := false
	updatedUser := &cdbm.User{}

	// Extract NGC user name
	ngcUserNameB64 := c.Request().Header.Get(ngcUserNameHeader)
	if ngcUserNameB64 != "" {
		// NGC User Name is base64 encoded, decode it
		decodedBytes, err := base64.StdEncoding.DecodeString(ngcUserNameB64)
		if err != nil {
			logger.Warn().Err(err).Msg("failed to decode NGC user name header, invalid base64 value")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid value in NGC org user name header", nil)
		}
		ngcUserName := string(decodedBytes)
		nameComps := strings.SplitN(ngcUserName, " ", 2)
		if len(nameComps) > 0 && nameComps[0] != "" && (existingUser.FirstName == nil || *existingUser.FirstName != nameComps[0]) {
			updatedUser.FirstName = &nameComps[0]
			isUserUpdated = true
		}
		if len(nameComps) > 1 && nameComps[1] != "" && (existingUser.LastName == nil || *existingUser.LastName != nameComps[1]) {
			updatedUser.LastName = &nameComps[1]
			isUserUpdated = true
		}
	} else {
		logger.Warn().Msg("request received without NGC user name header, first/last name may not be available for user")
	}

	// Extract NGC user email
	ngcUserEmailB64 := c.Request().Header.Get(ngcUserEmailHeader)
	if ngcUserEmailB64 != "" {
		// NGC User Email is base64 encoded, decode it
		decodedBytes, err := base64.StdEncoding.DecodeString(ngcUserEmailB64)
		if err != nil {
			logger.Warn().Err(err).Msg("failed to decode NGC user email header, invalid base64 value")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid value in NGC org user email header", nil)
		}
		ngcUserEmail := string(decodedBytes)
		if existingUser.Email == nil || *existingUser.Email != ngcUserEmail {
			updatedUser.Email = &ngcUserEmail
			isUserUpdated = true
		}
	} else {
		logger.Warn().Msg("request received without NGC user email header, email may not be available for user")
	}

	// Extract NGC roles
	ngcRolesValue := c.Request().Header.Get(ngcRolesHeader)
	if ngcRolesValue == "" {
		logger.Warn().Msg("request received without NGC roles header, access denied")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Request is missing NGC roles header", nil)
	}

	ngcRoles := strings.Split(ngcRolesValue, ",")
	newNgcRoles := []string{}
	// Format roles
	for _, role := range ngcRoles {
		curRole := strings.ReplaceAll(role, "-", "_")
		curRole = strings.ToUpper(curRole)
		newNgcRoles = append(newNgcRoles, curRole)
	}
	sort.Strings(newNgcRoles)

	orgData := existingUser.OrgData
	if orgData == nil {
		orgData = cdbm.OrgData{}
	}

	// Extract NGC org display name
	var ngcOrgDisplayName string

	ngcOrgDisplayNameB64 := c.Request().Header.Get(ngcOrgDisplayNameHeader)
	if ngcOrgDisplayNameB64 == "" {
		logger.Warn().Msg("request received without NGC org display name header, access denied")
		return nil, util.NewAPIError(http.StatusUnauthorized, "Request is missing NGC org display name header", nil)
	} else {
		// NGC Org Display Name is base64 encoded, decode it
		decodedBytes, err := base64.StdEncoding.DecodeString(ngcOrgDisplayNameB64)
		if err != nil {
			logger.Error().Err(err).Msg("failed to decode NGC org display name header")
			return nil, util.NewAPIError(http.StatusUnauthorized, "Invalid value in NGC org display name header", nil)
		}
		ngcOrgDisplayName = string(decodedBytes)
	}

	ngcOrg, err := existingUser.OrgData.GetOrgByName(ngcOrgName)
	if err != nil {
		// Org not found, create new
		now := time.Now().UTC()
		ngcOrg = &cdbm.Org{
			Name:        ngcOrgName,
			DisplayName: ngcOrgDisplayName,
			Roles:       newNgcRoles,
			Teams:       []cdbm.Team{},
			Updated:     &now,
		}
		orgData[ngcOrgName] = *ngcOrg
		updatedUser.OrgData = orgData
		isUserUpdated = true
	} else {
		// Check if user has any role changes
		updateRoles := len(ngcOrg.Roles) != len(newNgcRoles)
		if !updateRoles {
			existingRoleMap := map[string]bool{}
			for _, role := range ngcOrg.Roles {
				existingRoleMap[role] = true
			}
			for _, role := range newNgcRoles {
				_, found := existingRoleMap[role]
				if !found {
					updateRoles = true
					break
				}
			}
		}
		if updateRoles {
			now := time.Now().UTC()
			ngcOrg.Roles = newNgcRoles
			ngcOrg.Updated = &now
			orgData[ngcOrgName] = *ngcOrg
			updatedUser.OrgData = orgData
			isUserUpdated = true
		}

		if ngcOrg.DisplayName != ngcOrgDisplayName {
			now := time.Now().UTC()
			ngcOrg.DisplayName = ngcOrgDisplayName
			ngcOrg.Updated = &now
			orgData[ngcOrgName] = *ngcOrg
			updatedUser.OrgData = orgData
			isUserUpdated = true
		}
	}

	if isUserUpdated {
		return updatedUser, nil
	} else {
		return nil, nil
	}
}
