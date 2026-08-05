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
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil)))

	org := "issuer-test-org"
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

// onlyIssuer returns the single Issuer row the fixture's database holds.
func onlyIssuer(t *testing.T, f testIssuerFixture) cdbm.Issuer {
	t.Helper()

	rows, err := cdbm.NewIssuerDAO(f.dbSession).GetAll(context.Background(), nil, cdbm.IssuerFilterInput{})
	require.NoError(t, err)
	require.Len(t, rows, 1)

	return rows[0]
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

	assert.Equal(t, http.StatusBadRequest, rec.Code)
	assert.Contains(t, rec.Body.String(), "kas-legacy")
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
			name:         "issuer_url_held_by_the_configmap",
			body:         createOrUpdateIssuerRequestWithJWKS("https://static.example.com", "https://clashing.example.com/jwks", "clashing-org"),
			wantBodyPart: "reserved by a statically-configured issuer",
		},
		{
			name:         "jwks_url_held_by_the_configmap",
			body:         createOrUpdateIssuerRequestWithJWKS("https://clashing.example.com", "https://static.example.com/jwks", "clashing-org"),
			wantBodyPart: "reserved by a statically-configured issuer",
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

	issuers, err := cdbm.NewIssuerDAO(f.dbSession).GetAll(context.Background(), nil, cdbm.IssuerFilterInput{})
	require.NoError(t, err)
	assert.Len(t, issuers, 1, "no rejected PUT may have written a row")
}

func TestCreateOrUpdateAuthIssuerHandler_Handle(t *testing.T) {
	t.Run("repeated identical PUT is a no-op", func(t *testing.T) {
		f := testIssuerSetup(t, customOnlyIssuerConfig(t))
		handler := NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg)
		body := `{"issuerUrl":"https://repeat.example.com","jwksUrl":"https://repeat.example.com/jwks","jwksTimeout":"7s","audiences":["nico"],"scopes":["openid"],"claimMappings":[{"orgName":"repeat-org","roles":["TENANT_ADMIN"]}]}`

		first := f.request(t, handler.Handle, http.MethodPut, "", body)
		require.Equal(t, http.StatusCreated, first.Code, first.Body.String())
		storedBefore := onlyIssuer(t, f)
		fetchedAt := time.Now().UTC().Truncate(time.Microsecond)
		require.NoError(t, cdbm.NewIssuerDAO(f.dbSession).UpdateJWKSCache(
			context.Background(), nil, storedBefore.ID, json.RawMessage(testIssuerKeySet), fetchedAt,
		))
		before, err := cdbm.NewIssuerDAO(f.dbSession).GetByID(context.Background(), nil, storedBefore.ID)
		require.NoError(t, err)

		repeated := f.request(t, handler.Handle, http.MethodPut, "", body)
		require.Equal(t, http.StatusOK, repeated.Code, repeated.Body.String())
		after := onlyIssuer(t, f)

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
		before := onlyIssuer(t, f)
		fetchedAt := time.Now().UTC().Truncate(time.Microsecond)
		require.NoError(t, cdbm.NewIssuerDAO(f.dbSession).UpdateJWKSCache(
			context.Background(), nil, before.ID, json.RawMessage(testIssuerKeySet), fetchedAt,
		))

		replaced := f.request(t, handler.Handle, http.MethodPut, "", replacementBody)
		require.Equal(t, http.StatusOK, replaced.Code, replaced.Body.String())
		after := onlyIssuer(t, f)

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
		before := onlyIssuer(t, f)
		require.NoError(t, cdbm.NewIssuerDAO(f.dbSession).UpdateJWKSCache(
			context.Background(), nil, before.ID, json.RawMessage(testIssuerKeySet), time.Now().UTC(),
		))

		replaced := f.request(t, handler.Handle, http.MethodPut, "",
			createOrUpdateIssuerRequestWithJWKS(issuerURL, issuerURL+"/new-jwks", "keys-org"))
		require.Equal(t, http.StatusOK, replaced.Code, replaced.Body.String())
		response := model.APIAuthIssuer{}
		require.NoError(t, json.Unmarshal(replaced.Body.Bytes(), &response))
		after := onlyIssuer(t, f)

		assert.Equal(t, before.ID, after.ID)
		assert.Empty(t, after.JWKSKeys)
		assert.Nil(t, after.JWKSFetchedAt)
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
		_ = onlyIssuer(t, f)
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
	issuers, err := cdbm.NewIssuerDAO(f.dbSession).GetAll(context.Background(), nil, cdbm.IssuerFilterInput{})
	require.NoError(t, err)
	assert.Len(t, issuers, 1)
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
		assert.Equal(t, http.StatusBadRequest, createRec.Code)
		assert.Contains(t, createRec.Body.String(), "duplicate org name")
	}

	issuers, err := cdbm.NewIssuerDAO(f.dbSession).GetAll(context.Background(), nil, cdbm.IssuerFilterInput{})
	require.NoError(t, err)
	assert.LessOrEqual(t, len(issuers), 1)
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
		assert.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
		assert.Contains(t, rec.Body.String(), "duplicate org name")
		conflicts++
	}
	assert.Equal(t, 1, successes)
	assert.Equal(t, 1, conflicts)
}

// TestCreateIssuerRejectsInternalHostsByDefault is the egress guard: the refresh
// loop fetches jwksUrl from inside the cluster, so a Provider Admin must not be
// able to aim it at an internal address unless the deployment opts in.
func TestCreateIssuerRejectsInternalHostsByDefault(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))
	f.cfg.TokenOriginConfig = cauth.NewTokenOriginConfig()

	body := createOrUpdateIssuerRequestWithJWKS("https://idp.acme.com", "http://169.254.169.254/latest/meta-data/", "acme")
	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "", body)
	assert.Equal(t, http.StatusBadRequest, rec.Code, rec.Body.String())
	assert.Contains(t, rec.Body.String(), "internal network addresses")

	issuers, err := cdbm.NewIssuerDAO(f.dbSession).GetAll(context.Background(), nil, cdbm.IssuerFilterInput{})
	require.NoError(t, err)
	assert.Empty(t, issuers, "a rejected PUT must not write a row")

	f.cfg.SetAuthAllowInternalIssuerHosts(true)
	rec = f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "", body)
	assert.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())
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
	f.cfg.SetAuthAllowInternalIssuerHosts(true) // the test IdP listens on loopback

	rec := f.request(t, NewCreateOrUpdateAuthIssuerHandler(f.dbSession, f.cfg).Handle, http.MethodPut, "",
		createOrUpdateIssuerRequestWithJWKS("https://idp.acme.com", idp.URL, "acme"))
	require.Equal(t, http.StatusCreated, rec.Code, rec.Body.String())

	assert.Equal(t, int32(0), hits.Load(), "creating an issuer must not reach its IdP")

	live := f.cfg.TokenOriginConfig.GetConfig("https://idp.acme.com")
	require.NotNil(t, live, "the new issuer is registered immediately, keyless")
	assert.Equal(t, 0, live.KeyCount())

	stored := onlyIssuer(t, f)
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

	stored := onlyIssuer(t, f)
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
	assert.Equal(t, 3, page.Total)
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

func TestDeleteIssuerErrors(t *testing.T) {
	f := testIssuerSetup(t, customOnlyIssuerConfig(t))
	handler := NewDeleteAuthIssuerHandler(f.dbSession, f.cfg)

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
			rec := f.request(t, handler.Handle, http.MethodDelete, tt.issuerID, "")
			assert.Equal(t, tt.wantStatus, rec.Code, rec.Body.String())
		})
	}
}
