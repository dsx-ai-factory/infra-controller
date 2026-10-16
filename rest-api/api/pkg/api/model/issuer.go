// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"net"
	"net/url"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
)

const (
	validationErrorAuthIssuerDynamicClaimMapping = "dynamic claim attributes (orgAttribute/orgDisplayAttribute/rolesAttribute) are not permitted via the API; " +
		"only static orgName and roles claim mappings can be created"
	validationErrorAuthIssuerClaimMappingOrgName = "orgName is required (only static org name claim mappings can be created via the API)"
)

// APIAuthIssuerStatus reports whether an issuer can verify tokens yet. It is derived
// from the row's cached key set on every read, not stored.
type APIAuthIssuerStatus string

const (
	// AuthIssuerStatusPending means no signing keys have been fetched yet. Registration
	// never contacts the identity provider, so an issuer starts here and stays here
	// while its JWKS endpoint is unreachable; tokens it signs cannot be verified
	// until the background retry succeeds.
	AuthIssuerStatusPending APIAuthIssuerStatus = "Pending"
	// AuthIssuerStatusReady means a key set has been fetched and tokens can be verified.
	AuthIssuerStatusReady APIAuthIssuerStatus = "Ready"
)

// APIAuthIssuer is the API representation of a runtime-managed external JWT issuer.
type APIAuthIssuer struct {
	// ID is the unique identifier of the issuer.
	ID string `json:"id"`
	// Origin selects the token processor (kas-legacy|kas-ssa|keycloak|custom).
	Origin string `json:"origin"`
	// IssuerURL is the expected JWT "iss" claim and natural upsert key.
	IssuerURL string `json:"issuerUrl"`
	// JWKSUrl is where signing keys are fetched from.
	JWKSUrl string `json:"jwksUrl"`
	// JWKSTimeout is the JWKS fetch timeout (e.g. "5s").
	JWKSTimeout string `json:"jwksTimeout"`
	// ServiceAccount enables client-credentials flow (disconnected mode only).
	ServiceAccount bool `json:"serviceAccount"`
	// Audiences is the issuer-level allowed audience set (token needs at least one).
	Audiences []string `json:"audiences"`
	// Scopes is the issuer-level required scope set (token needs all).
	Scopes []string `json:"scopes"`
	// ClaimMappings is the org/role mapping array.
	ClaimMappings APIAuthIssuerClaimMappings `json:"claimMappings"`
	// Status reports whether signing keys have been fetched. A stored key set that
	// is present but unparseable reads as Ready; only an older or buggy writer can
	// produce one, and hydration logs it when it refuses the blob.
	Status APIAuthIssuerStatus `json:"status"`
	// JWKSFetchedAt is when the signing keys were last fetched, absent if never.
	JWKSFetchedAt *time.Time `json:"jwksFetchedAt"`
	// Created is the creation timestamp.
	Created time.Time `json:"created"`
	// Updated is the last-update timestamp.
	Updated time.Time `json:"updated"`
}

// APIAuthIssuerClaimMapping is the public REST representation of an issuer claim mapping.
// Keeping this type in the API package prevents persistence-model changes from
// silently changing the JSON contract.
type APIAuthIssuerClaimMapping struct {
	OrgAttribute        string   `json:"orgAttribute"`
	OrgDisplayAttribute string   `json:"orgDisplayAttribute"`
	OrgName             string   `json:"orgName"`
	OrgDisplayName      string   `json:"orgDisplayName"`
	RolesAttribute      string   `json:"rolesAttribute"`
	Roles               []string `json:"roles"`
	Audiences           []string `json:"audiences"`
	Scopes              []string `json:"scopes"`
	IsServiceAccount    bool     `json:"isServiceAccount"`
}

// ToDBModel converts the API mapping to its persistence representation.
func (m APIAuthIssuerClaimMapping) ToDBModel() cdbm.ClaimMapping {
	return cdbm.ClaimMapping{
		OrgAttribute:        m.OrgAttribute,
		OrgDisplayAttribute: m.OrgDisplayAttribute,
		OrgName:             m.OrgName,
		OrgDisplayName:      m.OrgDisplayName,
		RolesAttribute:      m.RolesAttribute,
		Roles:               m.Roles,
		Audiences:           m.Audiences,
		Scopes:              m.Scopes,
		IsServiceAccount:    m.IsServiceAccount,
	}
}

// FromDBModel populates the API mapping from its persistence representation.
func (m *APIAuthIssuerClaimMapping) FromDBModel(dbMapping cdbm.ClaimMapping) {
	roles := dbMapping.Roles
	if roles == nil {
		roles = []string{}
	}
	audiences := dbMapping.Audiences
	if audiences == nil {
		audiences = []string{}
	}
	scopes := dbMapping.Scopes
	if scopes == nil {
		scopes = []string{}
	}

	m.OrgAttribute = dbMapping.OrgAttribute
	m.OrgDisplayAttribute = dbMapping.OrgDisplayAttribute
	m.OrgName = dbMapping.OrgName
	m.OrgDisplayName = dbMapping.OrgDisplayName
	m.RolesAttribute = dbMapping.RolesAttribute
	m.Roles = roles
	m.Audiences = audiences
	m.Scopes = scopes
	m.IsServiceAccount = dbMapping.IsServiceAccount
}

// APIAuthIssuerClaimMappings is the public REST claim-mapping collection.
type APIAuthIssuerClaimMappings []APIAuthIssuerClaimMapping

// ToDBModel converts the API mappings to their persistence representation.
func (mappings APIAuthIssuerClaimMappings) ToDBModel() []cdbm.ClaimMapping {
	if mappings == nil {
		return nil
	}

	dbMappings := make([]cdbm.ClaimMapping, len(mappings))
	for i := range mappings {
		dbMappings[i] = mappings[i].ToDBModel()
	}
	return dbMappings
}

// FromDBModel populates the API mappings from their persistence representation.
func (mappings *APIAuthIssuerClaimMappings) FromDBModel(dbMappings []cdbm.ClaimMapping) {
	if dbMappings == nil {
		*mappings = APIAuthIssuerClaimMappings{}
		return
	}

	*mappings = make(APIAuthIssuerClaimMappings, len(dbMappings))
	for i := range dbMappings {
		(*mappings)[i].FromDBModel(dbMappings[i])
	}
}

// FromDBModel populates the API issuer from its persistence representation.
func (issuer *APIAuthIssuer) FromDBModel(dbIssuer *cdbm.Issuer) {
	if dbIssuer == nil {
		return
	}

	claimMappings := APIAuthIssuerClaimMappings{}
	claimMappings.FromDBModel(dbIssuer.ClaimMappings)
	audiences := dbIssuer.Audiences
	if audiences == nil {
		audiences = []string{}
	}
	scopes := dbIssuer.Scopes
	if scopes == nil {
		scopes = []string{}
	}

	status := AuthIssuerStatusPending
	if dbIssuer.HasCachedKeys() {
		status = AuthIssuerStatusReady
	}

	*issuer = APIAuthIssuer{
		ID:             dbIssuer.ID.String(),
		Origin:         dbIssuer.Origin,
		IssuerURL:      dbIssuer.IssuerURL,
		JWKSUrl:        dbIssuer.JWKSUrl,
		JWKSTimeout:    dbIssuer.JWKSTimeout,
		ServiceAccount: dbIssuer.ServiceAccount,
		Audiences:      audiences,
		Scopes:         scopes,
		ClaimMappings:  claimMappings,
		Status:         status,
		JWKSFetchedAt:  dbIssuer.JWKSFetchedAt,
		Created:        dbIssuer.CreatedAt,
		Updated:        dbIssuer.UpdatedAt,
	}
}

// APIAuthIssuerCreateOrUpdateRequest is the complete request body for creating or
// replacing an issuer selected by IssuerURL. Origin is not accepted: API-managed
// issuers are always custom.
type APIAuthIssuerCreateOrUpdateRequest struct {
	IssuerURL      string                     `json:"issuerUrl"`
	JWKSUrl        string                     `json:"jwksUrl"`
	JWKSTimeout    string                     `json:"jwksTimeout"`
	ServiceAccount bool                       `json:"serviceAccount"`
	Audiences      []string                   `json:"audiences"`
	Scopes         []string                   `json:"scopes"`
	ClaimMappings  APIAuthIssuerClaimMappings `json:"claimMappings"`
}

// DefaultJWKSURL returns the JWKS URL, defaulting to {issuerUrl}/.well-known/jwks.json.
func (r APIAuthIssuerCreateOrUpdateRequest) DefaultJWKSURL() string {
	if strings.TrimSpace(r.JWKSUrl) != "" {
		return r.JWKSUrl
	}
	return strings.TrimRight(r.IssuerURL, "/") + "/.well-known/jwks.json"
}

// validateStaticOnlyClaimMapping enforces that issuers managed through the API
// carry only static claim mappings: a fixed orgName plus optional static roles.
// Attribute-driven fields let the token choose its own org and roles, a
// cross-tenant escalation risk, so those issuers must live in the ConfigMap.
func (r APIAuthIssuerCreateOrUpdateRequest) validateStaticOnlyClaimMapping(value interface{}) error {
	cm, ok := value.(APIAuthIssuerClaimMapping)
	if !ok {
		return nil
	}
	if cm.OrgAttribute != "" || cm.OrgDisplayAttribute != "" || cm.RolesAttribute != "" {
		return errors.New(validationErrorAuthIssuerDynamicClaimMapping)
	}
	if strings.TrimSpace(cm.OrgName) == "" {
		return errors.New(validationErrorAuthIssuerClaimMappingOrgName)
	}
	return nil
}

// validateAbsoluteHTTPURL rejects relative, malformed, and non-HTTP(S) issuer
// endpoints while preserving the HTTP URLs used by local disconnected setups.
func validateAbsoluteHTTPURL(value interface{}) error {
	raw, ok := value.(string)
	if !ok || strings.TrimSpace(raw) == "" {
		return nil
	}

	parsed, err := url.ParseRequestURI(raw)
	if err != nil || !parsed.IsAbs() || parsed.Host == "" {
		return errors.New("must be an absolute URL")
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {
		return fmt.Errorf("scheme must be http or https, got %q", parsed.Scheme)
	}
	return nil
}

// validateNotInternalHost rejects endpoints on loopback, private, link-local, or
// in-cluster hosts. The refresh loop fetches these URLs from the pod network, so
// without this an issuer write is a path to probe or exfiltrate from services the
// caller cannot otherwise reach. Malformed values are left to validateAbsoluteHTTPURL.
func validateNotInternalHost(value interface{}) error {
	raw, ok := value.(string)
	if !ok || strings.TrimSpace(raw) == "" {
		return nil
	}
	parsed, err := url.ParseRequestURI(raw)
	if err != nil {
		return nil
	}
	host := strings.ToLower(parsed.Hostname())
	if ip := net.ParseIP(host); ip != nil {
		if ip.IsLoopback() || ip.IsPrivate() || ip.IsLinkLocalUnicast() || ip.IsLinkLocalMulticast() || ip.IsUnspecified() {
			return errors.New("must not target a loopback, private, link-local, or unspecified address")
		}
		return nil
	}
	if host == "localhost" || strings.HasSuffix(host, ".localhost") ||
		strings.HasSuffix(host, ".svc") || strings.Contains(host, ".svc.") ||
		strings.HasSuffix(host, ".cluster.local") || strings.HasSuffix(host, ".internal") {
		return errors.New("must not target an internal or in-cluster host")
	}
	return nil
}

// ValidateExternalHosts rejects issuer and JWKS endpoints on internal network
// addresses. It is separate from Validate because a deployment may opt out of it
// for local development; the handler decides whether it applies.
func (r *APIAuthIssuerCreateOrUpdateRequest) ValidateExternalHosts() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.IssuerURL, validation.By(validateNotInternalHost)),
		validation.Field(&r.JWKSUrl, validation.By(validateNotInternalHost)),
	)
}

// Validate performs lightweight, shape-level validation. Deep, cross-issuer rules
// (org uniqueness, single dynamic mapping, disconnected-only service accounts,
// role validity, ...) are enforced by config.ValidateIssuersConfig over the
// combined static+DB set in the handler.
func (r *APIAuthIssuerCreateOrUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.IssuerURL,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(util.NotAllWhitespaceRegexp).Error(validationErrorValueRequired),
			validation.By(validateAbsoluteHTTPURL)),
		validation.Field(&r.JWKSUrl,
			validation.By(validateAbsoluteHTTPURL)),
		validation.Field(&r.ClaimMappings,
			validation.Each(validation.By(r.validateStaticOnlyClaimMapping))),
	)
}

// ToCreateOrUpdateInput builds the complete DAO input from the request. Origin
// is always custom — runtime-managed issuers cannot select a privileged processor.
func (r *APIAuthIssuerCreateOrUpdateRequest) ToCreateOrUpdateInput(actorID *uuid.UUID) cdbm.IssuerCreateOrUpdateInput {
	claimMappings := r.ClaimMappings.ToDBModel()
	if claimMappings == nil {
		claimMappings = []cdbm.ClaimMapping{}
	}
	return cdbm.IssuerCreateOrUpdateInput{
		Origin:         cauth.TokenOriginCustom,
		IssuerURL:      r.IssuerURL,
		JWKSUrl:        r.DefaultJWKSURL(),
		JWKSTimeout:    r.JWKSTimeout,
		ServiceAccount: r.ServiceAccount,
		Audiences:      r.Audiences,
		Scopes:         r.Scopes,
		ClaimMappings:  claimMappings,
		ActorID:        actorID,
	}
}
