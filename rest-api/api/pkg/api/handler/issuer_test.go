// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/internal/config"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/handler/util/common"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	authz "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/authorization"
	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const testIssuerKeySet = `{"keys":[{"kty":"RSA","use":"sig","kid":"created-key","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`

// testIssuerConfig builds a Config from an in-memory issuers block, so a test
// controls the static configuration the handlers gate on.
func testIssuerConfig(t *testing.T, yamlDoc string) *config.Config {
	t.Helper()
	cfg, err := config.NewConfigFromYAML(yamlDoc)
	require.NoError(t, err)
	return cfg
}

// customOnlyIssuerConfig is a Config with no privileged and no dynamic static
// issuers, so the runtime issuer API is available. The one custom entry is the
// bootstrap IdP a Provider Admin authenticates against; an empty issuers list
// would leave nothing to verify that token.
func customOnlyIssuerConfig(t *testing.T) *config.Config {
	t.Helper()
	return testIssuerConfig(t, `
keycloak:
  enabled: false
issuers:
  - issuer: https://provider.example.com
    jwks: https://provider.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: provider-org
        roles: [PROVIDER_ADMIN]
env:
  disconnected: true
`)
}

// testIssuerFixture is the Provider Admin caller used by Issuer handler tests.
type testIssuerFixture struct {
	org       string
	user      *cdbm.User
	dbSession *cdb.Session
	cfg       *config.Config
}

func testIssuerSetup(t *testing.T, cfg *config.Config) testIssuerFixture {
	t.Helper()

	dbSession := common.TestInitDB(t)
	t.Cleanup(dbSession.Close)
	common.TestSetupSchema(t, dbSession)
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil), (*cdbm.StatusDetail)(nil)))
	require.NoError(t, cfg.SyncConfigMapIssuers(context.Background(), dbSession))

	// provider-org is the org the bootstrap ConfigMap issuer grants PROVIDER_ADMIN
	// to in every test config, which is what the gate anchors on.
	org := "provider-org"
	user := common.TestBuildUser(t, dbSession, "issuer-test-starfleet-id", org, []string{authz.ProviderAdminRole})

	return testIssuerFixture{org: org, user: user, dbSession: dbSession, cfg: cfg}
}

// request invokes an Issuer handler as the fixture's Provider Admin. It uses a
// non-terminating assertion because concurrency tests call it from workers that
// must still deliver their response recorders when a handler returns an error.
func (f testIssuerFixture) request(t *testing.T, handler echo.HandlerFunc, method, issuerID, body string) *httptest.ResponseRecorder {
	t.Helper()

	e := echo.New()
	req := httptest.NewRequest(method, "/v2/org/"+f.org+"/nico/auth-issuer", strings.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName", "authIssuerId")
	ec.SetParamValues(f.org, issuerID)
	ec.Set("user", f.user)

	assert.NoError(t, handler(ec))
	return rec
}

// createOrUpdateIssuerRequest is a complete PUT body with one static org mapping.
func createOrUpdateIssuerRequest(issuerURL, orgName string) string {
	return fmt.Sprintf(`{"issuerUrl":%q,"jwksUrl":%q,"claimMappings":[{"orgName":%q,"roles":[%q]}]}`,
		issuerURL, issuerURL+"/jwks", orgName, authz.TenantAdminRole)
}

// createOrUpdateIssuerRequestWithJWKS is createOrUpdateIssuerRequest with the
// JWKS endpoint given explicitly, so a test can point it at a live or dead server.
func createOrUpdateIssuerRequestWithJWKS(issuerURL, jwksURL, orgName string) string {
	return fmt.Sprintf(`{"issuerUrl":%q,"jwksUrl":%q,"claimMappings":[{"orgName":%q,"roles":["TENANT_ADMIN"]}]}`,
		issuerURL, jwksURL, orgName)
}

// issuerByURL returns the live Issuer row for issuerURL.
func issuerByURL(t *testing.T, f testIssuerFixture, issuerURL string) cdbm.Issuer {
	t.Helper()

	row, err := cdbm.NewIssuerDAO(f.dbSession).GetByIssuerURL(context.Background(), nil, issuerURL)
	require.NoError(t, err)

	return *row
}

// apiIssuers returns the live api-sourced rows; the ConfigMap's own rows are
// written by the fixture's sync and are not under test.
func apiIssuers(t *testing.T, f testIssuerFixture) []cdbm.Issuer {
	t.Helper()

	rows, err := cdbm.NewIssuerDAO(f.dbSession).GetAll(context.Background(), nil, cdbm.IssuerFilterInput{})
	require.NoError(t, err)
	api := rows[:0]
	for _, row := range rows {
		if row.Source == cdbm.IssuerSourceAPI {
			api = append(api, row)
		}
	}

	return api
}

// createIssuer creates an Issuer through the API and returns the response body.
func (f testIssuerFixture) createIssuer(t *testing.T, issuerURL, orgName string) model.APIAuthIssuer {
	t.Helper()

	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
		createOrUpdateIssuerRequest(issuerURL, orgName))
	require.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())

	apiIssuer := model.APIAuthIssuer{}
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiIssuer))
	return apiIssuer
}

// TestAuthIssuerHandler_TenantAdminForbidden verifies the provider-admin gate: a
// Tenant Admin (org member, but not Provider Admin) is rejected with 403 before
// any DB access, so issuer management is provider-only.
func TestAuthIssuerHandler_TenantAdminForbidden(t *testing.T) {
	ctx := context.Background()
	ctx = common.TestCommonTraceProviderSetup(t, ctx)

	org := "acme-org"
	user := &cdbm.User{
		ID:          uuid.New(),
		AuxiliaryID: cutil.GetPtr("aux-tenant-admin"),
		OrgData: cdbm.OrgData{
			org: cdbm.Org{Name: org, Roles: []string{authz.TenantAdminRole}},
		},
	}

	// dbSession is never touched on the 403 path (the gate rejects first).
	h := NewCreateOrUpdateAuthIssuerHandler(&cdb.Session{}, customOnlyIssuerConfig(t))

	e := echo.New()
	body := `{"issuerUrl":"https://idp.acme.com"}`
	req := httptest.NewRequest(http.MethodPut, "/v2/org/"+org+"/nico/auth-issuer", strings.NewReader(body))
	req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(org)
	ec.Set("user", user)

	ec.SetRequest(ec.Request().WithContext(ctx))

	err := h.Handle(ec)
	assert.NoError(t, err)
	assert.Equal(t, http.StatusForbidden, rec.Code)
}

// TestAuthIssuerHandlerRequiresAConfigMapGrantedOrg covers the anchor on the
// gate: a Provider Admin of an org that no ConfigMap issuer grants PROVIDER_ADMIN
// to gets 403 on every route, so a PROVIDER_ADMIN grant handed out by an API
// issuer's claim mapping does not by itself reach the trust plane.
func TestAuthIssuerHandlerRequiresAConfigMapGrantedOrg(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))
	existing := f.createIssuer(t, "https://existing.example.com", "existing-org")

	other := f
	other.org = "api-granted-org"
	other.user = common.TestBuildUser(t, f.dbSession, "api-granted-starfleet-id", other.org, []string{authz.ProviderAdminRole})

	routes := []struct {
		name    string
		handler echo.HandlerFunc
		method  string
		id      string
		body    string
	}{
		{name: "put", handler: NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, method: http.MethodPut, body: createOrUpdateIssuerRequest("https://new.example.com", "new-org")},
		{name: "get all", handler: NewGetAllAuthIssuerHandler(f.dbSession, f.cfg).Handle, method: http.MethodGet},
		{name: "get", handler: NewGetAuthIssuerHandler(f.dbSession, f.cfg).Handle, method: http.MethodGet, id: existing.ID},
		{name: "delete", handler: NewDeleteAuthIssuerHandler(f.dbSession, f.cfg).Handle, method: http.MethodDelete, id: existing.ID},
	}
	for _, r := range routes {
		t.Run(r.name, func(t *testing.T) {
			rec := other.request(t, r.handler, r.method, r.id, r.body)
			assert.Equal(t, http.StatusForbidden, rec.Code, rec.Body.String())
			assert.Contains(t, rec.Body.String(), authIssuerOrgNotAnchoredMessage)
		})
	}
	assert.Len(t, apiIssuers(t, f), 1, "nothing was written or deleted")
}

// TestCreateIssuerRejectsPrivilegedStaticOrigins: a privileged ConfigMap origin
// owns claim extraction and cannot be combined with a custom issuer, so no org is
// granted Provider Admin by a ConfigMap claim mapping and the gate refuses every
// caller before the privileged-origin check is reached.
func TestCreateIssuerRejectsPrivilegedStaticOrigins(t *testing.T) {
	f := testIssuerSetup(t, testIssuerConfig(t, `
env:
  disconnected: true
issuers:
  - origin: kas-legacy
    jwks: https://authn.example.com/jwks
    issuer: authn.example.com
`))

	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
		createOrUpdateIssuerRequest("https://idp.acme.com", "acme"))

	assert.Equal(t, http.StatusForbidden, rec.Code, rec.Body.String())
	assert.Contains(t, rec.Body.String(), authIssuerOrgNotAnchoredMessage)
}

func TestCreateIssuerRejectsDynamicClaimMappings(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))

	body := `{"issuerUrl":"https://dyn-attempt.example.com","claimMappings":[{"orgName":"acme","rolesAttribute":"roles"}]}`
	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "", body)

	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Contains(t, rec.Body.String(), "rolesAttribute")
}

// TestCreateIssuerForcesCustomOrigin verifies that an origin supplied in the
// request body cannot select a privileged token processor.
func TestCreateIssuerForcesCustomOrigin(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))

	body := fmt.Sprintf(`{"issuerUrl":"https://forced-custom.example.com","origin":%q}`, cauth.TokenOriginKeycloak)
	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "", body)
	require.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())

	apiIssuer := model.APIAuthIssuer{}
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiIssuer))
	assert.Equal(t, cauth.TokenOriginCustom, apiIssuer.Origin)
}

// TestCreateOrUpdateIssuerRejectsIdentityConflicts covers identity claims that
// belong to a different resource. A live row with the same issuerUrl is the PUT
// target and is therefore updated rather than treated as a conflict.
func TestCreateOrUpdateIssuerRejectsIdentityConflicts(t *testing.T) {
	f := testIssuerSetup(t, testIssuerConfig(t, `
env:
  disconnected: true
issuers:
  - issuer: https://provider.example.com
    jwks: https://provider.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: provider-org
        roles: [PROVIDER_ADMIN]
  - issuer: https://static.example.com
    jwks: https://static.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: static-org
        roles: [TENANT_ADMIN]
`))
	f.createIssuer(t, "https://existing.example.com", "existing-org")

	tests := []struct {
		name         string
		body         string
		wantBodyPart string
	}{
		{
			name:         "issuer_url_held_by_the_configmap_with_another_jwks",
			body:         createOrUpdateIssuerRequestWithJWKS("https://static.example.com", "https://clashing.example.com/jwks", "clashing-org"),
			wantBodyPart: "jwksUrl is defined by the ConfigMap issuer",
		},
		{
			name:         "jwks_url_held_by_the_configmap",
			body:         createOrUpdateIssuerRequestWithJWKS("https://clashing.example.com", "https://static.example.com/jwks", "clashing-org"),
			wantBodyPart: "duplicate JWKS URL",
		},
		{
			name:         "jwks_url_held_by_a_live_row",
			body:         createOrUpdateIssuerRequestWithJWKS("https://clashing.example.com", "https://existing.example.com/jwks", "clashing-org"),
			wantBodyPart: "duplicate JWKS URL",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "", tt.body)

			assert.Equal(t, http.StatusConflict, rec.Code, rec.Body.String())
			assert.Contains(t, rec.Body.String(), tt.wantBodyPart)
		})
	}

	assert.Len(t, apiIssuers(t, f), 1, "no rejected PUT may have written a row")
}

// TestCreateOrUpdateIssuerAddsMappingsToConfigMapIssuer covers a PUT whose
// issuerUrl is a ConfigMap issuer: the ConfigMap's definition and mappings stay,
// the request's mappings are stored as api-sourced, and a change to a
// ConfigMap-owned value is a 409.
func TestCreateOrUpdateIssuerAddsMappingsToConfigMapIssuer(t *testing.T) {
	f := testIssuerSetup(t, testIssuerConfig(t, `
env:
  disconnected: true
issuers:
  - issuer: https://provider.example.com
    jwks: https://provider.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: provider-org
        roles: [PROVIDER_ADMIN]
  - issuer: https://static.example.com
    jwks: https://static.example.com/jwks
    origin: custom
    audiences: [nico]
    claimMappings:
      - orgName: static-org
        roles: [TENANT_ADMIN]
`))
	handler := NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg)

	rec := f.request(t, handler.Handle, http.MethodPut, "",
		`{"issuerUrl":"https://static.example.com","claimMappings":[{"orgName":"added-org","roles":["TENANT_ADMIN"]}]}`)
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())

	apiIssuer := model.APIAuthIssuer{}
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiIssuer))
	assert.Equal(t, cdbm.IssuerSourceConfig, apiIssuer.Source)
	assert.Equal(t, "https://static.example.com/jwks", apiIssuer.JWKSUrl, "an omitted jwksUrl inherits the ConfigMap's")
	assert.Equal(t, []string{"nico"}, apiIssuer.Audiences)
	require.Len(t, apiIssuer.ClaimMappings, 2)
	assert.Equal(t, "static-org", apiIssuer.ClaimMappings[0].OrgName, "the ConfigMap mapping comes first")
	assert.Equal(t, cdbm.IssuerSourceConfig, apiIssuer.ClaimMappings[0].Source)
	assert.Equal(t, "added-org", apiIssuer.ClaimMappings[1].OrgName)
	assert.Equal(t, cdbm.IssuerSourceAPI, apiIssuer.ClaimMappings[1].Source)

	rec = f.request(t, handler.Handle, http.MethodPut, "",
		`{"issuerUrl":"https://static.example.com","audiences":["other"],"claimMappings":[{"orgName":"added-org","roles":["TENANT_ADMIN"]}]}`)
	assert.Equal(t, http.StatusConflict, rec.Code, rec.Body.String())
	assert.Contains(t, rec.Body.String(), "audiences is defined by the ConfigMap issuer")

	assert.Len(t, issuerByURL(t, f, "https://static.example.com").ClaimMappings, 2, "the refused request wrote nothing")

	// A GET body echoed back carries the ConfigMap's own mappings, Provider grants
	// included; identical ones are a no-op, a changed one is refused.
	rec = f.request(t, handler.Handle, http.MethodPut, "",
		`{"issuerUrl":"https://provider.example.com","claimMappings":[{"orgName":"provider-org","roles":["PROVIDER_ADMIN"]},{"orgName":"tenant-org","roles":["TENANT_ADMIN"]}]}`)
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
	require.Len(t, issuerByURL(t, f, "https://provider.example.com").ClaimMappings, 2)

	rec = f.request(t, handler.Handle, http.MethodPut, "",
		`{"issuerUrl":"https://provider.example.com","claimMappings":[{"orgName":"provider-org","roles":["PROVIDER_ADMIN","TENANT_ADMIN"]}]}`)
	assert.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
	assert.Contains(t, rec.Body.String(), "PROVIDER_ADMIN")
}

// TestCreateOrUpdateIssuerKeepsTheRowsConfigMapDefinition: during a rollout a
// replica still on the previous ConfigMap handles a PUT. The row, written by the
// newer ConfigMap, is the authority on every config-owned field, so the stale
// replica's own ConfigMap must not be copied back into it.
func TestCreateOrUpdateIssuerKeepsTheRowsConfigMapDefinition(t *testing.T) {
	issuers := func(audience string) string {
		return fmt.Sprintf(`
env:
  disconnected: true
issuers:
  - issuer: https://provider.example.com
    jwks: https://provider.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: provider-org
        roles: [PROVIDER_ADMIN]
  - issuer: https://static.example.com
    jwks: https://static.example.com/jwks
    origin: custom
    audiences: [%s]
    claimMappings:
      - orgName: static-org
        roles: [TENANT_ADMIN]
`, audience)
	}
	f := testIssuerSetup(t, testIssuerConfig(t, issuers("new")))
	stale := testIssuerConfig(t, issuers("old"))

	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, stale).Handle, http.MethodPut, "",
		`{"issuerUrl":"https://static.example.com","claimMappings":[{"orgName":"added-org","roles":["TENANT_ADMIN"]}]}`)

	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
	assert.Equal(t, []string{"new"}, issuerByURL(t, f, "https://static.example.com").Audiences)
}

func TestCreateOrUpdateAuthIssuerHandler_Handle(t *testing.T) {
	t.Run("repeated identical PUT is a no-op", func(t *testing.T) {
		f := testIssuerSetup(t, customOnlyIssuerConfig(t))
		handler := NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg)
		body := `{"issuerUrl":"https://repeat.example.com","jwksUrl":"https://repeat.example.com/jwks","jwksTimeout":"7s","audiences":["nico"],"scopes":["openid"],"claimMappings":[{"orgName":"repeat-org","roles":["TENANT_ADMIN"]}]}`

		first := f.request(t, handler.Handle, http.MethodPut, "", body)
		require.Equal(t, http.StatusCreated, first.Code, first.Body.String())
		storedBefore := issuerByURL(t, f, "https://repeat.example.com")
		fetchedAt := time.Now().UTC().Truncate(time.Microsecond)
		require.NoError(t, cdbm.NewIssuerDAO(f.dbSession).UpdateJWKSCache(
			context.Background(), nil, storedBefore.ID, storedBefore.JWKSUrl, json.RawMessage(testIssuerKeySet), fetchedAt,
		))
		before, err := cdbm.NewIssuerDAO(f.dbSession).GetByID(context.Background(), nil, storedBefore.ID)
		require.NoError(t, err)

		repeated := f.request(t, handler.Handle, http.MethodPut, "", body)
		require.Equal(t, http.StatusOK, repeated.Code, repeated.Body.String())
		after := issuerByURL(t, f, "https://repeat.example.com")

		assert.Equal(t, before.ID, after.ID)
		assert.Equal(t, before.CreatedAt, after.CreatedAt)
		assert.Equal(t, before.UpdatedAt, after.UpdatedAt)
		assert.JSONEq(t, string(before.JWKSKeys), string(after.JWKSKeys))
		assert.Equal(t, before.JWKSFetchedAt, after.JWKSFetchedAt)
	})

	t.Run("replacement preserves identity and same-endpoint keys", func(t *testing.T) {
		f := testIssuerSetup(t, customOnlyIssuerConfig(t))
		handler := NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg)
		issuerURL := "https://replace.example.com"
		jwksURL := issuerURL + "/jwks"
		originalBody := fmt.Sprintf(`{"issuerUrl":%q,"jwksUrl":%q,"audiences":["old-audience"],"scopes":["old-scope"],"claimMappings":[{"orgName":"old-org","roles":["TENANT_ADMIN"]}]}`, issuerURL, jwksURL)
		replacementBody := fmt.Sprintf(`{"issuerUrl":%q,"jwksUrl":%q,"jwksTimeout":"9s","audiences":["new-audience"],"claimMappings":[{"orgName":"new-org","roles":["TENANT_ADMIN"]}]}`, issuerURL, jwksURL)

		created := f.request(t, handler.Handle, http.MethodPut, "", originalBody)
		require.Equal(t, http.StatusCreated, created.Code, created.Body.String())
		before := issuerByURL(t, f, issuerURL)
		fetchedAt := time.Now().UTC().Truncate(time.Microsecond)
		require.NoError(t, cdbm.NewIssuerDAO(f.dbSession).UpdateJWKSCache(
			context.Background(), nil, before.ID, before.JWKSUrl, json.RawMessage(testIssuerKeySet), fetchedAt,
		))

		replaced := f.request(t, handler.Handle, http.MethodPut, "", replacementBody)
		require.Equal(t, http.StatusOK, replaced.Code, replaced.Body.String())
		after := issuerByURL(t, f, issuerURL)

		assert.Equal(t, before.ID, after.ID)
		assert.Equal(t, before.CreatedAt, after.CreatedAt)
		assert.Equal(t, "9s", after.JWKSTimeout)
		assert.Equal(t, []string{"new-audience"}, after.Audiences)
		assert.Empty(t, after.Scopes, "PUT replacement clears omitted fields")
		require.Len(t, after.ClaimMappings, 1)
		assert.Equal(t, "new-org", after.ClaimMappings[0].OrgName)
		assert.JSONEq(t, testIssuerKeySet, string(after.JWKSKeys))
		require.NotNil(t, after.JWKSFetchedAt)
		assert.True(t, fetchedAt.Equal(*after.JWKSFetchedAt))
	})

	t.Run("JWKS endpoint replacement clears cached keys", func(t *testing.T) {
		f := testIssuerSetup(t, customOnlyIssuerConfig(t))
		handler := NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg)
		issuerURL := "https://keys-reset.example.com"

		created := f.request(t, handler.Handle, http.MethodPut, "",
			createOrUpdateIssuerRequestWithJWKS(issuerURL, issuerURL+"/old-jwks", "keys-org"))
		require.Equal(t, http.StatusCreated, created.Code, created.Body.String())
		before := issuerByURL(t, f, issuerURL)
		require.NoError(t, cdbm.NewIssuerDAO(f.dbSession).UpdateJWKSCache(
			context.Background(), nil, before.ID, before.JWKSUrl, json.RawMessage(testIssuerKeySet), time.Now().UTC(),
		))

		replaced := f.request(t, handler.Handle, http.MethodPut, "",
			createOrUpdateIssuerRequestWithJWKS(issuerURL, issuerURL+"/new-jwks", "keys-org"))
		require.Equal(t, http.StatusOK, replaced.Code, replaced.Body.String())
		response := model.APIAuthIssuer{}
		require.NoError(t, json.Unmarshal(replaced.Body.Bytes(), &response))
		after := issuerByURL(t, f, issuerURL)

		assert.Equal(t, before.ID, after.ID)
		assert.Empty(t, after.JWKSKeys)
		assert.Nil(t, after.JWKSFetchedAt)
		assert.Nil(t, after.JWKSLastError, "the old endpoint's outcome does not describe the new one")
		assert.Equal(t, model.AuthIssuerStatusPending, response.Status)
	})

	t.Run("concurrent repeated PUTs create one row", func(t *testing.T) {
		f := testIssuerSetup(t, customOnlyIssuerConfig(t))
		handler := NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg)
		body := createOrUpdateIssuerRequest("https://concurrent-repeat.example.com", "concurrent-repeat-org")
		start := make(chan struct{})
		results := make(chan *httptest.ResponseRecorder, 2)

		for range 2 {
			go func() {
				<-start
				results <- f.request(t, handler.Handle, http.MethodPut, "", body)
			}()
		}
		close(start)

		codes := map[int]int{}
		codes[(<-results).Code]++
		codes[(<-results).Code]++
		assert.Equal(t, 1, codes[http.StatusCreated])
		assert.Equal(t, 1, codes[http.StatusOK])
		assert.Len(t, apiIssuers(t, f), 1)
	})
}

func TestCreateIssuerSerializesCombinedValidation(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))

	orgName := "concurrent-create-" + uuid.NewString()
	start := make(chan struct{})
	results := make(chan *httptest.ResponseRecorder, 2)

	for _, issuerURL := range []string{"https://create-a.example.com", "https://create-b.example.com"} {
		go func(issuerURL string) {
			<-start
			results <- f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
				createOrUpdateIssuerRequest(issuerURL, orgName))
		}(issuerURL)
	}
	close(start)

	assertOneIssuerMutationConflict(t, http.StatusCreated, <-results, <-results)
	assert.Len(t, apiIssuers(t, f), 1)
}

func TestDeleteIssuerSerializesWithConflictingCreate(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))

	orgName := "concurrent-delete-" + uuid.NewString()
	existing := f.createIssuer(t, "https://delete-existing.example.com", orgName)

	start := make(chan struct{})
	deleteResult := make(chan *httptest.ResponseRecorder, 1)
	createResult := make(chan *httptest.ResponseRecorder, 1)
	go func() {
		<-start
		deleteResult <- f.request(t, NewDeleteAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodDelete, existing.ID, "")
	}()
	go func() {
		<-start
		createResult <- f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
			createOrUpdateIssuerRequest("https://delete-replacement.example.com", orgName))
	}()
	close(start)

	deleteRec := <-deleteResult
	require.Equal(t, http.StatusOK, deleteRec.Code, deleteRec.Body.String())

	// The create either lands after the delete (201) or is rejected because the
	// org name is still claimed by the Issuer being deleted.
	createRec := <-createResult
	if createRec.Code != http.StatusCreated {
		assert.Equal(t, http.StatusConflict, createRec.Code)
		assert.Contains(t, createRec.Body.String(), "an org may belong to exactly one issuer")
	}

	assert.LessOrEqual(t, len(apiIssuers(t, f)), 1)
}

// assertOneIssuerMutationConflict asserts that exactly one of two concurrent
// mutations succeeded and the other was rejected for claiming the same static
// org name, which is what the issuer organization-mapping lock guarantees.
func assertOneIssuerMutationConflict(t *testing.T, successCode int, recs ...*httptest.ResponseRecorder) {
	t.Helper()
	successes := 0
	conflicts := 0
	for _, rec := range recs {
		if rec.Code == successCode {
			successes++
			continue
		}
		assert.Equal(t, http.StatusConflict, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "an org may belong to exactly one issuer")
		conflicts++
	}
	assert.Equal(t, 1, successes)
	assert.Equal(t, 1, conflicts)
}

// TestCreateIssuerDoesNotContactTheIdP pins the write path as free of network
// I/O. The inline reload that follows a create only hydrates from the row, so
// registering an issuer costs no IdP round-trip and cannot be slowed by one; the
// refresh loop, the pending retry loop, and the first token naming the issuer are
// what fill the key cache.
func TestCreateIssuerDoesNotContactTheIdP(t *testing.T) {
	var hits atomic.Int32
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(testIssuerKeySet))
	}))
	defer idp.Close()

	f := testIssuerSetup(t, customOnlyIssuerConfig(t))
	f.cfg.TokenOriginConfig = cauth.NewTokenOriginConfig()

	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
		createOrUpdateIssuerRequestWithJWKS("https://idp.acme.com", idp.URL, "acme"))
	require.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())

	assert.Equal(t, int32(0), hits.Load(), "creating an issuer must not reach its IdP")

	live := f.cfg.TokenOriginConfig.GetConfig("https://idp.acme.com")
	require.NotNil(t, live, "the new issuer is registered immediately, keyless")
	assert.Equal(t, 0, live.KeyCount())

	stored := issuerByURL(t, f, "https://idp.acme.com")
	assert.Empty(t, stored.JWKSKeys)
	assert.Nil(t, stored.JWKSFetchedAt)
}

// TestCreateIssuerWithUnreachableIdP is the availability guarantee on the write
// path: registering an issuer must not depend on its IdP being up at that moment.
func TestCreateIssuerWithUnreachableIdP(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))
	f.cfg.TokenOriginConfig = cauth.NewTokenOriginConfig()

	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
		createOrUpdateIssuerRequestWithJWKS("https://idp.acme.com", "http://203.0.113.1:1/jwks", "acme"))
	require.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())

	// The response is what tells the operator the issuer cannot verify tokens yet,
	// which is the whole reason the create is allowed to succeed without the IdP.
	created := model.APIAuthIssuer{}
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &created))
	assert.Equal(t, model.AuthIssuerStatusPending, created.Status)
	assert.Nil(t, created.JWKSFetchedAt)

	stored := issuerByURL(t, f, "https://idp.acme.com")
	assert.Empty(t, stored.JWKSKeys, "an unreachable IdP leaves the cache empty for the refresh loop to fill")
	assert.Nil(t, stored.JWKSFetchedAt)
}

func TestGetAllIssuerPaginatesDeterministically(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))
	for _, name := range []string{"charlie", "alpha", "bravo"} {
		f.createIssuer(t, "https://"+name+".example.com", name)
	}

	e := echo.New()
	req := httptest.NewRequest(http.MethodGet,
		"/v2/org/"+f.org+"/nico/auth-issuer?pageNumber=2&pageSize=1&orderBy=ISSUER_URL_ASC", nil)
	rec := httptest.NewRecorder()
	ec := e.NewContext(req, rec)
	ec.SetParamNames("orgName")
	ec.SetParamValues(f.org)
	ec.Set("user", f.user)

	require.NoError(t, NewGetAllAuthIssuerHandler(f.dbSession, f.cfg).Handle(ec))
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())

	issuers := []model.APIAuthIssuer{}
	require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &issuers))
	require.Len(t, issuers, 1)
	assert.Equal(t, "https://bravo.example.com", issuers[0].IssuerURL)

	page := pagination.PageResponse{}
	require.NoError(t, json.Unmarshal([]byte(rec.Header().Get(pagination.ResponseHeaderName)), &page))
	assert.Equal(t, 2, page.PageNumber)
	assert.Equal(t, 1, page.PageSize)
	assert.Equal(t, 4, page.Total, "the bootstrap ConfigMap issuer is a row too")
	require.NotNil(t, page.OrderBy)
	assert.Equal(t, "ISSUER_URL_ASC", *page.OrderBy)
}

func TestGetIssuerErrors(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))
	handler := NewGetAuthIssuerHandler(f.dbSession, f.cfg)

	tests := []struct {
		name       string
		issuerID   string
		wantStatus int
	}{
		{name: "malformed ID", issuerID: "not-a-uuid", wantStatus: http.StatusBadRequest},
		{name: "missing issuer", issuerID: uuid.NewString(), wantStatus: http.StatusNotFound},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := f.request(t, handler.Handle, http.MethodGet, tt.issuerID, "")
			assert.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())
		})
	}
}

func TestDeleteAuthIssuerHandler_Handle(t *testing.T) {
	f := testIssuerSetup(t, testIssuerConfig(t, `
env:
  disconnected: true
issuers:
  - issuer: https://provider.example.com
    jwks: https://provider.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: provider-org
        roles: [PROVIDER_ADMIN]
  - issuer: https://static.example.com
    jwks: https://static.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: static-org
        roles: [TENANT_ADMIN]
`))
	handler := NewDeleteAuthIssuerHandler(f.dbSession, f.cfg)

	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
		`{"issuerUrl":"https://static.example.com","claimMappings":[{"orgName":"added-org","roles":["TENANT_ADMIN"]}]}`)
	require.Equal(t, http.StatusOK, rec.Code, rec.Body.String())
	staticBefore := issuerByURL(t, f, "https://static.example.com")
	require.Len(t, staticBefore.ClaimMappings, 2)
	providerBefore := issuerByURL(t, f, "https://provider.example.com")
	apiIssuer := f.createIssuer(t, "https://api-only.example.com", "api-only-org")

	tests := []struct {
		name       string
		issuerID   string
		wantStatus int
		check      func(t *testing.T, rec *httptest.ResponseRecorder)
	}{
		{name: "malformed ID", issuerID: "not-a-uuid", wantStatus: http.StatusBadRequest},
		{name: "missing issuer", issuerID: uuid.NewString(), wantStatus: http.StatusNotFound},
		{
			name:       "configmap issuer withdraws only its api claim mappings",
			issuerID:   staticBefore.ID.String(),
			wantStatus: http.StatusOK,
			check: func(t *testing.T, rec *httptest.ResponseRecorder) {
				apiIssuer := model.APIAuthIssuer{}
				require.NoError(t, json.Unmarshal(rec.Body.Bytes(), &apiIssuer))
				assert.Equal(t, cdbm.IssuerSourceConfig, apiIssuer.Source)
				require.Len(t, apiIssuer.ClaimMappings, 1)
				assert.Equal(t, "static-org", apiIssuer.ClaimMappings[0].OrgName)

				after := issuerByURL(t, f, "https://static.example.com")
				assert.Equal(t, staticBefore.ID, after.ID, "the row is updated, not deleted")
				require.Len(t, after.ClaimMappings, 1)
				assert.Equal(t, cdbm.IssuerSourceConfig, after.ClaimMappings[0].Source)
				assert.Equal(t, staticBefore.JWKSUrl, after.JWKSUrl)

				history, err := cdbm.NewStatusDetailDAO(f.dbSession).GetRecentByEntityIDs(context.Background(), nil, []string{after.ID.String()}, 1)
				require.NoError(t, err)
				require.Len(t, history, 1)
				require.NotNil(t, history[0].Message)
				assert.Contains(t, *history[0].Message, "DELETE by user "+f.user.ID.String())
				assert.Contains(t, *history[0].Message, "added-org")
			},
		},
		{
			name:       "configmap issuer without api claim mappings is a no-op",
			issuerID:   providerBefore.ID.String(),
			wantStatus: http.StatusOK,
			check: func(t *testing.T, rec *httptest.ResponseRecorder) {
				after := issuerByURL(t, f, "https://provider.example.com")
				assert.Equal(t, providerBefore.Signature(), after.Signature(), "nothing changed")
				assert.Equal(t, providerBefore.UpdatedAt, after.UpdatedAt, "the row was not written")

				history, err := cdbm.NewStatusDetailDAO(f.dbSession).GetRecentByEntityIDs(context.Background(), nil, []string{after.ID.String()}, 1)
				require.NoError(t, err)
				for _, h := range history {
					require.NotNil(t, h.Message)
					assert.NotContains(t, *h.Message, "DELETE by user", "a no-op records no history")
				}
			},
		},
		{
			name:       "api issuer is deleted",
			issuerID:   apiIssuer.ID,
			wantStatus: http.StatusOK,
			check: func(t *testing.T, rec *httptest.ResponseRecorder) {
				_, err := cdbm.NewIssuerDAO(f.dbSession).GetByIssuerURL(context.Background(), nil, "https://api-only.example.com")
				assert.ErrorIs(t, err, cdb.ErrDoesNotExist)
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := f.request(t, handler.Handle, http.MethodDelete, tt.issuerID, "")
			assert.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())
			if tt.check != nil {
				tt.check(t, rec)
			}
		})
	}
}
