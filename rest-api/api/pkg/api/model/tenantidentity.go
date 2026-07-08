// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"errors"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
)

// APITenantIdentityConfigCreateOrUpdateRequest is the PUT /tenant-identity/config body.
type APITenantIdentityConfigCreateOrUpdateRequest struct {
	Enabled                  *bool    `json:"enabled"`
	Issuer                   string   `json:"issuer"`
	DefaultAudience          string   `json:"defaultAudience"`
	AllowedAudiences         []string `json:"allowedAudiences"`
	TokenTtlSeconds          uint32   `json:"tokenTtlSeconds"`
	SubjectPrefix            *string  `json:"subjectPrefix"`
	RotateKey                *bool    `json:"rotateKey"`
	SigningKeyOverlapSeconds *uint32  `json:"signingKeyOverlapSeconds"`
}

// Validate enforces the REST-layer contract. Enabled is optional; nil
// defaults to true at ToProto time so an omitted field does not land at
// Core as the proto default `false`.
func (req APITenantIdentityConfigCreateOrUpdateRequest) Validate() error {
	if err := validation.ValidateStruct(&req,
		validation.Field(&req.Issuer, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&req.DefaultAudience, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&req.TokenTtlSeconds, validation.Required.Error(validationErrorValueRequired), validation.Min(uint32(1))),
	); err != nil {
		return err
	}
	if len(req.AllowedAudiences) > 0 {
		found := false
		for _, a := range req.AllowedAudiences {
			if a == req.DefaultAudience {
				found = true
				break
			}
		}
		if !found {
			return validation.Errors{
				"defaultAudience": errors.New("defaultAudience must be a member of non-empty allowedAudiences"),
			}
		}
	}
	rotateKey := req.RotateKey != nil && *req.RotateKey
	switch {
	case rotateKey && req.SigningKeyOverlapSeconds == nil:
		return validation.Errors{
			"signingKeyOverlapSeconds": errors.New("signingKeyOverlapSeconds is required when rotateKey is true"),
		}
	case !rotateKey && req.SigningKeyOverlapSeconds != nil:
		return validation.Errors{
			"signingKeyOverlapSeconds": errors.New("signingKeyOverlapSeconds must be omitted when rotateKey is false"),
		}
	case rotateKey && req.SigningKeyOverlapSeconds != nil && *req.SigningKeyOverlapSeconds < req.TokenTtlSeconds:
		return validation.Errors{
			"signingKeyOverlapSeconds": errors.New("signingKeyOverlapSeconds must be >= tokenTtlSeconds so previously-signed JWTs remain verifiable until they expire"),
		}
	}
	return nil
}

// ToProto converts the request to its gRPC form. The Core proto field
// names retain the `_sec` suffix (`token_ttl_sec`, `signing_key_overlap_sec`);
// only the REST/JSON spelling uses `Seconds`.
func (req APITenantIdentityConfigCreateOrUpdateRequest) ToProto(org string) *corev1.SetTenantIdentityConfigRequest {
	cfg := &corev1.TenantIdentityConfig{
		DefaultAudience:  req.DefaultAudience,
		AllowedAudiences: req.AllowedAudiences,
		SubjectPrefix:    req.SubjectPrefix,
		Issuer:           req.Issuer,
		TokenTtlSec:      req.TokenTtlSeconds,
	}
	cfg.Enabled = req.Enabled == nil || *req.Enabled
	if req.RotateKey != nil {
		cfg.RotateKey = *req.RotateKey
	}
	if req.SigningKeyOverlapSeconds != nil {
		cfg.SigningKeyOverlapSec = req.SigningKeyOverlapSeconds
	}

	return &corev1.SetTenantIdentityConfigRequest{
		OrganizationId: org,
		Config:         cfg,
	}
}

// APITenantIdentitySigningKey describes one entry in `APITenantIdentityConfig.SigningKeys`.
type APITenantIdentitySigningKey struct {
	Kid           string     `json:"kid"`
	Alg           string     `json:"alg"`
	CurrentSigner bool       `json:"currentSigner"`
	ExpireAt      *time.Time `json:"expireAt"`
}

// APITenantIdentityConfig is the GET /tenant-identity/config response body.
type APITenantIdentityConfig struct {
	Org              string                        `json:"org"`
	Enabled          bool                          `json:"enabled"`
	Issuer           string                        `json:"issuer"`
	DefaultAudience  string                        `json:"defaultAudience"`
	AllowedAudiences []string                      `json:"allowedAudiences"`
	TokenTtlSeconds  uint32                        `json:"tokenTtlSeconds"`
	SubjectPrefix    string                        `json:"subjectPrefix"`
	SigningKeys      []APITenantIdentitySigningKey `json:"signingKeys"`
	Created          time.Time                     `json:"created"`
	Updated          time.Time                     `json:"updated"`
}

// FromResponseProto populates the response from the gRPC reply.
func (resp *APITenantIdentityConfig) FromResponseProto(proto *corev1.TenantIdentityConfigResponse) {
	if proto == nil {
		return
	}
	resp.Org = proto.GetOrganizationId()
	if cfg := proto.GetConfig(); cfg != nil {
		resp.Enabled = cfg.GetEnabled()
		resp.Issuer = cfg.GetIssuer()
		resp.DefaultAudience = cfg.GetDefaultAudience()
		resp.AllowedAudiences = cfg.GetAllowedAudiences()
		resp.TokenTtlSeconds = cfg.GetTokenTtlSec()
		resp.SubjectPrefix = cfg.GetSubjectPrefix()
	}
	if keys := proto.GetSigningKeys(); len(keys) > 0 {
		resp.SigningKeys = make([]APITenantIdentitySigningKey, 0, len(keys))
		for _, k := range keys {
			entry := APITenantIdentitySigningKey{
				Kid:           k.GetKid(),
				Alg:           k.GetAlg(),
				CurrentSigner: k.GetCurrentSigner(),
			}
			if ts := k.GetExpireAt(); ts != nil {
				v := ts.AsTime().UTC()
				entry.ExpireAt = &v
			}
			resp.SigningKeys = append(resp.SigningKeys, entry)
		}
	}
	if ts := proto.GetCreatedAt(); ts != nil {
		resp.Created = ts.AsTime().UTC()
	}
	if ts := proto.GetUpdatedAt(); ts != nil {
		resp.Updated = ts.AsTime().UTC()
	}
}

// IsCreated reports whether the object was created (rather than updated) by the upserting call.
func (resp *APITenantIdentityConfig) IsCreated() bool {
	if resp == nil || resp.Created.IsZero() || resp.Updated.IsZero() {
		return false
	}
	return resp.Created.Equal(resp.Updated)
}

// APITenantIdentityBasicClientSecretRequest carries OAuth2 client_secret_basic credentials.
type APITenantIdentityBasicClientSecretRequest struct {
	ClientID     string `json:"clientId"`
	ClientSecret string `json:"clientSecret"`
}

// APITenantIdentityBasicClientSecretResponse carries the SHA-256 hash of the stored secret.
type APITenantIdentityBasicClientSecretResponse struct {
	ClientID         string `json:"clientId"`
	ClientSecretHash string `json:"clientSecretHash"`
}

// APITenantIdentityTokenDelegationCreateOrUpdateRequest is the PUT /tenant-identity/token-delegation body.
type APITenantIdentityTokenDelegationCreateOrUpdateRequest struct {
	TokenEndpoint        string                                     `json:"tokenEndpoint"`
	ClientSecretBasic    *APITenantIdentityBasicClientSecretRequest `json:"clientSecretBasic,omitempty"`
	SubjectTokenAudience string                                     `json:"subjectTokenAudience"`
}

// Validate enforces required fields and, when `clientSecretBasic` is set,
// its sub-fields. The Core gRPC API validates `tokenEndpoint` scheme/host
// against its configured allowlist.
func (req APITenantIdentityTokenDelegationCreateOrUpdateRequest) Validate() error {
	if err := validation.ValidateStruct(&req,
		validation.Field(&req.TokenEndpoint, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&req.SubjectTokenAudience, validation.Required.Error(validationErrorValueRequired)),
	); err != nil {
		return err
	}
	if req.ClientSecretBasic != nil {
		return validation.ValidateStruct(req.ClientSecretBasic,
			validation.Field(&req.ClientSecretBasic.ClientID, validation.Required.Error(validationErrorValueRequired)),
			validation.Field(&req.ClientSecretBasic.ClientSecret, validation.Required.Error(validationErrorValueRequired)),
		)
	}
	return nil
}

// ToProto converts the request to its gRPC form.
func (req APITenantIdentityTokenDelegationCreateOrUpdateRequest) ToProto(org string) *corev1.TokenDelegationRequest {
	cfg := &corev1.TokenDelegation{
		TokenEndpoint:        req.TokenEndpoint,
		SubjectTokenAudience: req.SubjectTokenAudience,
	}
	if req.ClientSecretBasic != nil {
		cfg.AuthMethodConfig = &corev1.TokenDelegation_ClientSecretBasic{
			ClientSecretBasic: &corev1.ClientSecretBasic{
				ClientId:     req.ClientSecretBasic.ClientID,
				ClientSecret: req.ClientSecretBasic.ClientSecret,
			},
		}
	}
	return &corev1.TokenDelegationRequest{
		OrganizationId: org,
		Config:         cfg,
	}
}

// APITenantIdentityTokenDelegation is the GET /tenant-identity/token-delegation response body.
type APITenantIdentityTokenDelegation struct {
	TokenEndpoint        string                                      `json:"tokenEndpoint"`
	ClientSecretBasic    *APITenantIdentityBasicClientSecretResponse `json:"clientSecretBasic,omitempty"`
	SubjectTokenAudience string                                      `json:"subjectTokenAudience"`
	Created              time.Time                                   `json:"created"`
	Updated              time.Time                                   `json:"updated"`
}

// FromResponseProto populates the response from the gRPC reply.
func (resp *APITenantIdentityTokenDelegation) FromResponseProto(proto *corev1.TokenDelegationResponse) {
	if proto == nil {
		return
	}
	resp.TokenEndpoint = proto.GetTokenEndpoint()
	resp.SubjectTokenAudience = proto.GetSubjectTokenAudience()
	if basic := proto.GetClientSecretBasic(); basic != nil {
		resp.ClientSecretBasic = &APITenantIdentityBasicClientSecretResponse{
			ClientID:         basic.GetClientId(),
			ClientSecretHash: basic.GetClientSecretHash(),
		}
	}
	if ts := proto.GetCreatedAt(); ts != nil {
		resp.Created = ts.AsTime().UTC()
	}
	if ts := proto.GetUpdatedAt(); ts != nil {
		resp.Updated = ts.AsTime().UTC()
	}
}

// IsCreated reports whether the object was created (rather than updated) by the upserting call.
func (resp *APITenantIdentityTokenDelegation) IsCreated() bool {
	if resp == nil || resp.Created.IsZero() || resp.Updated.IsZero() {
		return false
	}
	return resp.Created.Equal(resp.Updated)
}

// NOTE: Standard for well known OpenID configurations is to use
// underscore_case attributes. All other NICo REST API attributes
// should be using camelCase.
//
// APIOpenIDConfiguration is the .well-known/openid-configuration response body.
type APIOpenIDConfiguration struct {
	Issuer                           string   `json:"issuer"`
	JwksURI                          string   `json:"jwks_uri"`
	ResponseTypesSupported           []string `json:"response_types_supported"`
	SubjectTypesSupported            []string `json:"subject_types_supported"`
	IDTokenSigningAlgValuesSupported []string `json:"id_token_signing_alg_values_supported"`
	SpiffeJwksURI                    string   `json:"spiffe_jwks_uri"`
}

// FromResponseProto populates the response from the gRPC reply.
func (resp *APIOpenIDConfiguration) FromResponseProto(proto *corev1.OpenIdConfiguration) {
	if proto == nil {
		return
	}
	resp.Issuer = proto.GetIssuer()
	resp.JwksURI = proto.GetJwksUri()
	resp.ResponseTypesSupported = proto.GetResponseTypesSupported()
	resp.SubjectTypesSupported = proto.GetSubjectTypesSupported()
	resp.IDTokenSigningAlgValuesSupported = proto.GetIdTokenSigningAlgValuesSupported()
	resp.SpiffeJwksURI = proto.GetSpiffeJwksUri()
}

// APITenantIdentityJWKS is the .well-known/jwks.json response body.
type APITenantIdentityJWKS struct {
	Keys []json.RawMessage `json:"keys"`
}

// APIReencryptTenantIdentitySecretsRequest is the POST /tenant-identity/reencrypt body.
// Both fields are optional: omitting organizationId targets all orgs; dryRun validates
// without writing.
type APIReencryptTenantIdentitySecretsRequest struct {
	OrganizationID *string `json:"organizationId,omitempty"`
	DryRun         bool    `json:"dryRun"`
}

// Validate enforces the request-local REST contract. Omitting organizationId targets
// all organizations; when supplied, it must be non-empty. The handler validates that
// the tenant organization has access to the selected Site.
func (req APIReencryptTenantIdentitySecretsRequest) Validate() error {
	return validation.ValidateStruct(&req,
		validation.Field(&req.OrganizationID,
			validation.When(req.OrganizationID != nil,
				validation.Required.Error("organizationId must not be empty"))),
	)
}

// ToProto converts the request to its gRPC form. organizationId comes from the body
// (not the path), so no org argument is taken.
func (req APIReencryptTenantIdentitySecretsRequest) ToProto() *corev1.ReencryptTenantIdentitySecretsRequest {
	return &corev1.ReencryptTenantIdentitySecretsRequest{
		OrganizationId: req.OrganizationID,
		DryRun:         req.DryRun,
	}
}

// APIReencryptTenantIdentityFailure describes one per-field re-wrap failure.
type APIReencryptTenantIdentityFailure struct {
	OrganizationID string `json:"organizationId"`
	Field          string `json:"field"`
	Error          string `json:"error"`
}

// APIReencryptTenantIdentitySecretsResponse is the POST /tenant-identity/reencrypt response body.
type APIReencryptTenantIdentitySecretsResponse struct {
	RowsExamined           int                                 `json:"rowsExamined"`
	RowsUpdated            int                                 `json:"rowsUpdated"`
	RowsSkippedAllOnTarget int                                 `json:"rowsSkippedAllOnTarget"`
	FieldsReencrypted      int                                 `json:"fieldsReencrypted"`
	FieldsSkippedOnTarget  int                                 `json:"fieldsSkippedOnTarget"`
	RowsFailed             int                                 `json:"rowsFailed"`
	Failures               []APIReencryptTenantIdentityFailure `json:"failures,omitempty"`
	CurrentEncryptionKeyID string                              `json:"currentEncryptionKeyId"`
}

// FromResponseProto populates the response from the gRPC reply.
func (resp *APIReencryptTenantIdentitySecretsResponse) FromResponseProto(proto *corev1.ReencryptTenantIdentitySecretsResponse) {
	if proto == nil {
		return
	}
	resp.RowsExamined = int(proto.GetRowsExamined())
	resp.RowsUpdated = int(proto.GetRowsUpdated())
	resp.RowsSkippedAllOnTarget = int(proto.GetRowsSkippedAllOnTarget())
	resp.FieldsReencrypted = int(proto.GetFieldsReencrypted())
	resp.FieldsSkippedOnTarget = int(proto.GetFieldsSkippedOnTarget())
	resp.RowsFailed = int(proto.GetRowsFailed())
	resp.CurrentEncryptionKeyID = proto.GetCurrentEncryptionKeyId()
	if failures := proto.GetFailures(); len(failures) > 0 {
		resp.Failures = make([]APIReencryptTenantIdentityFailure, 0, len(failures))
		for _, f := range failures {
			resp.Failures = append(resp.Failures, APIReencryptTenantIdentityFailure{
				OrganizationID: f.GetOrganizationId(),
				Field:          f.GetField(),
				Error:          f.GetError(),
			})
		}
	}
}
