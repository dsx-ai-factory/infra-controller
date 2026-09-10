// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	mapset "github.com/deckarep/golang-set/v2"
	"github.com/go-jose/go-jose/v4"
	"github.com/golang-jwt/jwt/v5"
	"github.com/labstack/echo/v4"
	"github.com/pkg/errors"
)

// =============================================================================
// Constants
// =============================================================================

const (
	minUpdateInterval = 10 * time.Second
)

// =============================================================================
// Package Variables
// =============================================================================

var (
	isServiceAccountContextKey = AuthContextKey("isServiceAccount")
)

// =============================================================================
// AuthContextKey Type and Functions
// =============================================================================

// AuthContextKey is a custom type for context keys to avoid collisions
type AuthContextKey string

// SetIsServiceAccountInContext stores whether the request is from a service account
func SetIsServiceAccountInContext(c echo.Context, isServiceAccount bool) {
	ctx := context.WithValue(c.Request().Context(), isServiceAccountContextKey, isServiceAccount)
	c.SetRequest(c.Request().WithContext(ctx))
}

// GetIsServiceAccountFromContext returns whether the request is from a service account
func GetIsServiceAccountFromContext(c echo.Context) bool {
	v := c.Request().Context().Value(isServiceAccountContextKey)
	if v == nil {
		return false
	}
	b, ok := v.(bool)
	return ok && b
}

// =============================================================================
// ClaimMapping Struct and Methods
// =============================================================================

// ClaimMapping defines how to map JWT claims to organization data.
// Dynamic mode: set OrgAttribute to extract org from token claims.
// Static mode: set OrgName for a fixed organization.
type ClaimMapping struct {
	// OrgAttribute: JWT claim path to extract org name (e.g., "org", "data.org"). Makes this a dynamic mapping.
	OrgAttribute string `mapstructure:"orgAttribute"`
	// OrgDisplayAttribute: JWT claim path for org display name (dynamic mappings only)
	OrgDisplayAttribute string `mapstructure:"orgDisplayAttribute"`

	// OrgName: fixed organization name (static mapping). Used when OrgAttribute is empty.
	OrgName string `mapstructure:"orgName"`
	// OrgDisplayName: display name for static org mappings
	OrgDisplayName string `mapstructure:"orgDisplayName"`

	// RolesAttribute: JWT claim path to extract roles (e.g., "roles", "data.roles"). Takes precedence over Roles.
	RolesAttribute string `mapstructure:"rolesAttribute"`
	// Roles: static role list. Used when RolesAttribute is empty and IsServiceAccount is false.
	Roles []string `mapstructure:"roles"`

	// Audiences: optional token audiences allowed to authorize this mapping. Any one exact match is sufficient.
	Audiences []string `mapstructure:"audiences"`

	// IsServiceAccount: if true, assigns admin roles (PROVIDER_ADMIN, TENANT_ADMIN). Ignores RolesAttribute/Roles.
	IsServiceAccount bool `mapstructure:"isServiceAccount"`
}

// IsOrgDynamic returns true if this is a valid dynamic org mapping.
// Dynamic mappings require all three attributes:
//   - OrgAttribute: JWT claim path to extract org name (e.g., "org", "data.org")
//   - OrgDisplayAttribute: JWT claim path to extract org display name (e.g., "org_display", "data.orgDisplayName")
//   - RolesAttribute: JWT claim path to extract roles (e.g., "roles", "data.roles")
//
// Service accounts are not allowed with dynamic orgs.
func (cm *ClaimMapping) IsOrgDynamic() bool {
	return cm.OrgAttribute != ""
}

// IsOrgStatic returns true if using a fixed org name (OrgName set).
func (cm *ClaimMapping) IsOrgStatic() bool { return cm.OrgName != "" }

// GetRoles returns roles based on mapping config: service account roles, dynamic extraction, or static roles.
func (cm *ClaimMapping) GetRoles(claims jwt.MapClaims) ([]string, error) {
	if cm.IsServiceAccount {
		return ServiceAccountRoles, nil
	}
	if cm.RolesAttribute != "" {
		return GetRolesFromAttribute(claims, cm.RolesAttribute)
	}
	return cm.Roles, nil
}

// GetOrgNameAndDisplayName extracts org and display name from claims (dynamic mappings only).
func (cm *ClaimMapping) GetOrgNameAndDisplayName(claims jwt.MapClaims) (orgName string, displayName string) {
	if !cm.IsOrgDynamic() {
		orgName = cm.OrgName
		displayName = cm.OrgDisplayName
		if displayName == "" {
			displayName = orgName
		}
		return orgName, displayName
	}

	rawOrgName := core.GetClaimAttributeAsString(claims, cm.OrgAttribute)
	orgName = strings.ToLower(rawOrgName)
	displayName = core.GetClaimAttributeAsString(claims, cm.OrgDisplayAttribute)

	// If display name not found, use the original (non-lowercased) org name
	if displayName == "" && rawOrgName != "" {
		displayName = rawOrgName
	}

	return orgName, displayName
}

// =============================================================================
// JwksConfig Struct, Constructor, and Methods
// =============================================================================

// JwksConfig holds configuration for a JWKS endpoint and token validation.
type JwksConfig struct {
	Name          string
	IsUpdating    uint32        // atomic flag for concurrent JWKS updates
	sync.RWMutex                // protects JWKS access
	URL           string        // JWKS endpoint URL
	Issuer        string        // expected "iss" claim value
	Origin        string        // token origin type (e.g., "kas-legacy", "kas-ssa", "keycloak", "custom", "kas")
	LastUpdated   time.Time     // last successful JWKS update timestamp
	LastAttempted time.Time     // last JWKS update attempt timestamp
	jwks          *core.JWKS    // cached JWKS keys
	updateDone    chan struct{} // closed when the current JWKS update finishes
	JWKSTimeout   time.Duration // fetch timeout (default: 5s)

	Audiences []string // allowed audience values (token must have at least one)
	Scopes    []string // required scopes (token must have ALL)

	ClaimMappings []ClaimMapping // org/role mapping configuration

	// ServiceAccount enables client credentials flow (Keycloak only).
	// For custom issuers, use ClaimMapping.IsServiceAccount instead.
	ServiceAccount bool

	// ReservedOrgNames prevents dynamic org mappings from claiming statically-configured org names.
	// Populated by nico-rest-api during initialization.
	ReservedOrgNames map[string]bool

	subjectPrefix string // SHA256(issuer)[0:10] - namespaces subject claims
}

// NewJwksConfig is a function that initializes and returns a configuration object for managing JWKS
func NewJwksConfig(name string, url string, issuer string, origin string, serviceAccount bool, audiences []string, scopes []string) *JwksConfig {
	// Default to custom origin if not specified
	if origin == "" {
		origin = TokenOriginCustom
	}
	return &JwksConfig{
		Name:           name,
		URL:            url,
		Issuer:         issuer,
		Origin:         origin,
		ServiceAccount: serviceAccount,
		Audiences:      audiences,
		Scopes:         scopes,
	}
}

// GetKeyByID is a method that returns a JWK secret by ID with enhanced validation
func (jcfg *JwksConfig) GetKeyByID(id string) (interface{}, error) {
	// Validate input parameters
	if strings.TrimSpace(id) == "" {
		return nil, jwt.ErrInvalidKey
	}

	jcfg.RLock()
	defer jcfg.RUnlock()

	if jcfg.jwks == nil {
		return nil, core.ErrJWKSNotInitialized
	}

	key, err := jcfg.jwks.GetKeyByID(id)
	if err != nil {
		return nil, errors.Wrap(jwt.ErrInvalidKey, err.Error())
	}

	// Validate key using go-jose's built-in validation
	if !key.Valid() {
		return nil, errors.Wrapf(jose.ErrUnsupportedKeyType, "go-jose validation failed for key %s", id)
	}

	return key.Key, nil
}

// KeyCount returns the number of keys in the JWKS
func (jcfg *JwksConfig) KeyCount() int {
	jcfg.RLock()
	defer jcfg.RUnlock()

	if jcfg.jwks == nil || jcfg.jwks.Set == nil {
		return 0
	}

	return len(jcfg.jwks.Set.Keys)
}

// MatchesIssuer checks if the given issuer exactly matches the configured issuer
func (jcfg *JwksConfig) MatchesIssuer(issuer string) bool {
	if jcfg == nil {
		return false
	}

	jcfg.RLock()
	defer jcfg.RUnlock()

	if jcfg.Issuer == "" {
		return false
	}

	return issuer == jcfg.Issuer
}

// UpdateJWKS fetches and validates JWKS from the configured URL. Throttled to minUpdateInterval.
func (jcfg *JwksConfig) UpdateJWKS() error {
	jcfg.Lock()
	if jcfg.URL == "" {
		jcfg.Unlock()
		return core.ErrJWKSURLEmpty
	}
	if !jcfg.LastAttempted.IsZero() && time.Since(jcfg.LastAttempted) < minUpdateInterval {
		if atomic.LoadUint32(&jcfg.IsUpdating) != 0 {
			jcfg.Unlock()
			return core.ErrJWKSUpdateInProgress
		}
		hasCachedKeys := jcfg.jwks != nil
		jcfg.Unlock()
		if !hasCachedKeys {
			return core.ErrJWKSNotInitialized
		}
		return nil
	}
	if !atomic.CompareAndSwapUint32(&jcfg.IsUpdating, 0, 1) {
		jcfg.Unlock()
		return core.ErrJWKSUpdateInProgress
	}
	jcfg.updateDone = make(chan struct{})
	urlCopy, timeout := jcfg.URL, jcfg.JWKSTimeout
	jcfg.LastAttempted = time.Now()
	jcfg.Unlock()
	defer func() {
		jcfg.Lock()
		atomic.StoreUint32(&jcfg.IsUpdating, 0)
		close(jcfg.updateDone)
		jcfg.updateDone = nil
		jcfg.Unlock()
	}()

	jwks, err := core.NewJWKSFromURL(urlCopy, timeout)
	if err != nil {
		return errors.Wrapf(err, "failed to update JWKS from %s", urlCopy)
	}
	if jwks.Set == nil || len(jwks.Set.Keys) == 0 {
		return errors.Wrapf(core.ErrEmptyKeySet, "from %s", urlCopy)
	}

	validKeyCount := 0
	for _, key := range jwks.Set.Keys {
		if key.Valid() {
			validKeyCount++
		}
	}
	if validKeyCount == 0 {
		return errors.Wrapf(core.ErrNoValidKeys, "from %s", urlCopy)
	}

	jcfg.Lock()
	defer jcfg.Unlock()
	jcfg.jwks = jwks
	jcfg.LastUpdated = time.Now()
	return nil
}

// GetJWKS returns the enhanced JWKS with go-jose capabilities
func (jcfg *JwksConfig) GetJWKS() *core.JWKS {
	jcfg.RLock()
	defer jcfg.RUnlock()
	return jcfg.jwks
}

// ValidateToken parses token from Authorization header with caller-provided claims and enhanced validation
func (jcfg *JwksConfig) ValidateToken(authHeader string, claims jwt.Claims) (*jwt.Token, error) {
	// Validate input parameters
	if strings.TrimSpace(authHeader) == "" {
		return nil, jwt.ErrTokenMalformed
	}

	if claims == nil {
		return nil, jwt.ErrTokenMalformed
	}

	// Use a comprehensive set of common JWT algorithms instead of restricting to current JWKS
	// This allows tokens with algorithms that might become available after JWKS updates
	allCommonAlgorithms := []string{
		"RS256", "RS384", "RS512", // RSA with SHA
		"PS256", "PS384", "PS512", // RSA-PSS with SHA
		"ES256", "ES384", "ES512", // ECDSA with SHA
		"HS256", "HS384", "HS512", // HMAC with SHA
		"EdDSA", // Ed25519/Ed448
	}

	jwtParser := jwt.NewParser(jwt.WithValidMethods(allCommonAlgorithms))

	token, err := jwtParser.ParseWithClaims(authHeader, claims, jcfg.getPublicKey)
	if err != nil {
		return nil, err
	}

	if !token.Valid {
		return nil, jwt.ErrTokenInvalidClaims
	}

	return token, nil
}

// getPublicKey retrieves the public key from the JWKS for JWT validation
func (jcfg *JwksConfig) getPublicKey(token *jwt.Token) (interface{}, error) {
	if token == nil || token.Header == nil {
		return nil, jwt.ErrTokenMalformed
	}

	algorithm, _ := token.Header["alg"].(string)
	if algorithm == "" {
		return nil, jwt.ErrTokenMalformed
	}

	kid, _ := token.Header["kid"].(string)

	// If kid is present, use existing single-key logic
	if kid != "" {
		key, err := jcfg.getKeyFromJWKS(kid)
		if err != nil {
			// Attempt JWKS update with retry logic for concurrent updates
			if updateErr := jcfg.tryUpdateJWKSWithRetry(); updateErr == nil {
				key, err = jcfg.getKeyFromJWKS(kid)
			}
		}
		return key, err
	}

	// No kid provided - try all candidate keys for the algorithm
	return jcfg.tryMultipleKeysForValidation(token, algorithm)
}

// tryUpdateJWKSWithRetry attempts to update JWKS with retry logic for concurrent updates
func (jcfg *JwksConfig) tryUpdateJWKSWithRetry() error {
	updateErr := jcfg.UpdateJWKS()
	if updateErr == nil {
		return nil
	}
	if !errors.Is(updateErr, core.ErrJWKSUpdateInProgress) {
		return updateErr
	}

	jcfg.RLock()
	updateDone := jcfg.updateDone
	jcfg.RUnlock()
	if updateDone != nil {
		<-updateDone
	}

	if jcfg.GetJWKS() == nil {
		return core.ErrJWKSNotInitialized
	}
	return nil
}

// tryMultipleKeysForValidation tries all candidate keys for algorithm-only validation
func (jcfg *JwksConfig) tryMultipleKeysForValidation(token *jwt.Token, algorithm string) (interface{}, error) {
	// Get all candidate keys from current JWKS
	candidateKeys, err := jcfg.getCandidateKeysWithRetry(algorithm)
	if err != nil {
		return nil, errors.Wrap(jwt.ErrInvalidKey, err.Error())
	}

	// Try to validate token with current candidate keys
	key, err := jcfg.tryValidateWithCandidateKeys(token, candidateKeys)
	if err == nil {
		return key, nil
	}

	// If all current keys failed, try with fresh JWKS update
	return jcfg.tryValidateWithFreshJWKS(token, algorithm, err)
}

// getCandidateKeysWithRetry gets candidate keys, with JWKS update retry if initial attempt fails
func (jcfg *JwksConfig) getCandidateKeysWithRetry(algorithm string) ([]interface{}, error) {
	candidateKeys, err := jcfg.getAllCandidateKeys(algorithm)
	if err != nil {
		// Attempt JWKS update and retry
		if updateErr := jcfg.tryUpdateJWKSWithRetry(); updateErr == nil {
			candidateKeys, err = jcfg.getAllCandidateKeys(algorithm)
		}
	}
	return candidateKeys, err
}

// tryValidateWithCandidateKeys attempts to validate token with provided candidate keys
func (jcfg *JwksConfig) tryValidateWithCandidateKeys(token *jwt.Token, candidateKeys []interface{}) (interface{}, error) {
	// Use the same comprehensive algorithm list as ValidateToken
	allCommonAlgorithms := []string{
		"RS256", "RS384", "RS512", // RSA with SHA
		"PS256", "PS384", "PS512", // RSA-PSS with SHA
		"ES256", "ES384", "ES512", // ECDSA with SHA
		"HS256", "HS384", "HS512", // HMAC with SHA
		"EdDSA", // Ed25519/Ed448
	}

	jwtParser := jwt.NewParser(jwt.WithValidMethods(allCommonAlgorithms))

	var lastErr error
	for _, candidateKey := range candidateKeys {
		keyFunc := func(token *jwt.Token) (interface{}, error) {
			return candidateKey, nil
		}

		_, parseErr := jwtParser.Parse(token.Raw, keyFunc)
		if parseErr == nil {
			return candidateKey, nil
		}
		lastErr = parseErr
	}

	return nil, lastErr
}

// tryValidateWithFreshJWKS attempts validation after updating JWKS with fresh keys
func (jcfg *JwksConfig) tryValidateWithFreshJWKS(token *jwt.Token, algorithm string, previousErr error) (interface{}, error) {
	if updateErr := jcfg.tryUpdateJWKSWithRetry(); updateErr == nil {
		freshCandidateKeys, freshErr := jcfg.getAllCandidateKeys(algorithm)
		if freshErr == nil && len(freshCandidateKeys) > 0 {
			key, err := jcfg.tryValidateWithCandidateKeys(token, freshCandidateKeys)
			if err == nil {
				return key, nil
			}
			previousErr = err // Update error from fresh validation attempt
		}
	}

	return nil, errors.Wrap(jwt.ErrInvalidKey, previousErr.Error())
}

// getAllCandidateKeys retrieves all candidate keys for an algorithm (used when no kid provided)
func (jcfg *JwksConfig) getAllCandidateKeys(algorithm string) ([]interface{}, error) {
	jwks := jcfg.GetJWKS()
	if jwks == nil {
		return nil, core.ErrJWKSNotInitialized
	}

	if algorithm == "" {
		return nil, jwt.ErrTokenMalformed
	}

	supportedKeys := jwks.GetKeysForAlgorithm(algorithm)
	if len(supportedKeys) == 0 {
		return nil, errors.Wrapf(jose.ErrUnsupportedAlgorithm, "algorithm %s", algorithm)
	}

	// Collect all valid keys, preferring signing keys first
	var signingKeys []interface{}
	var otherKeys []interface{}

	for _, key := range supportedKeys {
		if key.Valid() {
			if key.Use == "" || key.Use == "sig" {
				signingKeys = append(signingKeys, key.Key)
			} else {
				otherKeys = append(otherKeys, key.Key)
			}
		}
	}

	// Return signing keys first, then other keys
	result := append(signingKeys, otherKeys...)
	if len(result) == 0 {
		return nil, errors.Wrapf(jose.ErrUnsupportedAlgorithm, "algorithm %s", algorithm)
	}

	return result, nil
}

// getKeyFromJWKS retrieves a key from JWKS by kid
func (jcfg *JwksConfig) getKeyFromJWKS(kid string) (interface{}, error) {
	jwks := jcfg.GetJWKS()
	if jwks == nil {
		return nil, core.ErrJWKSNotInitialized
	}

	if kid == "" {
		return nil, errors.Wrapf(jwt.ErrInvalidKeyType, "kid is empty")
	}

	return jcfg.GetKeyByID(kid)
}

// HasClaimMappings returns true if claim mappings are configured.
func (jcfg *JwksConfig) HasClaimMappings() bool { return len(jcfg.ClaimMappings) > 0 }

// GetClaimMappings returns the claim mappings.
func (jcfg *JwksConfig) GetClaimMappings() []ClaimMapping { return jcfg.ClaimMappings }

// GetSubjectPrefix returns the issuer-derived prefix for namespacing subjects.
func (jcfg *JwksConfig) GetSubjectPrefix() string {
	if jcfg.Origin == TokenOriginCustom && jcfg.subjectPrefix == "" && jcfg.Issuer != "" {
		jcfg.subjectPrefix = core.ComputeIssuerPrefix(jcfg.Issuer)
	}
	return jcfg.subjectPrefix
}

// hasAnyAudience checks if the token has any of the configured audiences.
func (jcfg *JwksConfig) hasAnyAudience(claims jwt.MapClaims, audiences []string) bool {
	if audiences == nil || len(audiences) == 0 {
		return true
	}

	tokenAudiences, err := claims.GetAudience()
	if err != nil {
		return false
	}
	tokenAudSet := mapset.NewSet([]string(tokenAudiences)...)
	allowedAudSet := mapset.NewSet(audiences...)
	return tokenAudSet.Intersect(allowedAudSet).Cardinality() > 0
}

// ValidateAudience checks token has at least one configured audience. Returns nil if none configured.
func (jcfg *JwksConfig) ValidateAudience(claims jwt.MapClaims) error {
	if !jcfg.hasAnyAudience(claims, jcfg.Audiences) {
		return core.ErrInvalidAudience
	}
	return nil
}

// ValidateScopes checks token has ALL configured scopes. Returns nil if none configured.
func (jcfg *JwksConfig) ValidateScopes(claims jwt.MapClaims) error {
	if len(jcfg.Scopes) == 0 {
		return nil
	}
	tokenScopes := core.GetScopes(claims)
	tokenScopeSet := mapset.NewSet(tokenScopes...)
	requiredScopeSet := mapset.NewSet(jcfg.Scopes...)
	if !tokenScopeSet.IsSuperset(requiredScopeSet) {
		return core.ErrInvalidScope
	}
	return nil
}

// GetOrgDataFromClaim extracts org data for the requested org from claim mappings.
// This method validates org access and returns errors if:
//   - core.ErrReservedOrgName: dynamic org claims a statically-configured org name
//   - core.ErrInvalidAudience: token audience is not authorized for the requested org
//   - core.ErrInvalidConfiguration: no claim mapping configured for the requested org
//   - core.ErrNoClaimRoles: no roles found for the requested org
//
// Returns orgData, isServiceAccount, and any error.
func (jcfg *JwksConfig) GetOrgDataFromClaim(claims jwt.MapClaims, reqOrgFromRoute string) (cdbm.OrgData, bool, error) {
	reqOrg := strings.ToLower(reqOrgFromRoute)

	for _, cm := range jcfg.ClaimMappings {
		orgName, displayName := cm.GetOrgNameAndDisplayName(claims)
		if orgName != reqOrg {
			continue
		}

		if cm.IsOrgDynamic() && jcfg.ReservedOrgNames != nil && jcfg.ReservedOrgNames[orgName] {
			return nil, false, core.ErrReservedOrgName
		}

		if !jcfg.hasAnyAudience(claims, cm.Audiences) {
			return nil, false, core.ErrInvalidAudience
		}

		roles, err := cm.GetRoles(claims)
		if err != nil || len(roles) == 0 {
			return nil, false, core.ErrNoClaimRoles
		}

		now := time.Now().UTC()
		orgData := cdbm.OrgData{
			orgName: cdbm.Org{
				Name:        orgName,
				DisplayName: displayName,
				OrgType:     "ENTERPRISE",
				Roles:       roles,
				Teams:       []cdbm.Team{},
				Updated:     &now,
			},
		}

		return orgData, cm.IsServiceAccount, nil
	}

	return nil, false, core.ErrInvalidConfiguration
}
