// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package claim

import (
	"strings"

	"github.com/golang-jwt/jwt/v5"
)

const (
	// NgcOrgClaimTypePrefix is the prefix for access claim that contains NGC organization name
	// e.g. Staging: "group/ngc-stg", Production: "group/ngc"
	NgcOrgClaimTypePrefix = "group/ngc"
	// NgcAudience describes the audience value present in NGC tokens
	NgcAudience = "ngc"

	// SsaScopeKas is the scope required to access KAS
	SsaScopeKas = "kas"
)

// NgcAccessClaim represent the custom NGC KAS access claims
type NgcAccessClaim struct {
	Type    string   `json:"type"`
	Name    string   `json:"name"`
	Actions []string `json:"actions"`
}

// NgcKasLegacyClaims represent the custom JWT claims used by NGC KAS
type NgcKasClaims struct {
	Access []NgcAccessClaim `json:"access"`
	jwt.RegisteredClaims
}

// ValidateOrg checks whether a specified org name is included in claims
func (nc *NgcKasClaims) ValidateOrg(orgName string) bool {
	isValid := false
	for _, claim := range nc.Access {
		if strings.HasPrefix(claim.Type, NgcOrgClaimTypePrefix) && claim.Name == orgName {
			isValid = true
			break
		}
	}

	return isValid
}

// SsaClaims represent the custom JWT claims used by SSA
type SsaClaims struct {
	Scopes []string `json:"scopes"`
	jwt.RegisteredClaims
}

// ValidateScope checks whether a specified scope is included in claims
func (sc *SsaClaims) ValidateScope(scope string) bool {
	for _, s := range sc.Scopes {
		if s == scope {
			return true
		}
	}
	return false
}
