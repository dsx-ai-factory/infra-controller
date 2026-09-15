// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package processors

import (
	"bytes"
	"context"
	"crypto/sha256"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/roles"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const testStarfleetKey = nvapiPrefix + "0123456789012345678901234567890123456789012345678901234567890123"

type kasMetricsRecorder func(operation, result string)

func (record kasMetricsRecorder) RecordNGCRequest(operation, result string) {
	record(operation, result)
}

// testKasResolverDB resets the user table so each resolver test starts from a known state.
func testKasResolverDB(t *testing.T) *cdb.Session {
	dbSession := cdbu.TestInitDB(t)
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)
	return dbSession
}

func TestValidateCredential(t *testing.T) {
	tests := []struct {
		name    string
		raw     string
		wantErr bool
	}{
		{name: "starfleet key", raw: testStarfleetKey},
		{name: "payload too short", raw: nvapiPrefix + strings.Repeat("x", nvapiPayloadLen-1), wantErr: true},
		{name: "payload too long", raw: nvapiPrefix + strings.Repeat("x", nvapiPayloadLen+1), wantErr: true},
		{name: "missing prefix", raw: strings.Repeat("x", nvapiPayloadLen), wantErr: true},
		{name: "empty credential", raw: "", wantErr: true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := validateCredential(tt.raw)
			if tt.wantErr {
				assert.ErrorIs(t, err, errInvalidKeyFormat)
				return
			}
			assert.NoError(t, err)
		})
	}
}

func TestResolverFetchIdentity(t *testing.T) {
	const personalResponse = `{"userId":"personal-user-id","orgName":"Provider-ORG","type":"PERSONAL_KEY",` +
		`"products":["forge-provider"],"user":{"starfleetId":"starfleet-id","email":"user@test.com","name":"Test User",` +
		`"roles":[{"org":{"id":1,"name":"provider-org","displayName":"Provider Org"},"orgRoles":["` + roles.ProviderAdminRole + `"]},` +
		`{"org":{"id":2,"name":"other-org"},"orgRoles":["MEMBER"]}]}}`
	const serviceResponse = `{"userId":"service-user-id","orgName":"provider-org","type":"SERVICE_KEY",` +
		`"products":["forge-provider","forge-tenant","unmapped-product"]}`

	tests := []struct {
		name     string
		response string
		urlOrg   string
		check    func(t *testing.T, id *identity)
		wantErr  error
	}{
		{
			name:     "personal key narrows to the org the route names",
			response: personalResponse,
			urlOrg:   "other-org",
			check: func(t *testing.T, id *identity) {
				require.NotNil(t, id.starfleetID)
				assert.Equal(t, "starfleet-id", *id.starfleetID)
				assert.Nil(t, id.auxiliaryID)
				assert.Equal(t, "user@test.com", id.email)
				assert.Equal(t, "Test", id.firstName)
				assert.Equal(t, "User", id.lastName)
				// The route, not the caller orgName "Provider-ORG", is what scopes this
				assert.Equal(t, cdbm.OrgData{"other-org": cdbm.Org{
					ID:    2,
					Name:  "other-org",
					Roles: []string{"MEMBER"},
					Teams: []cdbm.Team{},
				}}, id.orgData)
			},
		},
		{
			name:     "personal key holding no roles in the route org",
			response: personalResponse,
			urlOrg:   "third-org",
			wantErr:  errOrgNotGranted,
		},
		{
			name: "org names are stored in lower case",
			response: `{"userId":"personal-user-id","type":"PERSONAL_KEY","user":{"starfleetId":"starfleet-id",` +
				`"roles":[{"org":{"id":1,"name":"Provider-ORG"},"orgRoles":["` + roles.ProviderAdminRole + `"]}]}}`,
			urlOrg: "provider-org",
			check: func(t *testing.T, id *identity) {
				require.Len(t, id.orgData, 1)
				org, found := id.orgData["provider-org"]
				require.True(t, found, "the map key must be lowered")
				assert.Equal(t, "provider-org", org.Name, "the org name must be lowered too")
			},
		},
		{
			name:     "service key identity comes from the caller userId and products",
			response: serviceResponse,
			urlOrg:   "provider-org",
			check: func(t *testing.T, id *identity) {
				require.NotNil(t, id.auxiliaryID)
				assert.Equal(t, "service-user-id", *id.auxiliaryID)
				assert.Nil(t, id.starfleetID)
				require.Len(t, id.orgData, 1)
				org, err := id.orgData.GetOrgByName("provider-org")
				require.NoError(t, err)
				assert.ElementsMatch(t, []string{roles.ProviderAdminRole, roles.TenantAdminRole}, org.Roles)
			},
		},
		{
			name:     "service key issued against another org",
			response: serviceResponse,
			urlOrg:   "other-org",
			wantErr:  errOrgNotGranted,
		},
		{
			name:     "personal key without a user",
			response: `{"userId":"personal-user-id","orgName":"provider-org","type":"PERSONAL_KEY"}`,
			urlOrg:   "provider-org",
			wantErr:  errNgcUpstream,
		},
		{
			name: "personal key whose user has no starfleetId",
			response: `{"userId":"personal-user-id","orgName":"provider-org","type":"PERSONAL_KEY",` +
				`"user":{"email":"user@test.com"}}`,
			urlOrg:  "provider-org",
			wantErr: errNgcUpstream,
		},
		{
			name:     "service key without a userId",
			response: `{"orgName":"provider-org","type":"SERVICE_KEY","products":["forge-provider"]}`,
			urlOrg:   "provider-org",
			wantErr:  errNgcUpstream,
		},
		{
			name:     "service key without an org",
			response: `{"userId":"service-user-id","type":"SERVICE_KEY","products":["forge-provider"]}`,
			urlOrg:   "provider-org",
			wantErr:  errNgcUpstream,
		},
		{
			name:     "unrecognized key type",
			response: `{"userId":"id","orgName":"provider-org","type":"CLOUD_KEY"}`,
			urlOrg:   "provider-org",
			wantErr:  errNgcUpstream,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			requests := 0
			server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
				requests++
				assert.Equal(t, callerInfoPath, req.URL.Path)
				assert.Equal(t, testStarfleetKey, req.PostFormValue("credentials"))
				_, err := res.Write([]byte(tt.response))
				require.NoError(t, err)
			}))
			defer server.Close()

			id, err := (&resolver{ngc: &ngcClient{
				http:    server.Client(),
				baseURL: server.URL,
			}}).fetchIdentity(context.Background(), testStarfleetKey, tt.urlOrg)

			assert.Equal(t, 1, requests)
			if tt.wantErr != nil {
				assert.ErrorIs(t, err, tt.wantErr)
				assert.NotErrorIs(t, err, errKeyRejected, "the credential itself is still valid")
				return
			}
			require.NoError(t, err)
			tt.check(t, id)
		})
	}
}

func TestResolverCreateOrUpdateUser(t *testing.T) {
	// The seeded record holds one org the credential may still report, carrying
	// metadata only a richer source such as the kas-legacy workflow can supply, and
	// one org no credential below reports
	staleUpdated := time.Now().UTC().Add(-time.Hour)
	seededOrgData := cdbm.OrgData{
		"provider-org": cdbm.Org{
			ID:          7,
			Name:        "provider-org",
			DisplayName: "Provider Org",
			OrgType:     "ENTERPRISE",
			Roles:       []string{"MEMBER"},
			Teams:       []cdbm.Team{{ID: 3, Name: "team-a", Roles: []string{"MEMBER"}}},
			Updated:     &staleUpdated,
		},
		"unrelated-org": cdbm.Org{
			Name:    "unrelated-org",
			Roles:   []string{"MEMBER"},
			Updated: &staleUpdated,
		},
	}

	// unrelated-org has to come out of every case untouched: one resolution speaks
	// for the one org the route named
	assertUnrelatedOrgPreserved := func(t *testing.T, user *cdbm.User) {
		unrelated, err := user.OrgData.GetOrgByName("unrelated-org")
		require.NoError(t, err)
		assert.Equal(t, []string{"MEMBER"}, unrelated.Roles)
		require.NotNil(t, unrelated.Updated)
		assert.Equal(t, staleUpdated.UnixNano(), unrelated.Updated.UnixNano())
	}

	tests := []struct {
		name    string
		orgData cdbm.OrgData
		check   func(t *testing.T, user *cdbm.User)
	}{
		{
			name: "a stored org takes the reported roles and keeps its richer metadata",
			orgData: cdbm.OrgData{"provider-org": cdbm.Org{
				Name:  "provider-org",
				Roles: []string{roles.ProviderAdminRole},
			}},
			check: func(t *testing.T, user *cdbm.User) {
				require.Len(t, user.OrgData, 2)
				org, err := user.OrgData.GetOrgByName("provider-org")
				require.NoError(t, err)
				assert.Equal(t, []string{roles.ProviderAdminRole}, org.Roles)
				require.NotNil(t, org.Updated)
				assert.True(t, org.Updated.After(staleUpdated))

				assert.Equal(t, 7, org.ID)
				assert.Equal(t, "Provider Org", org.DisplayName)
				assert.Equal(t, "ENTERPRISE", org.OrgType)
				assert.Equal(t, []cdbm.Team{{ID: 3, Name: "team-a", Roles: []string{"MEMBER"}}}, org.Teams)

				assertUnrelatedOrgPreserved(t, user)
			},
		},
		{
			name: "an org the record does not hold is added from what NGC reported",
			orgData: cdbm.OrgData{"second-org": cdbm.Org{
				ID:          9,
				Name:        "second-org",
				DisplayName: "Second Org",
				Roles:       []string{roles.TenantAdminRole},
				Teams:       []cdbm.Team{},
			}},
			check: func(t *testing.T, user *cdbm.User) {
				require.Len(t, user.OrgData, 3)
				org, err := user.OrgData.GetOrgByName("second-org")
				require.NoError(t, err)
				assert.Equal(t, 9, org.ID)
				assert.Equal(t, "Second Org", org.DisplayName)
				assert.Equal(t, []string{roles.TenantAdminRole}, org.Roles)
				require.NotNil(t, org.Updated)
				assert.True(t, org.Updated.After(staleUpdated))

				stale, err := user.OrgData.GetOrgByName("provider-org")
				require.NoError(t, err)
				require.NotNil(t, stale.Updated)
				assert.Equal(t, staleUpdated.UnixNano(), stale.Updated.UnixNano())

				assertUnrelatedOrgPreserved(t, user)
			},
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dbSession := testKasResolverDB(t)
			defer dbSession.Close()

			existingUser, err := cdbm.NewUserDAO(dbSession).Create(context.Background(), nil, cdbm.UserCreateInput{
				StarfleetID: cutil.GetPtr("starfleet-id"),
				OrgData:     seededOrgData,
			})
			require.NoError(t, err)

			updatedUser, err := (&resolver{dbSession: dbSession}).createOrUpdateUser(context.Background(), &identity{
				starfleetID: cutil.GetPtr("starfleet-id"),
				email:       "user@test.com",
				orgData:     tt.orgData,
			})
			require.NoError(t, err)
			assert.Equal(t, existingUser.ID, updatedUser.ID)
			assert.Equal(t, existingUser.Updated.UnixNano(), updatedUser.Updated.UnixNano())

			require.NotNil(t, updatedUser.Email)
			assert.Equal(t, "user@test.com", *updatedUser.Email)

			tt.check(t, updatedUser)

			// The write has to reach the row, not just the returned struct
			persisted, err := cdbm.NewUserDAO(dbSession).Get(context.Background(), nil, existingUser.ID, nil)
			require.NoError(t, err)
			tt.check(t, persisted)
		})
	}
}

func TestResolverResolveUsesVerifiedAt(t *testing.T) {
	dbSession := testKasResolverDB(t)
	defer dbSession.Close()

	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, _ *http.Request) {
		requests++
		_, err := res.Write([]byte(`{"userId":"service-user-id","orgName":"provider-org","type":"SERVICE_KEY","products":["forge-provider"]}`))
		require.NoError(t, err)
	}))
	defer server.Close()

	res := newResolver(dbSession, server.URL, nil)
	res.ngc.http = server.Client()

	resolved, err := res.resolve(context.Background(), testStarfleetKey, "provider-org")
	require.NoError(t, err)
	assert.Equal(t, 1, requests)

	_, err = res.resolve(context.Background(), testStarfleetKey, "provider-org")
	require.NoError(t, err)
	assert.Equal(t, 1, requests)

	entry, found := res.cache.isAllowed(res.getDigest(testStarfleetKey))
	require.True(t, found)
	assert.Equal(t, "provider-org", entry.orgName, "the entry records the route org, not the NGC org data")

	staleAt := time.Now().UTC().Add(-apiKeyStalePeriod - time.Minute)
	res.cache.allowLRU.Add(res.getDigest(testStarfleetKey), cachedUser{
		userID:     resolved.ID,
		orgName:    entry.orgName,
		verifiedAt: staleAt,
	})

	_, err = res.resolve(context.Background(), testStarfleetKey, "provider-org")
	require.NoError(t, err)
	assert.Equal(t, 2, requests)

	entry, found = res.cache.isAllowed(res.getDigest(testStarfleetKey))
	require.True(t, found)
	assert.True(t, entry.verifiedAt.After(staleAt), "refresh must restamp verifiedAt")

	_, err = res.resolve(context.Background(), testStarfleetKey, "provider-org")
	require.NoError(t, err)
	assert.Equal(t, 2, requests)
}

func TestResolverResolveUnsetsAllowedOnFailure(t *testing.T) {
	const notGranted = `{"userId":"service-user-id","orgName":"unrelated-org",` +
		`"type":"SERVICE_KEY","products":["forge-provider"]}`

	tests := []struct {
		name         string
		statusCode   int
		body         string
		cachedOrg    string
		wantErr      error
		wantRetained bool
	}{
		{
			name:       "the route org is no longer granted",
			statusCode: http.StatusOK,
			body:       notGranted,
			cachedOrg:  "provider-org",
			wantErr:    errOrgNotGranted,
		},
		{
			name:         "another org keeps its verification",
			statusCode:   http.StatusOK,
			body:         notGranted,
			cachedOrg:    "other-org",
			wantErr:      errOrgNotGranted,
			wantRetained: true,
		},
		{
			name:       "NGC is unreachable for the route org",
			statusCode: http.StatusBadGateway,
			cachedOrg:  "provider-org",
			wantErr:    errUnresolvable,
		},
		{
			name:         "NGC is unreachable and the entry is for another org",
			statusCode:   http.StatusBadGateway,
			cachedOrg:    "other-org",
			wantErr:      errUnresolvable,
			wantRetained: true,
		},
		{
			name:       "the credential itself is rejected",
			statusCode: http.StatusUnauthorized,
			cachedOrg:  "other-org",
			wantErr:    errKeyRejected,
		},
	}
	dbSession := testKasResolverDB(t)
	defer dbSession.Close()

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, _ *http.Request) {
				res.WriteHeader(tt.statusCode)
				_, _ = res.Write([]byte(tt.body))
			}))
			defer server.Close()

			res := newResolver(dbSession, server.URL, nil)
			res.ngc.http = server.Client()

			// A fresh entry the cache-hit path cannot serve, because the row it
			// names is gone. Resolution falls through to NGC with it still in place
			digest := res.getDigest(testStarfleetKey)
			res.cache.setAllowed(digest, uuid.New(), tt.cachedOrg)

			_, err := res.resolve(context.Background(), testStarfleetKey, "provider-org")
			assert.ErrorIs(t, err, tt.wantErr)

			_, found := res.cache.isAllowed(digest)
			assert.Equal(t, tt.wantRetained, found)
		})
	}
}

func TestResolverResolveBlocksRejectedKeys(t *testing.T) {
	tests := []struct {
		name       string
		statusCode int
		body       string
	}{
		{name: "NGC reports the key unauthorized", statusCode: http.StatusUnauthorized},
		{name: "NGC reports the key forbidden", statusCode: http.StatusForbidden},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			requests := 0
			server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, _ *http.Request) {
				requests++
				res.WriteHeader(tt.statusCode)
				_, _ = res.Write([]byte(tt.body))
			}))
			defer server.Close()

			res := newResolver(nil, server.URL, nil)
			res.ngc.http = server.Client()

			_, err := res.resolve(context.Background(), testStarfleetKey, "provider-org")
			assert.ErrorIs(t, err, errKeyRejected)

			// A blocked key is blocked for every org, not just the one it was
			// rejected for
			_, err = res.resolve(context.Background(), testStarfleetKey, "other-org")
			assert.ErrorIs(t, err, errKeyRejected)
			assert.Equal(t, 1, requests, "a blocked key must not reach NGC again")
		})
	}

	_, err := newResolver(nil, "http://ngc.invalid", nil).resolve(context.Background(), "not-an-ngc-key", "provider-org")
	assert.ErrorIs(t, err, errKeyRejected)
}

func TestAPIKeyCacheSetBlocked(t *testing.T) {
	cache, err := newAPIKeyCache()
	require.NoError(t, err)
	cache.blockPeriod = time.Millisecond

	digest := sha256.Sum256([]byte(testStarfleetKey))
	cache.setAllowed(digest, uuid.New(), "provider-org")
	cache.setBlocked(digest)

	assert.True(t, cache.isBlocked(digest))
	_, allowed := cache.isAllowed(digest)
	assert.False(t, allowed)
	require.Eventually(t, func() bool {
		return !cache.isBlocked(digest)
	}, time.Second, time.Millisecond)
}

func TestKasOriginProcessorProcessToken(t *testing.T) {
	tests := []struct {
		name        string
		statusCode  int
		wantStatus  int
		wantMessage string
		wantLog     string
	}{
		{
			name:        "rate limited",
			statusCode:  http.StatusTooManyRequests,
			wantStatus:  http.StatusServiceUnavailable,
			wantMessage: "NGC rate limited API key verification, try again later",
		},
		{
			name:        "rejected key",
			statusCode:  http.StatusUnauthorized,
			wantStatus:  http.StatusUnauthorized,
			wantMessage: "Invalid authorization token in request",
			wantLog:     errNgcUnauthorized.Error(),
		},
		{
			name:        "upstream failure",
			statusCode:  http.StatusInternalServerError,
			wantStatus:  http.StatusServiceUnavailable,
			wantMessage: "Failed to verify authorization token, try again later",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, _ *http.Request) {
				res.WriteHeader(tt.statusCode)
			}))
			defer server.Close()

			processor := &KasOriginProcessor{resolver: newResolver(nil, server.URL, nil)}
			processor.resolver.ngc.http = server.Client()

			c := echo.New().NewContext(httptest.NewRequest(http.MethodGet, "/", nil), httptest.NewRecorder())
			var logs bytes.Buffer

			dbUser, apiErr := processor.ProcessToken(c, testStarfleetKey, nil, zerolog.New(&logs))

			assert.Nil(t, dbUser)
			require.NotNil(t, apiErr)
			assert.Equal(t, tt.wantStatus, apiErr.Code)
			assert.Equal(t, tt.wantMessage, apiErr.Message)
			if tt.wantLog != "" {
				assert.Contains(t, logs.String(), tt.wantLog)
				assert.NotContains(t, logs.String(), testStarfleetKey)
			}
		})
	}

	t.Run("enforces the route org against what NGC reports for the key", func(t *testing.T) {
		dbSession := testKasResolverDB(t)
		defer dbSession.Close()

		// The record already holds an org from an earlier source that this key is
		// not issued against
		userDAO := cdbm.NewUserDAO(dbSession)
		existingUser, err := userDAO.Create(context.Background(), nil, cdbm.UserCreateInput{
			AuxiliaryID: cutil.GetPtr("service-user-id"),
			OrgData: cdbm.OrgData{"other-org": cdbm.Org{
				Name:  "other-org",
				Roles: []string{roles.ProviderAdminRole},
			}},
		})
		require.NoError(t, err)

		requests := 0
		server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, _ *http.Request) {
			requests++
			_, err := res.Write([]byte(`{"userId":"service-user-id","orgName":"provider-org","type":"SERVICE_KEY","products":["forge-provider"]}`))
			require.NoError(t, err)
		}))
		defer server.Close()

		processor := &KasOriginProcessor{resolver: newResolver(dbSession, server.URL, nil)}
		processor.resolver.ngc.http = server.Client()

		newContext := func(orgName string) echo.Context {
			c := echo.New().NewContext(httptest.NewRequest(http.MethodGet, "/", nil), httptest.NewRecorder())
			c.SetParamNames("orgName")
			c.SetParamValues(orgName)
			return c
		}

		var logs bytes.Buffer
		dbUser, apiErr := processor.ProcessToken(newContext("other-org"), testStarfleetKey, nil, zerolog.New(&logs))
		assert.Nil(t, dbUser)
		require.NotNil(t, apiErr)
		assert.Equal(t, http.StatusForbidden, apiErr.Code)
		assert.Equal(t, "API key is not authorized for requested organization", apiErr.Message)
		assert.NotContains(t, logs.String(), testStarfleetKey)

		// The refusal is not a revoked credential, so nothing was cached either way
		digest := processor.resolver.getDigest(testStarfleetKey)
		_, allowed := processor.resolver.cache.isAllowed(digest)
		assert.False(t, allowed)
		assert.False(t, processor.resolver.cache.isBlocked(digest))

		dbUser, apiErr = processor.ProcessToken(newContext("PROVIDER-ORG"), testStarfleetKey, nil, zerolog.Nop())
		require.Nil(t, apiErr)
		require.NotNil(t, dbUser)
		require.Len(t, dbUser.OrgData, 1)
		_, err = dbUser.OrgData.GetOrgByName("provider-org")
		require.NoError(t, err)
		_, err = dbUser.OrgData.GetOrgByName("other-org")
		assert.Error(t, err, "the request scope carries only the route org")

		// The record keeps "other-org", which was on it before the key resolved. The
		// fetch-time check, not the stored map, is what refuses a request for it
		persistedUser, err := userDAO.Get(context.Background(), nil, existingUser.ID, nil)
		require.NoError(t, err)
		require.Len(t, persistedUser.OrgData, 2)
		_, err = persistedUser.OrgData.GetOrgByName("provider-org")
		require.NoError(t, err)
		_, err = persistedUser.OrgData.GetOrgByName("other-org")
		require.NoError(t, err)
		assert.Equal(t, 2, requests)
	})

	t.Run("a personal key reaches an org other than the one get-caller-info names", func(t *testing.T) {
		dbSession := testKasResolverDB(t)
		defer dbSession.Close()

		server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, _ *http.Request) {
			_, err := res.Write([]byte(`{"userId":"personal-user-id","orgName":"provider-org","type":"PERSONAL_KEY",` +
				`"user":{"starfleetId":"starfleet-id","roles":[` +
				`{"org":{"id":1,"name":"provider-org"},"orgRoles":["` + roles.ProviderAdminRole + `"]},` +
				`{"org":{"id":2,"name":"Second-Org"},"orgRoles":["` + roles.TenantAdminRole + `"]}]}}`))
			require.NoError(t, err)
		}))
		defer server.Close()

		processor := &KasOriginProcessor{resolver: newResolver(dbSession, server.URL, nil)}
		processor.resolver.ngc.http = server.Client()

		c := echo.New().NewContext(httptest.NewRequest(http.MethodGet, "/", nil), httptest.NewRecorder())
		c.SetParamNames("orgName")
		c.SetParamValues("second-org")

		dbUser, apiErr := processor.ProcessToken(c, testStarfleetKey, nil, zerolog.Nop())
		require.Nil(t, apiErr)
		require.NotNil(t, dbUser)
		require.Len(t, dbUser.OrgData, 1)
		org, err := dbUser.OrgData.GetOrgByName("second-org")
		require.NoError(t, err)
		assert.Equal(t, 2, org.ID)
		assert.Equal(t, "second-org", org.Name)
		assert.Equal(t, []string{roles.TenantAdminRole}, org.Roles)
		assert.Equal(t, []cdbm.Team{}, org.Teams)
		assert.NotNil(t, org.Updated)
	})
}

func TestNGCClientGetCallerInfo(t *testing.T) {
	tests := []struct {
		name       string
		statusCode int
		body       string
		cancel     bool
		wantErr    error
		wantCause  error
		wantResult string
	}{
		{
			name:       "success",
			statusCode: http.StatusOK,
			body:       `{"userId":"id","orgName":"provider-org","type":"SERVICE_KEY"}`,
			wantResult: ngcResultSuccess,
		},
		{name: "unauthorized", statusCode: http.StatusUnauthorized, wantErr: errNgcUnauthorized, wantResult: ngcResultRejected},
		{name: "forbidden", statusCode: http.StatusForbidden, wantErr: errNgcUnauthorized, wantResult: ngcResultRejected},
		{name: "rate limited", statusCode: http.StatusTooManyRequests, wantErr: errNgcRateLimited, wantResult: ngcResultRateLimited},
		{
			name:       "oversized",
			statusCode: http.StatusOK,
			body:       strings.Repeat("x", maxNGCResponseSize+1),
			wantErr:    errNgcUpstream,
			wantResult: ngcResultUpstreamError,
		},
		{
			name:       "transport failure",
			cancel:     true,
			wantErr:    errNgcUpstream,
			wantCause:  context.Canceled,
			wantResult: ngcResultUpstreamError,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, _ *http.Request) {
				res.WriteHeader(tt.statusCode)
				_, _ = res.Write([]byte(tt.body))
			}))
			defer server.Close()

			ctx := context.Background()
			if tt.cancel {
				canceled, cancel := context.WithCancel(ctx)
				cancel()
				ctx = canceled
			}

			var operation, result string
			client := &ngcClient{
				http:    server.Client(),
				baseURL: server.URL,
				metrics: kasMetricsRecorder(func(recordedOperation, recordedResult string) {
					operation = recordedOperation
					result = recordedResult
				}),
			}
			_, err := client.getCallerInfo(ctx, testStarfleetKey)

			assert.Equal(t, ngcOperationCallerInfo, operation)
			assert.Equal(t, tt.wantResult, result)
			if tt.wantErr == nil {
				assert.NoError(t, err)
				return
			}
			assert.ErrorIs(t, err, tt.wantErr)
			if tt.wantCause != nil {
				assert.ErrorIs(t, err, tt.wantCause)
			}
		})
	}
}
