// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"net/url"
	"strings"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
)

// APIAuthIssuerStatus is the issuer's stored lifecycle status.
type APIAuthIssuerStatus string

const (
	// AuthIssuerStatusPending means no usable signing keys are loaded.
	AuthIssuerStatusPending APIAuthIssuerStatus = "Pending"
	// AuthIssuerStatusReady means a key set is loaded and every replica's last refresh
	// succeeded.
	AuthIssuerStatusReady APIAuthIssuerStatus = "Ready"
	// AuthIssuerStatusDegraded means a key set is loaded but at least one replica's
	// last refresh failed.
	AuthIssuerStatusDegraded APIAuthIssuerStatus = "Degraded"
	// AuthIssuerStatusSuperseded appears only in statusHistory, when the ConfigMap
	// took over an issuer created through the API.
	AuthIssuerStatusSuperseded APIAuthIssuerStatus = "Superseded"
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
	// Source is config for an issuer the ConfigMap defines, api otherwise.
	Source string `json:"source"`
	// Status is Pending, Ready, or Degraded. StatusMessage explains Degraded.
	Status        APIAuthIssuerStatus `json:"status"`
	StatusMessage *string             `json:"statusMessage"`
	// JWKSFetchedAt is when the signing keys were last fetched, absent if never.
	// JWKSLastAttemptAt and JWKSLastError describe the last refresh attempt.
	JWKSFetchedAt     *time.Time `json:"jwksFetchedAt"`
	JWKSLastAttemptAt *time.Time `json:"jwksLastAttemptAt"`
	JWKSLastError     *string    `json:"jwksLastError"`
	// StatusHistory lists recent status transitions and configuration changes.
	StatusHistory []APIStatusDetail `json:"statusHistory"`
	// Created is the creation timestamp.
	Created time.Time `json:"created"`
	// Updated is the last-update timestamp.
	Updated time.Time `json:"updated"`
}

// APIAuthIssuerClaimMapping is the API representation of an issuer claim mapping.
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
	// Source is config or api. Read-only: a value in a request is ignored.
	Source string `json:"source"`
}

// ToDBModel converts the API mapping to its persistence representation. Every
// mapping a request carries is API-sourced, whatever the body says.
func (m APIAuthIssuerClaimMapping) ToDBModel() cdbm.ClaimMapping {
	return cdbm.ClaimMapping{
		Source:              cdbm.IssuerSourceAPI,
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
	m.Source = dbMapping.Source
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

// FromDBModel populates the API issuer from its persistence representation and
// the status_detail rows recorded for it.
func (issuer *APIAuthIssuer) FromDBModel(dbIssuer *cdbm.Issuer, dbsds []cdbm.StatusDetail) {
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

	history := make([]APIStatusDetail, 0, len(dbsds))
	for _, dbsd := range dbsds {
		history = append(history, NewAPIStatusDetail(dbsd))
	}

	*issuer = APIAuthIssuer{
		ID:                dbIssuer.ID.String(),
		Origin:            dbIssuer.Origin,
		IssuerURL:         dbIssuer.IssuerURL,
		JWKSUrl:           dbIssuer.JWKSUrl,
		JWKSTimeout:       dbIssuer.JWKSTimeout,
		ServiceAccount:    dbIssuer.ServiceAccount,
		Audiences:         audiences,
		Scopes:            scopes,
		ClaimMappings:     claimMappings,
		Source:            dbIssuer.Source,
		Status:            APIAuthIssuerStatus(dbIssuer.Status),
		StatusMessage:     dbIssuer.StatusMessage,
		JWKSFetchedAt:     dbIssuer.JWKSFetchedAt,
		JWKSLastAttemptAt: dbIssuer.JWKSLastAttemptAt,
		JWKSLastError:     dbIssuer.JWKSLastError,
		StatusHistory:     history,
		Created:           dbIssuer.CreatedAt,
		Updated:           dbIssuer.UpdatedAt,
	}
}

// NewAPIAuthIssuer returns the API issuer for dbIssuer and its status_detail rows,
// or nil when dbIssuer is nil.
func NewAPIAuthIssuer(dbIssuer *cdbm.Issuer, dbsds []cdbm.StatusDetail) *APIAuthIssuer {
	if dbIssuer == nil {
		return nil
	}
	issuer := &APIAuthIssuer{}
	issuer.FromDBModel(dbIssuer, dbsds)
	return issuer
}

// APIAuthIssuerCreateOrUpdateRequest is the request body for creating or replacing
// an Auth Issuer.
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

// Validate performs shape-level validation; the handler enforces issuer rules.
func (r *APIAuthIssuerCreateOrUpdateRequest) Validate() error {
	return validation.ValidateStruct(r,
		validation.Field(&r.IssuerURL,
			validation.Required.Error(validationErrorValueRequired),
			validation.Match(util.NotAllWhitespaceRegexp).Error(validationErrorValueRequired),
			validation.By(validateAbsoluteHTTPURL)),
		validation.Field(&r.JWKSUrl,
			validation.By(validateAbsoluteHTTPURL)),
		// API issuers are always custom, so the Keycloak serviceAccount switch does not apply.
		validation.Field(&r.ServiceAccount,
			validation.In(false).Error("serviceAccount applies to Keycloak issuers only; a custom issuer's service account is a ConfigMap claim mapping with isServiceAccount")),
	)
}

// ToCreateOrUpdateInput builds the complete DAO input from the request. Origin
// is always custom — runtime-managed issuers cannot select a privileged processor.
func (r *APIAuthIssuerCreateOrUpdateRequest) ToCreateOrUpdateInput(userID *uuid.UUID) cdbm.IssuerCreateOrUpdateInput {
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
		CreatedBy:      userID,
		UpdatedBy:      userID,
	}
}
