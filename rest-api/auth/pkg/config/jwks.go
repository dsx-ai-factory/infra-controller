// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	mapset "github.com/deckarep/golang-set/v2"
	"github.com/go-jose/go-jose/v4"
	"github.com/golang-jwt/jwt/v5"
	"github.com/labstack/echo/v4"
	"github.com/pkg/errors"
	"golang.org/x/sync/singleflight"
)

// =============================================================================
// Constants
// =============================================================================

const (
	minUpdateInterval = 10 * time.Second

	// negativeKIDTTL is how long a kid absent from a freshly fetched key set is
	// remembered as unknown: long enough that a flood of garbage kids cannot drive a
	// fetch storm, short enough to pick up a genuinely new key promptly.
	negativeKIDTTL = 30 * time.Second

	// maxNegativeKIDs bounds the negative cache against tokens minted with random
	// kids. On overflow the map is dropped wholesale, costing at most one fetch.
	maxNegativeKIDs = 256

	// maxJWKSAge bounds how long a key set may keep validating tokens with no
	// successful fetch confirming it. Persisting key sets buys availability across an
	// IdP outage, but with no ceiling that becomes indefinite trust in keys the
	// issuer may have retired. Reaching it is not a hard failure: the lookup miss
	// drives a refresh, so only an issuer unreachable for the whole window fails
	// closed.
	maxJWKSAge = 24 * time.Hour
)

// =============================================================================
// Package Variables
// =============================================================================

var (
	isServiceAccountContextKey = AuthContextKey("isServiceAccount")

	// jwksFetchGroup collapses concurrent fetches for the same endpoint into one
	// request. It is keyed by JWKS URL, not by JwksConfig pointer, so a registry
	// rebuild mid-flight still shares the in-flight fetch.
	jwksFetchGroup singleflight.Group
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

	// Scopes: optional scopes the token must carry to authorize this mapping. All are required, in addition to any issuer-level scopes.
	Scopes []string `mapstructure:"scopes"`

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
	sync.RWMutex                // protects JWKS access
	URL           string        // JWKS endpoint URL
	Issuer        string        // expected "iss" claim value
	Origin        string        // token origin type (e.g., "kas-legacy", "kas-ssa", "keycloak", "custom", "kas")
	LastUpdated   time.Time     // when the installed key set was fetched (success only)
	LastAttempted time.Time     // last JWKS fetch attempt, successful or not; keys the throttle
	lastSettledAt time.Time     // attempt time of the last fetch that settled; what LastAttempt reports
	lastError     error         // how the last settled fetch ended, nil for success
	jwks          *core.JWKS    // cached JWKS keys
	updateDone    chan struct{} // closed when this config's in-flight fetch settles
	JWKSTimeout   time.Duration // fetch timeout (default: 5s)

	Audiences []string // allowed audience values (token must have at least one)
	Scopes    []string // required scopes (token must have ALL)

	ClaimMappings []ClaimMapping // org/role mapping configuration

	// ServiceAccount enables client credentials flow (Keycloak only).
	// For custom issuers, use ClaimMapping.IsServiceAccount instead.
	ServiceAccount bool

	// RestrictEgress refuses key fetches that would reach an internal network
	// address or follow a redirect. Set for issuers an API caller defined.
	RestrictEgress bool

	// reservedOrgNames prevents dynamic org mappings from claiming
	// statically-configured org names. Guarded by the embedded mutex.
	reservedOrgNames map[string]bool

	// negativeKIDs records kids that were still absent after a fresh fetch, with
	// the expiry after which they are worth re-checking. Guarded by the embedded
	// mutex; cleared whenever a new key set is installed.
	negativeKIDs map[string]time.Time
}

// NewJwksConfig is a function that initializes and returns a configuration object for managing JWKS
func NewJwksConfig(url string, issuer string, origin string, serviceAccount bool, audiences []string, scopes []string) *JwksConfig {
	// Default to custom origin if not specified
	if origin == "" {
		origin = TokenOriginCustom
	}
	return &JwksConfig{
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
	if jcfg.expiredLocked() {
		return nil, core.ErrJWKSExpired
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

// expiredLocked reports whether the installed key set is older than maxJWKSAge and
// so may no longer be used to validate tokens. The caller must hold the lock.
func (jcfg *JwksConfig) expiredLocked() bool {
	return !jcfg.LastUpdated.IsZero() && time.Since(jcfg.LastUpdated) > maxJWKSAge
}

// beginFetch claims the next fetch unless minUpdateInterval has not passed since
// the last attempt, successful or not, so a failing IdP is not re-fetched on every
// request. The returned channel is closed by finishFetch once the fetch settles.
func (jcfg *JwksConfig) beginFetch() (chan struct{}, time.Time, bool) {
	jcfg.Lock()
	defer jcfg.Unlock()
	if !jcfg.LastAttempted.IsZero() && time.Since(jcfg.LastAttempted) < minUpdateInterval {
		return nil, time.Time{}, false
	}
	jcfg.LastAttempted = time.Now()
	done := make(chan struct{})
	jcfg.updateDone = done
	return done, jcfg.LastAttempted, true
}

// finishFetch releases callers waiting on the fetch begun with done.
func (jcfg *JwksConfig) finishFetch(done chan struct{}) {
	jcfg.Lock()
	defer jcfg.Unlock()
	close(done)
	if jcfg.updateDone == done {
		jcfg.updateDone = nil
	}
}

// awaitInFlight waits for a fetch this config has in flight and reports whether
// one settled. It reports false at once when none is in flight or ctx is done.
func (jcfg *JwksConfig) awaitInFlight(ctx context.Context) bool {
	jcfg.RLock()
	done := jcfg.updateDone
	jcfg.RUnlock()
	if done == nil {
		return false
	}
	select {
	case <-done:
		return true
	case <-ctx.Done():
		return false
	}
}

// UpdateJWKS fetches and validates JWKS from the configured URL. Throttled to
// minUpdateInterval; while throttled it waits for a fetch already in flight and
// reports ErrJWKSNotInitialized when no key set has been installed.
func (jcfg *JwksConfig) UpdateJWKS() error {
	ctx := context.Background()
	_, _, fetched, err := jcfg.RefreshJWKS(ctx)
	if err != nil || fetched {
		return err
	}
	jcfg.awaitInFlight(ctx)
	if jcfg.GetJWKS() == nil {
		return core.ErrJWKSNotInitialized
	}
	return nil
}

// RefreshJWKS fetches and validates the key set from the configured URL, installs
// it, and returns its serialized form plus the time the fetch was issued so the
// caller can persist both.
//
// fetched separates the two non-error outcomes: true means the installed set is
// new, false means the minUpdateInterval throttle suppressed the fetch and nothing
// changed, so a caller that just missed a kid must not retry its lookup.
//
// Concurrent callers for the same endpoint share one fetch. That shared fetch is
// bounded by the JWKS timeout rather than any one caller's cancellation, while each
// waiter still returns as soon as its own ctx is done.
func (jcfg *JwksConfig) RefreshJWKS(ctx context.Context) (raw []byte, fetchedAt time.Time, fetched bool, err error) {
	jcfg.RLock()
	urlCopy, timeout, restrict := jcfg.URL, jcfg.JWKSTimeout, jcfg.RestrictEgress
	jcfg.RUnlock()
	if urlCopy == "" {
		return nil, time.Time{}, false, core.ErrJWKSURLEmpty
	}
	done, attemptAt, ok := jcfg.beginFetch()
	if !ok {
		return nil, time.Time{}, false, nil
	}

	// A restricted and an unrestricted fetch of one URL never share a flight.
	flightKey := urlCopy
	if restrict {
		flightKey = "restricted|" + urlCopy
	}
	fetchCtx := context.WithoutCancel(ctx)
	ch := jwksFetchGroup.DoChan(flightKey, func() (any, error) {
		return fetchJWKS(fetchCtx, urlCopy, timeout, restrict)
	})

	// The shared fetch outlives a cancelled caller, so its result is installed and
	// in-flight waiters released whether or not this caller is still listening.
	// Every waiter installs the result on itself: the flight is keyed by URL, so a
	// waiter can be a different JwksConfig than the one that ran the fetch.
	type outcome struct {
		res       singleflight.Result
		installed bool
	}
	settled := make(chan outcome, 1)
	go func() {
		res := <-ch
		installed := false
		if res.Err == nil {
			result := res.Val.(*jwksFetchResult)
			installed = jcfg.install(result.jwks, result.fetchedAt)
		}
		jcfg.Lock()
		jcfg.lastSettledAt, jcfg.lastError = attemptAt, res.Err
		jcfg.Unlock()
		jcfg.finishFetch(done)
		settled <- outcome{res: res, installed: installed}
	}()

	select {
	case <-ctx.Done():
		return nil, time.Time{}, false, ctx.Err()
	case o := <-settled:
		if o.res.Err != nil {
			return nil, time.Time{}, false, o.res.Err
		}
		result := o.res.Val.(*jwksFetchResult)
		// A set the monotonic guard refused changed nothing: the caller must not
		// retry its lookup against it, nor persist it over the newer stored set.
		return result.raw, result.fetchedAt, o.installed, nil
	}
}

// jwksFetchResult is what one deduplicated fetch hands to every waiter.
type jwksFetchResult struct {
	jwks *core.JWKS
	raw  []byte
	// fetchedAt is when the request was issued, not when it completed: a slow
	// request that started first can finish last, and a completion stamp would let
	// it overwrite a newer key set.
	fetchedAt time.Time
}

// fetchJWKS performs the single deduplicated fetch behind RefreshJWKS.
func fetchJWKS(ctx context.Context, url string, timeout time.Duration, restrictEgress bool) (*jwksFetchResult, error) {
	fetchedAt := time.Now().UTC()

	jwks, err := core.NewJWKSFromURLContext(ctx, url, timeout, restrictEgress)
	if err != nil {
		return nil, errors.Wrapf(err, "failed to update JWKS from %s", url)
	}
	if err := jwks.Validate(); err != nil {
		return nil, errors.Wrapf(err, "from %s", url)
	}

	raw, err := jwks.Marshal()
	if err != nil {
		return nil, errors.Wrapf(err, "from %s", url)
	}

	return &jwksFetchResult{jwks: jwks, raw: raw, fetchedAt: fetchedAt}, nil
}

// install replaces the live key set, unless what is already installed was acquired
// at or after fetchedAt, and clears the negative cache derived from the old set.
// It reports whether the set was installed.
//
// That monotonic guard mirrors the row's stale-write predicate: without it a slow
// fetch could roll this replica back to an older set than the one a reload just
// hydrated. Cross-replica stamps make the comparison approximate, so a skewed peer
// can cost a local fetch its install; the trusted set stays and the next refresh
// corrects it. SetJWKSFromCache clamps a peer's stamp so the skew is bounded by
// the fetch interval rather than by the peer's clock.
func (jcfg *JwksConfig) install(jwks *core.JWKS, fetchedAt time.Time) bool {
	jcfg.Lock()
	defer jcfg.Unlock()
	if jcfg.jwks != nil && !fetchedAt.After(jcfg.LastUpdated) {
		return false
	}
	jcfg.jwks = jwks
	jcfg.LastUpdated = fetchedAt
	if fetchedAt.After(jcfg.LastAttempted) {
		jcfg.LastAttempted = fetchedAt
	}
	jcfg.negativeKIDs = nil
	return true
}

// SetJWKSFromCache installs a key set loaded from persistent storage. fetchedAt
// seeds LastUpdated, so the throttle treats a set fetched seconds ago by another
// replica as already fetched rather than refetching it on startup.
//
// The stamp came from another replica's clock. One that reads in the future is
// clamped to now: left as is, it would suppress this replica's fetches until its
// own clock caught up, make the monotonic guard refuse every local fetch until
// then, and extend maxJWKSAge by the skew.
func (jcfg *JwksConfig) SetJWKSFromCache(jwks *core.JWKS, fetchedAt time.Time) {
	if jwks == nil {
		return
	}
	if now := time.Now(); fetchedAt.After(now) {
		fetchedAt = now
	}
	jcfg.install(jwks, fetchedAt)
}

// CarryAttempt adopts the settled fetch of the entry this one replaces for the
// same endpoint, so the replacement reports the outcome this process observed.
func (jcfg *JwksConfig) CarryAttempt(old *JwksConfig) {
	at, err := old.LastAttempt()
	jcfg.Lock()
	defer jcfg.Unlock()
	if at.After(jcfg.lastSettledAt) {
		jcfg.lastSettledAt, jcfg.lastError = at, err
	}
}

// JWKSURL returns the key endpoint.
func (jcfg *JwksConfig) JWKSURL() string {
	jcfg.RLock()
	defer jcfg.RUnlock()
	return jcfg.URL
}

// LastAttempt reports when the last fetch that has settled was attempted, by
// any caller, and how it ended: a nil error for success. Zero time means no fetch
// has settled yet; an attempt still in flight is not reported.
func (jcfg *JwksConfig) LastAttempt() (time.Time, error) {
	jcfg.RLock()
	defer jcfg.RUnlock()
	return jcfg.lastSettledAt, jcfg.lastError
}

// LastFetchedAt reports when the installed key set was fetched, zero if never.
func (jcfg *JwksConfig) LastFetchedAt() time.Time {
	jcfg.RLock()
	defer jcfg.RUnlock()
	return jcfg.LastUpdated
}

// GetJWKS returns the enhanced JWKS with go-jose capabilities, or nil once the
// installed set has aged past maxJWKSAge. Callers already treat nil as "refresh
// before use", which is what an expired set needs.
func (jcfg *JwksConfig) GetJWKS() *core.JWKS {
	jcfg.RLock()
	defer jcfg.RUnlock()
	if jcfg.expiredLocked() {
		return nil
	}
	return jcfg.jwks
}

// ValidateTokenContext parses token from Authorization header with caller-provided
// claims and enhanced validation. A key lookup that misses may trigger a JWKS
// fetch, and ctx caps how long that fetch may hold the caller, so a client that
// hangs up does not leave a fetch running past the request.
func (jcfg *JwksConfig) ValidateTokenContext(ctx context.Context, authHeader string, claims jwt.Claims) (*jwt.Token, error) {
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

	token, err := jwtParser.ParseWithClaims(authHeader, claims, jcfg.keyFunc(ctx))
	if err != nil {
		return nil, err
	}

	if !token.Valid {
		return nil, jwt.ErrTokenInvalidClaims
	}

	return token, nil
}

// keyFunc returns the jwt.Keyfunc that resolves a token's signing key, carrying
// ctx so that any JWKS fetch the resolution triggers is bounded by the caller.
func (jcfg *JwksConfig) keyFunc(ctx context.Context) jwt.Keyfunc {
	return func(token *jwt.Token) (interface{}, error) {
		return jcfg.getPublicKey(ctx, token)
	}
}

// getPublicKey retrieves the public key from the JWKS for JWT validation
func (jcfg *JwksConfig) getPublicKey(ctx context.Context, token *jwt.Token) (interface{}, error) {
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
		return jcfg.resolveKeyByID(ctx, kid)
	}

	// No kid provided - try all candidate keys for the algorithm
	return jcfg.tryMultipleKeysForValidation(ctx, token, algorithm)
}

// resolveKeyByID looks up a key by kid, refreshing the key set once if it misses.
// A miss that survives the refresh is remembered, so a flood of tokens carrying
// the same unknown kid costs one fetch rather than one per request.
func (jcfg *JwksConfig) resolveKeyByID(ctx context.Context, kid string) (interface{}, error) {
	key, err := jcfg.getKeyFromJWKS(kid)
	if err == nil {
		return key, nil
	}
	if jcfg.isUnknownKID(kid) {
		return nil, err
	}

	retry, refreshErr := jcfg.refreshedOrAwaited(ctx)
	if refreshErr != nil {
		// The fetch that could have produced this kid failed; remember the miss so the
		// next request within negativeKIDTTL does not pay for another attempt.
		jcfg.rememberUnknownKID(kid)
		return nil, errors.Wrap(err, refreshErr.Error())
	}
	if !retry {
		return nil, err
	}

	key, retryErr := jcfg.getKeyFromJWKS(kid)
	if retryErr != nil {
		jcfg.rememberUnknownKID(kid)
		return nil, retryErr
	}

	return key, nil
}

// refreshedOrAwaited refreshes the key set, or, when the refresh is throttled,
// waits for the fetch this config already has in flight. It reports whether a
// lookup that just missed is worth retrying: true when a new set was installed
// or an in-flight fetch settled, false when the set is unchanged. Every lookup
// path uses it, so a request that arrives while another request's fetch is in
// flight waits for that fetch whether or not its token names a kid.
func (jcfg *JwksConfig) refreshedOrAwaited(ctx context.Context) (bool, error) {
	_, _, fetched, err := jcfg.RefreshJWKS(ctx)
	if err != nil {
		return false, err
	}
	return fetched || jcfg.awaitInFlight(ctx), nil
}

// isUnknownKID reports whether kid was already found absent from a freshly
// fetched key set recently enough that re-fetching would be pointless.
func (jcfg *JwksConfig) isUnknownKID(kid string) bool {
	jcfg.RLock()
	defer jcfg.RUnlock()

	expiry, ok := jcfg.negativeKIDs[kid]
	return ok && time.Now().Before(expiry)
}

// rememberUnknownKID records a kid that a fresh fetch did not produce.
func (jcfg *JwksConfig) rememberUnknownKID(kid string) {
	jcfg.Lock()
	defer jcfg.Unlock()

	if jcfg.negativeKIDs == nil || len(jcfg.negativeKIDs) >= maxNegativeKIDs {
		jcfg.negativeKIDs = make(map[string]time.Time, 1)
	}
	jcfg.negativeKIDs[kid] = time.Now().Add(negativeKIDTTL)
}

// tryMultipleKeysForValidation tries all candidate keys for algorithm-only validation
func (jcfg *JwksConfig) tryMultipleKeysForValidation(ctx context.Context, token *jwt.Token, algorithm string) (interface{}, error) {
	// Get all candidate keys from current JWKS
	candidateKeys, err := jcfg.getCandidateKeysWithRetry(ctx, algorithm)
	if err != nil {
		return nil, errors.Wrap(jwt.ErrInvalidKey, err.Error())
	}

	// Try to validate token with current candidate keys
	key, err := jcfg.tryValidateWithCandidateKeys(token, candidateKeys)
	if err == nil {
		return key, nil
	}

	// If all current keys failed, try with fresh JWKS update
	return jcfg.tryValidateWithFreshJWKS(ctx, token, algorithm, err)
}

// getCandidateKeysWithRetry gets candidate keys, refreshing the key set once if
// the first attempt finds none.
func (jcfg *JwksConfig) getCandidateKeysWithRetry(ctx context.Context, algorithm string) ([]interface{}, error) {
	candidateKeys, err := jcfg.getAllCandidateKeys(algorithm)
	if err != nil {
		if retry, refreshErr := jcfg.refreshedOrAwaited(ctx); refreshErr == nil && retry {
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
func (jcfg *JwksConfig) tryValidateWithFreshJWKS(ctx context.Context, token *jwt.Token, algorithm string, previousErr error) (interface{}, error) {
	if retry, refreshErr := jcfg.refreshedOrAwaited(ctx); refreshErr == nil && retry {
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

// getKeyFromJWKS retrieves a key by kid, leaving the uninitialized and expired
// cases to GetKeyByID so the caller can tell one from the other.
func (jcfg *JwksConfig) getKeyFromJWKS(kid string) (interface{}, error) {
	if kid == "" {
		return nil, errors.Wrapf(jwt.ErrInvalidKeyType, "kid is empty")
	}

	return jcfg.GetKeyByID(kid)
}

// HasClaimMappings returns true if claim mappings are configured.
func (jcfg *JwksConfig) HasClaimMappings() bool { return len(jcfg.ClaimMappings) > 0 }

// GetClaimMappings returns the claim mappings.
func (jcfg *JwksConfig) GetClaimMappings() []ClaimMapping { return jcfg.ClaimMappings }

// SetReservedOrgNames replaces the reserved-org set. The input is copied so a
// caller cannot race readers by retaining and mutating the map.
func (jcfg *JwksConfig) SetReservedOrgNames(reserved map[string]bool) {
	jcfg.Lock()
	defer jcfg.Unlock()
	jcfg.reservedOrgNames = make(map[string]bool, len(reserved))
	for org := range reserved {
		jcfg.reservedOrgNames[org] = true
	}
}

// GetReservedOrgNames returns a snapshot of the current reserved-org set.
func (jcfg *JwksConfig) GetReservedOrgNames() map[string]bool {
	jcfg.RLock()
	defer jcfg.RUnlock()
	reserved := make(map[string]bool, len(jcfg.reservedOrgNames))
	for org := range jcfg.reservedOrgNames {
		reserved[org] = true
	}
	return reserved
}

// isReservedOrg reports whether some issuer statically owns org.
func (jcfg *JwksConfig) isReservedOrg(org string) bool {
	jcfg.RLock()
	defer jcfg.RUnlock()
	return jcfg.reservedOrgNames[org]
}

// GetSubjectPrefix returns the issuer-derived prefix for namespacing subjects.
func (jcfg *JwksConfig) GetSubjectPrefix() string {
	if jcfg.Origin != TokenOriginCustom || jcfg.Issuer == "" {
		return ""
	}
	return core.ComputeIssuerPrefix(jcfg.Issuer)
}

// hasAnyAudience checks if the token has any of the configured audiences.
func (jcfg *JwksConfig) hasAnyAudience(claims jwt.MapClaims, audiences []string) bool {
	if len(audiences) == 0 {
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

// hasAllScopes checks if the token carries every required scope.
func hasAllScopes(claims jwt.MapClaims, scopes []string) bool {
	if len(scopes) == 0 {
		return true
	}

	tokenScopeSet := mapset.NewSet(core.GetScopes(claims)...)
	requiredScopeSet := mapset.NewSet(scopes...)
	return tokenScopeSet.IsSuperset(requiredScopeSet)
}

// ValidateScopes checks token has ALL configured scopes. Returns nil if none configured.
func (jcfg *JwksConfig) ValidateScopes(claims jwt.MapClaims) error {
	if !hasAllScopes(claims, jcfg.Scopes) {
		return core.ErrInvalidScope
	}
	return nil
}

// GetOrgDataFromClaim extracts organization data from claims for the org named in
// the request path. Of the mappings that name that org, those whose audiences or
// scopes the token does not satisfy are skipped, and exactly one must remain: it
// supplies the roles. Repeated mappings of one org carry disjoint audiences (see
// ValidateIssuersConfig), but a token may present several audiences, so two
// survivors are possible and the token is refused. A dynamic mapping that names a
// reserved org is skipped rather than fatal: a static mapping of the same org in
// this issuer is an explicit grant and still wins.
//
// This method validates org access and returns errors if:
//   - core.ErrReservedOrgName: only a dynamic mapping named the org, and it is reserved
//   - core.ErrInvalidAudience: no mapping for the org accepts the token's audience
//   - core.ErrInvalidScope: a mapping accepts the audience but the token lacks a scope it requires;
//     when other mappings refused the audience, the error carries both
//   - core.ErrAmbiguousMapping: more than one mapping accepts the token
//   - core.ErrInvalidConfiguration: no claim mapping names the requested org
//   - core.ErrNoClaimRoles: the selected mapping yields no roles
//
// Returns orgData, isServiceAccount, and any error.
func (jcfg *JwksConfig) GetOrgDataFromClaim(claims jwt.MapClaims, reqOrgFromRoute string) (cdbm.OrgData, bool, error) {
	reqOrg := strings.ToLower(reqOrgFromRoute)

	var selected *ClaimMapping
	var selectedOrg, selectedDisplay string
	named, audienceMiss, reserved := 0, 0, false
	for i := range jcfg.ClaimMappings {
		cm := &jcfg.ClaimMappings[i]
		orgName, displayName := cm.GetOrgNameAndDisplayName(claims)
		if orgName != reqOrg {
			continue
		}
		named++

		if cm.IsOrgDynamic() && jcfg.isReservedOrg(orgName) {
			reserved = true
			continue
		}
		if !jcfg.hasAnyAudience(claims, cm.Audiences) {
			audienceMiss++
			continue
		}
		if !hasAllScopes(claims, cm.Scopes) {
			continue
		}
		if selected != nil {
			return nil, false, core.ErrAmbiguousMapping
		}
		selected, selectedOrg, selectedDisplay = cm, orgName, displayName
	}
	switch {
	case named == 0:
		return nil, false, core.ErrInvalidConfiguration
	case selected == nil && reserved:
		return nil, false, core.ErrReservedOrgName
	case selected == nil && audienceMiss == named:
		return nil, false, core.ErrInvalidAudience
	case selected == nil && audienceMiss > 0:
		return nil, false, fmt.Errorf("%w; %w", core.ErrInvalidAudience, core.ErrInvalidScope)
	case selected == nil:
		return nil, false, core.ErrInvalidScope
	}

	roles, err := selected.GetRoles(claims)
	if err != nil || len(roles) == 0 {
		return nil, false, core.ErrNoClaimRoles
	}

	now := time.Now().UTC()
	orgData := cdbm.OrgData{
		selectedOrg: cdbm.Org{
			Name:        selectedOrg,
			DisplayName: selectedDisplay,
			OrgType:     "ENTERPRISE",
			Roles:       roles,
			Teams:       []cdbm.Team{},
			Updated:     &now,
		},
	}
	return orgData, selected.IsServiceAccount, nil
}
