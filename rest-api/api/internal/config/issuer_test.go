// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/http/httptest"
	"sync/atomic"
	"testing"
	"time"

	cauth "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	dbutil "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"

	"github.com/golang-jwt/jwt/v5"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

const cachedIssuerKeySet = `{"keys":[{"kty":"RSA","use":"sig","kid":"cached-key","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`

// rotatedIssuerKeySet stands in for the key set an IdP publishes after a rotation.
const rotatedIssuerKeySet = `{"keys":[{"kty":"RSA","use":"sig","kid":"rotated-key","alg":"RS256","n":"test-n-value","e":"AQAB"}]}`

// unreachableJWKSURL points at a closed port so a fetch fails immediately rather
// than making the test wait out a timeout.
const unreachableJWKSURL = "http://127.0.0.1:1/jwks"

func staticIssuerConfig(t *testing.T) *Config {
	t.Helper()
	c, err := NewConfigFromYAML(`
env:
  disconnected: true
issuers:
  - issuer: https://static.example.com
    jwks: https://static.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: static-org
        roles: [TENANT_ADMIN]
`)
	require.NoError(t, err)
	return c
}

func mustConfigFromYAML(t *testing.T, yamlDoc string) *Config {
	t.Helper()
	c, err := NewConfigFromYAML(yamlDoc)
	require.NoError(t, err)
	return c
}

// ~~~~~ pure helpers (no DB) ~~~~~ //

func TestIsStaticIssuer(t *testing.T) {
	c := staticIssuerConfig(t)
	assert.True(t, c.IsStaticIssuer("https://static.example.com", ""), "match by issuer URL")
	assert.True(t, c.IsStaticIssuer("", "https://static.example.com/jwks"), "match by JWKS URL")
	assert.False(t, c.IsStaticIssuer("https://idp.acme.com", "https://idp.acme.com/jwks"), "no match")
	assert.False(t, c.IsStaticIssuer("", ""), "empty arguments match nothing")
}

func TestHasPrivilegedStaticIssuerOrigins(t *testing.T) {
	assert.False(t, staticIssuerConfig(t).HasPrivilegedStaticIssuerOrigins(), "custom-only static set")
	assert.False(t, mustConfigFromYAML(t, `issuers: []`).HasPrivilegedStaticIssuerOrigins(), "empty set")
	assert.True(t, mustConfigFromYAML(t, `
issuers:
  - issuer: https://invalid.example.com
    jwks: https://invalid.example.com/jwks
    origin: typo
`).HasPrivilegedStaticIssuerOrigins(), "an invalid origin must fail closed")

	for _, origin := range []string{"keycloak", "kas-legacy", "kas-ssa"} {
		c := mustConfigFromYAML(t, fmt.Sprintf(`
issuers:
  - issuer: https://privileged.example.com
    jwks: https://privileged.example.com/jwks
    origin: %s
`, origin))
		assert.True(t, c.HasPrivilegedStaticIssuerOrigins(), origin)
	}
}

// TestDynamicIssuersEnabled is the policy matrix for the whole feature. This one
// condition decides both whether the issuer routes exist and whether the
// resolver, the startup reload, and the background loops run, so every mode the
// ConfigMap can express is enumerated here rather than rediscovered per call site.
func TestDynamicIssuersEnabled(t *testing.T) {
	tests := []struct {
		name string
		yaml string
		want bool
	}{
		{
			name: "custom_only_issuers",
			yaml: `
env:
  disconnected: true
keycloak:
  enabled: false
issuers:
  - issuer: https://custom.example.com
    jwks: https://custom.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: acme
        roles: [TENANT_ADMIN]
`,
			want: true,
		},
		{
			name: "empty_issuer_list",
			yaml: `
env:
  disconnected: true
keycloak:
  enabled: false
issuers: []
`,
			want: true,
		},
		{
			name: "connected_mode_is_configmap_only",
			yaml: `
env:
  disconnected: false
keycloak:
  enabled: false
issuers:
  - issuer: https://custom.example.com
    jwks: https://custom.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: acme
        roles: [TENANT_ADMIN]
`,
			want: false,
		},
		{
			name: "dynamic_configmap_mapping_is_not_a_bar",
			yaml: `
env:
  disconnected: true
keycloak:
  enabled: false
issuers:
  - issuer: https://dyn.example.com
    jwks: https://dyn.example.com/jwks
    origin: custom
    claimMappings:
      - orgAttribute: org
        orgDisplayAttribute: org_display
        rolesAttribute: roles
`,
			want: true,
		},
		{
			name: "keycloak_enabled",
			yaml: `
keycloak:
  enabled: true
issuers: []
`,
			want: false,
		},
		{
			name: "kas_legacy_static_issuer",
			yaml: `
keycloak:
  enabled: false
issuers:
  - issuer: https://kas.example.com
    jwks: https://kas.example.com/jwks
    origin: kas-legacy
`,
			want: false,
		},
		{
			name: "kas_ssa_static_issuer",
			yaml: `
keycloak:
  enabled: false
issuers:
  - issuer: https://ssa.example.com
    jwks: https://ssa.example.com/jwks
    origin: kas-ssa
`,
			want: false,
		},
		{
			name: "keycloak_enabled_alongside_custom_issuers",
			yaml: `
keycloak:
  enabled: true
issuers:
  - issuer: https://custom.example.com
    jwks: https://custom.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: acme
        roles: [TENANT_ADMIN]
`,
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			assert.Equal(t, tt.want, mustConfigFromYAML(t, tt.yaml).DynamicIssuersEnabled())
		})
	}
}

func TestDynamicIssuerFuncsNoopWhenDisabled(t *testing.T) {
	ctx := context.Background()
	c := mustConfigFromYAML(t, `
env:
  disconnected: false
keycloak:
  enabled: false
issuers: []
`)
	reg := cauth.NewTokenOriginConfig()
	c.TokenOriginConfig = reg

	dbSession := dbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	require.NoError(t, dbSession.DB.ResetModel(ctx, (*cdbm.Issuer)(nil)))
	seedIssuer(t, ctx, dbSession, cdbm.NewIssuerDAO(dbSession), "https://idp.acme.com", "https://idp.acme.com/jwks")

	require.False(t, c.DynamicIssuersEnabled())
	c.InstallIssuerResolver(dbSession)
	assert.Nil(t, reg.ResolveConfig(ctx, "https://idp.acme.com"), "resolver must not be installed")
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	assert.Nil(t, reg.GetConfig("https://idp.acme.com"), "reload must not pull DB issuers")
	c.StartIssuerReloadLoop(ctx, dbSession, time.Millisecond)
	c.StartJWKSRefreshLoop(ctx, dbSession, time.Millisecond)
	c.StartJWKSPendingRetryLoop(ctx, dbSession, time.Millisecond)
	assert.Nil(t, reg.GetConfig("https://idp.acme.com"), "loops must not start")
}

func TestConvertClaimMappings_LowercasesOrgName(t *testing.T) {
	out := convertClaimMappings([]cdbm.ClaimMapping{
		{OrgName: "ACME-Corp", Roles: []string{"TENANT_ADMIN"}, Audiences: []string{"org-acme"}, Scopes: []string{"nico:read"}},
		{OrgAttribute: "org", OrgDisplayAttribute: "org_display", RolesAttribute: "roles"},
	})
	require.Len(t, out, 2)
	assert.Equal(t, "acme-corp", out[0].OrgName)
	// The per-mapping gates decide authorization, so dropping one here would widen
	// access silently rather than fail.
	assert.Equal(t, []string{"org-acme"}, out[0].Audiences)
	assert.Equal(t, []string{"nico:read"}, out[0].Scopes)
	assert.Equal(t, "org", out[1].OrgAttribute)
	assert.Equal(t, "", out[1].OrgName)
}

func TestComputeReservedOrgNames(t *testing.T) {
	reserved := computeReservedOrgNames([]IssuerConfig{
		{ClaimMappings: []cauth.ClaimMapping{{OrgName: "Acme"}, {OrgAttribute: "org"}}},
		{ClaimMappings: []cauth.ClaimMapping{{OrgName: "beta"}}},
	})
	assert.True(t, reserved["acme"])
	assert.True(t, reserved["beta"])
	assert.False(t, reserved["org"]) // dynamic attribute is not a reserved static name
}

func TestIssuerHasDynamicMapping(t *testing.T) {
	assert.True(t, (cdbm.Issuer{ClaimMappings: []cdbm.ClaimMapping{{OrgAttribute: "org"}}}).HasDynamicMapping())
	assert.False(t, (cdbm.Issuer{ClaimMappings: []cdbm.ClaimMapping{{OrgName: "acme"}}}).HasDynamicMapping())
}

func TestDBIssuerSignature_ChangeDetection(t *testing.T) {
	base := cdbm.Issuer{
		IssuerURL:     "https://idp.acme.com",
		JWKSUrl:       "https://idp.acme.com/jwks",
		Origin:        "custom",
		JWKSTimeout:   "5s",
		Audiences:     []string{"api"},
		ClaimMappings: []cdbm.ClaimMapping{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}}},
	}
	sig1 := base.Signature()
	assert.Equal(t, sig1, base.Signature(), "same input → same signature")

	changed := base
	changed.ClaimMappings = []cdbm.ClaimMapping{{OrgName: "acme", Roles: []string{"PROVIDER_ADMIN"}}}
	assert.NotEqual(t, sig1, changed.Signature(), "claim mapping change → new signature")

	changed2 := base
	changed2.JWKSUrl = "https://idp.acme.com/jwks/v2"
	assert.NotEqual(t, sig1, changed2.Signature(), "JWKS URL change → new signature")
}

func TestIssuerConfigFromDB(t *testing.T) {
	di := cdbm.Issuer{
		Origin:        "custom",
		IssuerURL:     "https://idp.acme.com",
		JWKSUrl:       "https://idp.acme.com/jwks",
		JWKSTimeout:   "10s",
		Audiences:     []string{"api"},
		Scopes:        []string{"carbide"},
		ClaimMappings: []cdbm.ClaimMapping{{OrgName: "Acme", Roles: []string{"TENANT_ADMIN"}}},
	}
	ic := issuerConfigFromDB(di)
	assert.Equal(t, "https://idp.acme.com", ic.Issuer)
	assert.Equal(t, "https://idp.acme.com/jwks", ic.JWKS)
	assert.Equal(t, "10s", ic.JWKSTimeout)
	require.Len(t, ic.ClaimMappings, 1)
	assert.Equal(t, "acme", ic.ClaimMappings[0].OrgName) // normalized
}

// ~~~~~ DB-backed: seed, static-wins, convergence ~~~~~ //

func TestSeedAndReloadDBIssuers(t *testing.T) {
	ctx := context.Background()
	c := staticIssuerConfig(t)

	// Registry pre-populated with the static issuer, as GetOrInit would have done.
	reg := cauth.NewTokenOriginConfig()
	reg.AddJwksConfig(cauth.NewJwksConfig("https://static.example.com/jwks", "https://static.example.com", "custom", false, nil, nil))
	c.TokenOriginConfig = reg

	dbSession := dbutil.GetTestDBSession(t, false)
	defer dbSession.Close()
	require.NoError(t, dbSession.DB.ResetModel(ctx, (*cdbm.Issuer)(nil)))
	dao := cdbm.NewIssuerDAO(dbSession)

	// A valid DB issuer (unreachable JWKS URL so the fetch fails fast & non-fatally).
	err := cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		_, derr := dao.Create(ctx, tx, cdbm.IssuerCreateOrUpdateInput{
			Origin:        "custom",
			IssuerURL:     "https://idp.acme.com",
			JWKSUrl:       "http://127.0.0.1:1/jwks",
			ClaimMappings: []cdbm.ClaimMapping{{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}}},
		})
		return derr
	})
	require.NoError(t, err)

	// Seed → DB issuer present, static preserved.
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	assert.NotNil(t, reg.GetConfig("https://idp.acme.com"), "DB issuer loaded into registry")
	assert.NotNil(t, reg.GetConfig("https://static.example.com"), "static issuer preserved")

	// Insert a DB row that conflicts with the static issuer URL → must be skipped
	// (static wins) and must NOT overwrite the static config.
	err = cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		_, derr := dao.Create(ctx, tx, cdbm.IssuerCreateOrUpdateInput{
			Origin:        "custom",
			IssuerURL:     "https://static.example.com",
			JWKSUrl:       "http://127.0.0.1:1/jwks",
			ClaimMappings: []cdbm.ClaimMapping{{OrgName: "shadow", Roles: []string{"TENANT_ADMIN"}}},
		})
		return derr
	})
	require.NoError(t, err)

	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	staticCfg := reg.GetConfig("https://static.example.com")
	require.NotNil(t, staticCfg, "static issuer still present after conflicting DB row")
	assert.Equal(t, "https://static.example.com/jwks", staticCfg.URL, "static config was NOT overwritten by the DB row")

	// Delete the DB issuer → convergence removes it; static remains.
	var acme *cdbm.Issuer
	acme, err = firstByURL(ctx, dao, "https://idp.acme.com")
	require.NoError(t, err)
	require.NotNil(t, acme)
	err = cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		return dao.Delete(ctx, tx, acme.ID)
	})
	require.NoError(t, err)

	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	assert.Nil(t, reg.GetConfig("https://idp.acme.com"), "deleted DB issuer removed from registry")
	assert.NotNil(t, reg.GetConfig("https://static.example.com"), "static issuer still present")
}

// TestReloadSkipsDynamicDBIssuer verifies the hard security boundary: a DB issuer
// carrying a dynamic mapping — e.g. inserted out-of-band — is never applied to the
// live registry. Each attribute is exercised on its own, because any one of them
// lets token claims drive org or role assignment; combined in a single mapping,
// orgAttribute alone would keep this green while the other two regressed. The
// static row is the control that proves a skip is caused by the mapping rather
// than by something else in the reload path.
func TestReloadSkipsDynamicDBIssuer(t *testing.T) {
	const issuerURL = "http://localhost:8082/realms/dyn"

	tests := []struct {
		name           string
		mapping        cdbm.ClaimMapping
		wantRegistered bool
	}{
		{
			name:    "org attribute",
			mapping: cdbm.ClaimMapping{OrgAttribute: "org"},
		},
		{
			name:    "org display attribute",
			mapping: cdbm.ClaimMapping{OrgName: "acme", OrgDisplayAttribute: "org_display", Roles: []string{"TENANT_ADMIN"}},
		},
		{
			name:    "roles attribute",
			mapping: cdbm.ClaimMapping{OrgName: "acme", RolesAttribute: "roles"},
		},
		{
			name:           "fully static mapping",
			mapping:        cdbm.ClaimMapping{OrgName: "acme", Roles: []string{"TENANT_ADMIN"}},
			wantRegistered: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := context.Background()
			idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
				_, _ = w.Write([]byte(cachedIssuerKeySet))
			}))
			defer idp.Close()

			c, err := NewConfigFromYAML(`
env:
  disconnected: true
issuers: []
`)
			require.NoError(t, err)
			reg := cauth.NewTokenOriginConfig()
			c.TokenOriginConfig = reg

			dbSession := dbutil.GetTestDBSession(t, false)
			defer dbSession.Close()
			require.NoError(t, dbSession.DB.ResetModel(ctx, (*cdbm.Issuer)(nil)))
			dao := cdbm.NewIssuerDAO(dbSession)

			// DAO.Create does not validate, so this is a row inserted out-of-band,
			// bypassing the API guard.
			require.NoError(t, cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
				_, e := dao.Create(ctx, tx, cdbm.IssuerCreateOrUpdateInput{
					Origin:        "custom",
					IssuerURL:     issuerURL,
					JWKSUrl:       idp.URL + "/jwks",
					ClaimMappings: []cdbm.ClaimMapping{tt.mapping},
				})
				return e
			}))

			require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
			if tt.wantRegistered {
				assert.NotNil(t, reg.GetConfig(issuerURL), "a static DB issuer must reach the registry")
				return
			}
			assert.Nil(t, reg.GetConfig(issuerURL),
				"a dynamic DB issuer must be skipped, never applied to the registry")
		})
	}
}

// ~~~~~ DB-backed: JWKS cache, resolution, shutdown ~~~~~ //

func TestBuildJwksConfigHydratesFromCache(t *testing.T) {
	fetchedAt := time.Now().UTC().Add(-time.Minute)

	tests := []struct {
		name          string
		keys          json.RawMessage
		fetchedAt     *time.Time
		wantKeyCount  int
		wantLastFetch time.Time
	}{
		{
			name:          "cached_key_set_is_installed",
			keys:          json.RawMessage(cachedIssuerKeySet),
			fetchedAt:     &fetchedAt,
			wantKeyCount:  1,
			wantLastFetch: fetchedAt,
		},
		{
			name:         "never_fetched_row_stays_keyless",
			wantKeyCount: 0,
		},
		{
			// A blob written by an older or buggy version must not install a key set
			// the fetch path would have rejected.
			name:         "corrupt_blob_is_ignored",
			keys:         json.RawMessage(`{"keys": [not json`),
			fetchedAt:    &fetchedAt,
			wantKeyCount: 0,
		},
		{
			// jwks_fetched_at is what seeds the throttle. Without it there is no
			// defensible LastUpdated to install, so the keys are not trusted either.
			name:         "keys_without_fetch_time_are_ignored",
			keys:         json.RawMessage(cachedIssuerKeySet),
			wantKeyCount: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			cfg := buildJwksConfig(cdbm.Issuer{
				Origin:        "custom",
				IssuerURL:     "https://idp.acme.com",
				JWKSUrl:       unreachableJWKSURL,
				JWKSTimeout:   "5s",
				JWKSKeys:      tt.keys,
				JWKSFetchedAt: tt.fetchedAt,
			})

			assert.Equal(t, tt.wantKeyCount, cfg.KeyCount())
			if tt.wantKeyCount > 0 {
				assert.Equal(t, tt.wantLastFetch, cfg.LastFetchedAt(),
					"the cached fetch time seeds the refresh throttle")
			}
		})
	}
}

// TestRefreshJWKSPersistsFetchedKeys covers the write path: a refresh pass stores
// the key set it fetched, so every other replica starts warm from the row.
func TestRefreshJWKSPersistsFetchedKeys(t *testing.T) {
	ctx := context.Background()
	var hits atomic.Int32
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()

	created := seedIssuer(t, ctx, dbSession, dao, "https://idp.acme.com", idp.URL)
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	assert.Equal(t, int32(0), hits.Load(), "reload hydrates from the DB and never reaches the IdP")

	beforeRefresh := time.Now().UTC()
	c.refreshJWKS(ctx, dbSession, false)

	stored, gerr := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, gerr)
	assert.JSONEq(t, cachedIssuerKeySet, string(stored.JWKSKeys), "the fetched key set is persisted")
	require.NotNil(t, stored.JWKSFetchedAt)
	assert.False(t, stored.JWKSFetchedAt.Before(beforeRefresh),
		"the persisted stamp is the fetch this pass issued, not an inherited one")
	assert.Equal(t, int32(1), hits.Load())

	// A second replica hydrating from that row must not need the IdP at all.
	other, err := NewConfigFromYAML(`
env:
  disconnected: true
issuers: []
`)
	require.NoError(t, err)
	other.TokenOriginConfig = cauth.NewTokenOriginConfig()
	require.NoError(t, other.ReloadDBIssuers(ctx, dbSession))
	assert.Equal(t, int32(1), hits.Load(), "hydration from the DB must not re-fetch")

	cfg := other.TokenOriginConfig.GetConfig("https://idp.acme.com")
	require.NotNil(t, cfg)
	assert.Equal(t, 1, cfg.KeyCount())
}

func TestRefreshJWKSBoundsConcurrentFetches(t *testing.T) {
	ctx := context.Background()
	var active atomic.Int32
	var peak atomic.Int32
	release := make(chan struct{})
	released := false
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		current := active.Add(1)
		defer active.Add(-1)
		for {
			previous := peak.Load()
			if current <= previous || peak.CompareAndSwap(previous, current) {
				break
			}
		}
		<-release
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()
	defer func() {
		if !released {
			close(release)
		}
	}()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()
	for i := range maxConcurrentJWKSRefreshes + 4 {
		seedIssuerWithOrg(t, ctx, dbSession, dao,
			fmt.Sprintf("https://bounded-%d.example.com", i),
			fmt.Sprintf("%s/jwks-%d", idp.URL, i),
			fmt.Sprintf("bounded-org-%d", i))
	}
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))

	done := make(chan struct{})
	go func() {
		c.refreshJWKS(ctx, dbSession, false)
		close(done)
	}()

	require.Eventually(t, func() bool {
		return active.Load() == int32(maxConcurrentJWKSRefreshes)
	}, 5*time.Second, 10*time.Millisecond)
	assert.Equal(t, int32(maxConcurrentJWKSRefreshes), peak.Load())
	close(release)
	released = true
	require.Eventually(t, func() bool {
		select {
		case <-done:
			return true
		default:
			return false
		}
	}, 5*time.Second, 10*time.Millisecond)
}

// TestReloadRegistersWithoutContactingIdP pins reload's architectural role: it
// hydrates and registers from the row alone. An issuer whose IdP has never been
// reached is still registered, keyless, for the refresh and pending loops to
// fill in later.
func TestReloadRegistersWithoutContactingIdP(t *testing.T) {
	ctx := context.Background()
	var hits atomic.Int32
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()

	created := seedIssuer(t, ctx, dbSession, dao, "https://idp.acme.com", idp.URL)
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))

	live := c.TokenOriginConfig.GetConfig("https://idp.acme.com")
	require.NotNil(t, live, "a row with no cached keys is still registered")
	assert.Equal(t, 0, live.KeyCount())
	assert.Equal(t, int32(0), hits.Load(), "reload performs no network I/O")

	stored, err := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.Empty(t, stored.JWKSKeys, "nothing was fetched, so nothing is cached")
	assert.Nil(t, stored.JWKSFetchedAt)
}

// TestReloadDoesNotBlockOnHungIdP is the regression for reload accidentally
// fetching: several hung IdPs used to be paid sequentially at startup.
func TestReloadDoesNotBlockOnHungIdP(t *testing.T) {
	hang := make(chan struct{})
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		<-hang
	}))
	// Release the handlers before Close, which waits for outstanding requests.
	defer idp.Close()
	defer close(hang)

	ctx := context.Background()
	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()

	for i := range 3 {
		seedIssuerWithOrg(t, ctx, dbSession, dao,
			fmt.Sprintf("https://idp%d.acme.com", i),
			fmt.Sprintf("%s/jwks-%d", idp.URL, i),
			fmt.Sprintf("acme-org-%d", i))
	}

	start := time.Now()
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	elapsed := time.Since(start)

	assert.Less(t, elapsed, 2*time.Second, "reload must not contact identity providers")
	for i := range 3 {
		live := c.TokenOriginConfig.GetConfig(fmt.Sprintf("https://idp%d.acme.com", i))
		require.NotNil(t, live)
		assert.Equal(t, 0, live.KeyCount())
	}
}

// TestReloadAdoptsNewerPersistedJWKS is the cross-replica convergence guarantee:
// replica A refreshes and persists K2, and replica B must pick K2 up on its next
// reload even though the issuer's configuration signature never changed and the
// IdP is unreachable from B.
func TestReloadAdoptsNewerPersistedJWKS(t *testing.T) {
	ctx := context.Background()
	a, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()

	const issuerURL = "https://idp.acme.com"
	created := seedIssuer(t, ctx, dbSession, dao, issuerURL, unreachableJWKSURL)

	// Both replicas start from the same persisted K1.
	k1At := time.Now().UTC().Add(-time.Hour)
	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, json.RawMessage(cachedIssuerKeySet), k1At))
	require.NoError(t, a.ReloadDBIssuers(ctx, dbSession))

	b, err := NewConfigFromYAML(`
env:
  disconnected: true
issuers: []
`)
	require.NoError(t, err)
	b.TokenOriginConfig = cauth.NewTokenOriginConfig()
	require.NoError(t, b.ReloadDBIssuers(ctx, dbSession))

	liveB := b.TokenOriginConfig.GetConfig(issuerURL)
	require.NotNil(t, liveB)
	require.WithinDuration(t, k1At, liveB.LastFetchedAt(), time.Millisecond)
	_, kerr := liveB.GetKeyByID("rotated-key")
	require.Error(t, kerr, "K2 is not reachable before the reload that adopts it")

	// Replica A refreshes and persists K2.
	k2At := time.Now().UTC()
	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, created.ID, json.RawMessage(rotatedIssuerKeySet), k2At))

	require.NoError(t, b.ReloadDBIssuers(ctx, dbSession))

	assert.Same(t, liveB, b.TokenOriginConfig.GetConfig(issuerURL),
		"an unchanged signature must not rebuild the registry entry")
	assert.WithinDuration(t, k2At, liveB.LastFetchedAt(), time.Millisecond)
	_, kerr = liveB.GetKeyByID("rotated-key")
	assert.NoError(t, kerr, "the newer persisted key set is usable without contacting the IdP")

	// A row that is no longer newer than what is installed changes nothing.
	adopted := liveB.LastFetchedAt()
	require.NoError(t, b.ReloadDBIssuers(ctx, dbSession))
	assert.Equal(t, adopted, liveB.LastFetchedAt(), "a re-read of the same row is not a newer key set")
}

// TestReloadIgnoresRowsTheConfigMapNowClaims covers the restart after a ConfigMap
// edit that lands on issuers already in the table. The ConfigMap is the source of
// truth, so such a row must be ignored — kept in the table, kept out of the
// registry, and kept out of the background refresh — whether the collision is on
// the issuer URL or on the JWKS URL.
func TestReloadIgnoresRowsTheConfigMapNowClaims(t *testing.T) {
	ctx := context.Background()
	var hits atomic.Int32
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()

	// One static issuer claims an existing row's issuer URL, the other claims a
	// different row's JWKS URL.
	c, dbSession, dao := newIssuerTestEnvWithYAML(t, fmt.Sprintf(`
issuers:
  - issuer: https://iss-clash.example.com
    jwks: %[1]s/static-iss-owner-jwks
    origin: custom
    claimMappings:
      - orgName: static-iss-org
        roles: [TENANT_ADMIN]
  - issuer: https://static-jwks-owner.example.com
    jwks: %[1]s/contested-jwks
    origin: custom
    claimMappings:
      - orgName: static-jwks-org
        roles: [TENANT_ADMIN]
`, idp.URL))

	issClash := seedIssuerWithOrg(t, ctx, dbSession, dao, "https://iss-clash.example.com", idp.URL+"/iss-clash-jwks", "iss-clash")
	jwksClash := seedIssuerWithOrg(t, ctx, dbSession, dao, "https://jwks-clash.example.com", idp.URL+"/contested-jwks", "jwks-clash")
	accepted := seedIssuerWithOrg(t, ctx, dbSession, dao, "https://idp.acme.com", idp.URL+"/acme-jwks", "acme-org")

	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))

	// The registry holds only issuers reload accepted; the static entries are
	// registered elsewhere, so nil here means the row itself was ignored.
	assert.Nil(t, c.TokenOriginConfig.GetConfig(issClash.IssuerURL), "the ConfigMap owns this issuer URL")
	assert.Nil(t, c.TokenOriginConfig.GetConfig(jwksClash.IssuerURL), "the ConfigMap owns this JWKS URL")
	require.NotNil(t, c.TokenOriginConfig.GetConfig(accepted.IssuerURL), "an uncontested row still registers")

	// Ignored is not deleted: GetByID excludes soft-deleted rows, so both still
	// resolving proves reload left the operator's data alone.
	for _, ignored := range []*cdbm.Issuer{issClash, jwksClash} {
		stored, gerr := dao.GetByID(ctx, nil, ignored.ID)
		require.NoError(t, gerr, "an ignored row must stay in the table")
		assert.Nil(t, stored.Deleted)
	}

	// Neither the refresh loop nor a one-shot refresh may fetch for an ignored row.
	c.refreshJWKS(ctx, dbSession, false)
	assert.Equal(t, int32(1), hits.Load(), "only the accepted row's IdP is contacted")
}

// TestResolveIssuerOnDemand covers the cross-replica gap the removal of
// LISTEN/NOTIFY opens: a token naming an issuer this replica has not reloaded yet
// must still be servable. Resolution is a database read and nothing more — the row
// is published with the key set it already carries, so the token path pays no
// identity-provider round-trip for an issuer some replica has already refreshed.
func TestResolveIssuerOnDemand(t *testing.T) {
	ctx := context.Background()
	var hits atomic.Int32
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()
	c.InstallIssuerResolver(dbSession)

	// Both rows were created after this replica's last reload, so the registry has
	// never seen either. Only the first carries a persisted key set.
	const cachedURL = "https://cached.acme.com"
	const keylessURL = "https://keyless.acme.com"
	cached := seedIssuerWithOrg(t, ctx, dbSession, dao, cachedURL, idp.URL+"/cached", "cached-org")
	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, cached.ID, json.RawMessage(cachedIssuerKeySet), time.Now().UTC()))
	seedIssuerWithOrg(t, ctx, dbSession, dao, keylessURL, idp.URL+"/keyless", "keyless-org")
	require.Nil(t, c.TokenOriginConfig.GetConfig(cachedURL))

	resolved := c.TokenOriginConfig.ResolveConfig(ctx, cachedURL)
	require.NotNil(t, resolved, "an issuer present in the DB must resolve on first use")
	assert.Equal(t, 1, resolved.KeyCount(), "the persisted key set is installed as-is")
	assert.NotNil(t, c.TokenOriginConfig.GetConfig(cachedURL), "the resolved issuer joins the registry")

	// A row the background paths have not reached yet still joins the registry; its
	// keys arrive with the token path's own refresh on an unknown kid.
	keyless := c.TokenOriginConfig.ResolveConfig(ctx, keylessURL)
	require.NotNil(t, keyless, "a row with no cached keys still resolves")
	assert.Equal(t, 0, keyless.KeyCount())
	assert.NotNil(t, c.TokenOriginConfig.GetConfig(keylessURL))

	assert.Equal(t, int32(0), hits.Load(), "resolving an issuer must not reach its IdP")

	assert.Nil(t, c.TokenOriginConfig.ResolveConfig(ctx, "https://nonexistent.example.com"),
		"an issuer that does not exist must not resolve")
}

// TestIssuerLoopsStopOnClose covers shutdown: the loops outlive every request, so
// they used to run on context.Background() and keep polling the DB and the IdPs
// after the server had stopped serving.
func TestIssuerLoopsStopOnClose(t *testing.T) {
	ctx := context.Background()
	var hits atomic.Int32
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		hits.Add(1)
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()

	seedIssuer(t, ctx, dbSession, dao, "https://idp.acme.com", idp.URL)
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))

	loopCtx := c.NewIssuerLoopContext(ctx)
	c.StartIssuerReloadLoop(loopCtx, dbSession, 10*time.Millisecond)
	c.StartJWKSRefreshLoop(loopCtx, dbSession, 10*time.Millisecond)

	require.Eventually(t, func() bool { return hits.Load() > 0 }, 5*time.Second, 10*time.Millisecond,
		"the loops must actually be running before their shutdown means anything")

	c.Close()
	require.Error(t, loopCtx.Err(), "Close cancels the context the loops run on")

	settled := hits.Load()
	time.Sleep(200 * time.Millisecond)
	assert.Equal(t, settled, hits.Load(), "no further work is issued after shutdown")
}

// TestPendingRetryLoopFillsAKeylessIssuer covers what makes accepting a create
// against an unreachable identity provider defensible: the issuer is registered
// keyless, and the retry loop keeps fetching until it has keys, without waiting out
// the ordinary refresh interval. An issuer that already has keys is not its job.
func TestPendingRetryLoopFillsAKeylessIssuer(t *testing.T) {
	ctx := context.Background()
	var hits atomic.Int32
	var reachable atomic.Bool
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !reachable.Load() {
			w.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		hits.Add(1)
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()

	const keylessURL = "https://keyless.acme.com"
	const readyURL = "https://ready.acme.com"
	keyless := seedIssuerWithOrg(t, ctx, dbSession, dao, keylessURL, idp.URL+"/keyless", "keyless-org")
	ready := seedIssuerWithOrg(t, ctx, dbSession, dao, readyURL, idp.URL+"/ready", "ready-org")
	require.NoError(t, dao.UpdateJWKSCache(ctx, nil, ready.ID, json.RawMessage(cachedIssuerKeySet), time.Now().UTC()))

	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	require.Equal(t, 0, c.TokenOriginConfig.GetConfig(keylessURL).KeyCount(), "the create left it keyless")

	loopCtx := c.NewIssuerLoopContext(ctx)
	defer c.Close()
	c.StartJWKSPendingRetryLoop(loopCtx, dbSession, 10*time.Millisecond)

	// While the IdP refuses the fetch the issuer stays keyless, and the loop keeps
	// coming back rather than giving up after the first failure.
	time.Sleep(100 * time.Millisecond)
	require.Equal(t, 0, c.TokenOriginConfig.GetConfig(keylessURL).KeyCount())

	// The failed attempts armed the per-issuer fetch throttle, so recovery is
	// observed within one minUpdateInterval of the IdP answering, not one tick.
	reachable.Store(true)
	require.Eventually(t, func() bool { return c.TokenOriginConfig.GetConfig(keylessURL).KeyCount() > 0 },
		15*time.Second, 50*time.Millisecond, "the retry must install the keys once the IdP answers and the throttle window passes")

	stored, err := dao.GetByID(ctx, nil, keyless.ID)
	require.NoError(t, err)
	assert.True(t, stored.HasCachedKeys(), "the fetched key set is persisted, so the issuer reads back Ready")

	// Only the keyless issuer was fetched: the one that already had keys belongs to
	// the ordinary refresh interval, not to this loop.
	settled := hits.Load()
	time.Sleep(100 * time.Millisecond)
	assert.Equal(t, settled, hits.Load(), "an issuer with keys is not retried by the pending loop")
}

// TestOversizedJWKSIsNeitherInstalledNorStored covers the endpoint-size bound
// from the persistence side: an issuer's JWKS URL is operator-supplied, and a
// response past the cap must not reach the registry or the row.
func TestOversizedJWKSIsNeitherInstalledNorStored(t *testing.T) {
	filler := bytes.Repeat([]byte("a"), 64*1024)
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		for written := 0; written <= core.MaxJWKSResponseBytes; written += len(filler) {
			if _, werr := w.Write(filler); werr != nil {
				return
			}
		}
	}))
	defer idp.Close()

	ctx := context.Background()
	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()

	const issuerURL = "https://idp.acme.com"
	created := seedIssuer(t, ctx, dbSession, dao, issuerURL, idp.URL)

	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	c.refreshJWKS(ctx, dbSession, false)

	live := c.TokenOriginConfig.GetConfig(issuerURL)
	require.NotNil(t, live)
	assert.Equal(t, 0, live.KeyCount(), "an oversized key set must not be installed")

	stored, err := dao.GetByID(ctx, nil, created.ID)
	require.NoError(t, err)
	assert.Empty(t, stored.JWKSKeys, "and must not be written to the issuer row")
	assert.Nil(t, stored.JWKSFetchedAt)
}

// TestResolverDoesNotPublishADeletedIssuer covers the race the on-demand path
// opens: the candidate row is read while holding no lock, so the issuer can be
// deleted before the resolver takes the lock to publish it. Publishing it then
// would silently restore trust an operator just withdrew. The two steps are driven
// separately so the interleaving is the test rather than a timing accident.
func TestResolverDoesNotPublishADeletedIssuer(t *testing.T) {
	ctx := context.Background()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()
	c.InstallIssuerResolver(dbSession)

	const issuerURL = "https://idp.acme.com"
	created := seedIssuer(t, ctx, dbSession, dao, issuerURL, unreachableJWKSURL)

	// The resolver's first step: the lock-free candidate read.
	candidate, err := c.findAcceptableDBIssuer(ctx, dbSession, issuerURL)
	require.NoError(t, err)
	require.NotNil(t, candidate)

	// The delete handler's shape — lock, delete, commit — while the resolver holds
	// a candidate it read and no lock.
	require.NoError(t, cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		derr := tx.AcquireAdvisoryLock(ctx, cdb.GetAdvisoryLockIDFromString(IssuerOrgMappingLockKey), true)
		if derr != nil {
			return derr
		}
		return dao.Delete(ctx, tx, created.ID)
	}))

	// The resolver's second step must notice the deletion.
	published, perr := c.publishResolvedIssuer(ctx, dbSession, c.TokenOriginConfig, candidate.ID, buildJwksConfig(*candidate))
	require.NoError(t, perr)
	assert.False(t, published, "an issuer deleted mid-resolution must not be published")
	assert.Nil(t, c.TokenOriginConfig.GetConfig(issuerURL), "and must not be re-added to the registry")

	assert.Nil(t, c.TokenOriginConfig.ResolveConfig(ctx, issuerURL), "nor resolve on a later token")
}

// TestResolverAppliesTheSameValidationAsReload pins the acceptance rules to one
// definition. A conflicting row that reload skips must not be servable just
// because it arrived on the token path instead.
func TestResolverAppliesTheSameValidationAsReload(t *testing.T) {
	ctx := context.Background()
	idp := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		_, _ = w.Write([]byte(cachedIssuerKeySet))
	}))
	defer idp.Close()

	c, dbSession, dao := newIssuerTestEnv(t)
	defer dbSession.Close()
	c.InstallIssuerResolver(dbSession)

	const conflictingURL = "https://idp-b.acme.com"
	seedIssuerWithOrg(t, ctx, dbSession, dao, "https://idp-a.acme.com", idp.URL+"/a", "acme")
	seedIssuerWithOrg(t, ctx, dbSession, dao, conflictingURL, idp.URL+"/b", "acme")

	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	require.NotNil(t, c.TokenOriginConfig.GetConfig("https://idp-a.acme.com"), "the first row is acceptable")
	require.Nil(t, c.TokenOriginConfig.GetConfig(conflictingURL), "reload skips the duplicate org mapping")

	assert.Nil(t, c.TokenOriginConfig.ResolveConfig(ctx, conflictingURL),
		"the resolver must refuse what reload refused")
	assert.Nil(t, c.TokenOriginConfig.GetConfig(conflictingURL))
}

// TestReservedOrgNamesFollowDBIssuers covers what lets a ConfigMap dynamic issuer
// coexist with runtime ones: every org name a static mapping owns is reserved
// against the dynamic mapping, and the reservation has to follow the issuer table,
// not just the ConfigMap. Without this a row created through the API would serve an
// org the dynamic issuer also mints from a claim, and two issuers would authorize
// the same org.
func TestReservedOrgNamesFollowDBIssuers(t *testing.T) {
	ctx := context.Background()
	c, dbSession, dao, dynCfg := newDynamicConfigMapIssuerEnv(t)
	defer dbSession.Close()

	row := seedIssuerWithOrg(t, ctx, dbSession, dao, "https://idp.acme.com", unreachableJWKSURL, "acme")

	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	reserved := dynCfg.GetReservedOrgNames()
	assert.True(t, reserved["acme"], "an accepted row's static org is reserved against the dynamic mapping")
	assert.True(t, reserved["config-org"], "the ConfigMap's own static orgs stay reserved")

	_, _, err := dynCfg.GetOrgDataFromClaim(orgClaims("acme"), "acme")
	assert.ErrorIs(t, err, core.ErrReservedOrgName, "the dynamic mapping cannot claim an org a row owns")

	// Withdrawing the row releases the name, because reload recomputes the whole set.
	require.NoError(t, cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		return dao.Delete(ctx, tx, row.ID)
	}))
	require.NoError(t, c.ReloadDBIssuers(ctx, dbSession))
	assert.False(t, dynCfg.GetReservedOrgNames()["acme"], "a deleted row no longer reserves its org")

	orgData, _, err := dynCfg.GetOrgDataFromClaim(orgClaims("acme"), "acme")
	require.NoError(t, err, "with the row gone the dynamic mapping serves the org again")
	assert.Contains(t, orgData, "acme")
}

// TestResolverReservesResolvedOrgName closes the same gap on the token path. A row
// this replica has not reloaded yet becomes live through the resolver, so the
// reservation cannot wait for the next reload tick.
func TestResolverReservesResolvedOrgName(t *testing.T) {
	ctx := context.Background()
	c, dbSession, dao, dynCfg := newDynamicConfigMapIssuerEnv(t)
	defer dbSession.Close()
	c.InstallIssuerResolver(dbSession)

	const acmeURL = "https://idp.acme.com"
	seedIssuerWithOrg(t, ctx, dbSession, dao, acmeURL, unreachableJWKSURL, "acme")
	require.False(t, dynCfg.GetReservedOrgNames()["acme"], "not reserved before the row is seen")

	require.NotNil(t, c.TokenOriginConfig.ResolveConfig(ctx, acmeURL), "the row resolves on first use")
	assert.True(t, dynCfg.GetReservedOrgNames()["acme"], "resolution reserves the org it publishes")

	_, _, err := dynCfg.GetOrgDataFromClaim(orgClaims("acme"), "acme")
	assert.ErrorIs(t, err, core.ErrReservedOrgName)
}

// ~~~~~ helpers ~~~~~ //

func firstByURL(ctx context.Context, dao cdbm.IssuerDAO, url string) (*cdbm.Issuer, error) {
	all, err := dao.GetAll(ctx, nil, cdbm.IssuerFilterInput{IssuerURL: &url})
	if err != nil {
		return nil, err
	}
	if len(all) == 0 {
		return nil, nil
	}
	return &all[0], nil
}

// newDynamicConfigMapIssuerEnv builds an environment whose ConfigMap holds a dynamic
// issuer plus a static one, with the dynamic issuer live in the registry exactly as
// GetOrInitTokenOriginConfig would have left it: reserving the ConfigMap's static orgs
// and nothing else.
func newDynamicConfigMapIssuerEnv(t *testing.T) (*Config, *cdb.Session, cdbm.IssuerDAO, *cauth.JwksConfig) {
	t.Helper()

	const dynIssuerURL = "https://dynamic.example.com"
	c, dbSession, dao := newIssuerTestEnvWithYAML(t, fmt.Sprintf(`
issuers:
  - issuer: %s
    jwks: %s/jwks
    origin: custom
    claimMappings:
      - orgAttribute: org
        rolesAttribute: roles
  - issuer: https://static.example.com
    jwks: https://static.example.com/jwks
    origin: custom
    claimMappings:
      - orgName: config-org
        roles: [TENANT_ADMIN]
`, dynIssuerURL, dynIssuerURL))

	dynCfg := cauth.NewJwksConfig(dynIssuerURL+"/jwks", dynIssuerURL, cauth.TokenOriginCustom, false, nil, nil)
	dynCfg.ClaimMappings = []cauth.ClaimMapping{{OrgAttribute: "org", RolesAttribute: "roles"}}
	dynCfg.SetReservedOrgNames(map[string]bool{"config-org": true})
	c.TokenOriginConfig.AddJwksConfig(dynCfg)

	return c, dbSession, dao, dynCfg
}

// orgClaims is a token body the dynamic mapping accepts, naming org.
func orgClaims(org string) jwt.MapClaims {
	return jwt.MapClaims{"org": org, "roles": []string{"TENANT_ADMIN"}}
}

func newIssuerTestEnv(t *testing.T) (*Config, *cdb.Session, cdbm.IssuerDAO) {
	t.Helper()

	return newIssuerTestEnvWithYAML(t, `
env:
  disconnected: true
issuers: []
`)
}

// newIssuerTestEnvWithYAML is newIssuerTestEnv with a caller-supplied static
// issuers block, for the cases where ConfigMap precedence is what is under test.
func newIssuerTestEnvWithYAML(t *testing.T, yamlDoc string) (*Config, *cdb.Session, cdbm.IssuerDAO) {
	t.Helper()

	c, err := NewConfigFromYAML(yamlDoc)
	require.NoError(t, err)
	c.v.SetDefault(ConfigEnvDisconnected, true)
	c.TokenOriginConfig = cauth.NewTokenOriginConfig()

	dbSession := dbutil.GetTestDBSession(t, false)
	require.NoError(t, dbSession.DB.ResetModel(context.Background(), (*cdbm.Issuer)(nil)))

	return c, dbSession, cdbm.NewIssuerDAO(dbSession)
}

func seedIssuer(t *testing.T, ctx context.Context, dbSession *cdb.Session, dao cdbm.IssuerDAO, issuerURL, jwksURL string) *cdbm.Issuer {
	t.Helper()

	return seedIssuerWithOrg(t, ctx, dbSession, dao, issuerURL, jwksURL, "acme-org")
}

// seedIssuerWithOrg inserts an issuer row directly, bypassing the API, which is
// how a row that the combined-set rules reject can exist in the first place. Each
// caller passes a distinct org so several issuers can coexist without colliding on
// the org-uniqueness rule.
func seedIssuerWithOrg(t *testing.T, ctx context.Context, dbSession *cdb.Session, dao cdbm.IssuerDAO, issuerURL, jwksURL, orgName string) *cdbm.Issuer {
	t.Helper()

	var created *cdbm.Issuer
	err := cdb.WithTx(ctx, dbSession, func(tx *cdb.Tx) error {
		var derr error
		created, derr = dao.Create(ctx, tx, cdbm.IssuerCreateOrUpdateInput{
			Origin:        "custom",
			IssuerURL:     issuerURL,
			JWKSUrl:       jwksURL,
			ClaimMappings: []cdbm.ClaimMapping{{OrgName: orgName, Roles: []string{"TENANT_ADMIN"}}},
		})
		return derr
	})
	require.NoError(t, err)
	require.NotNil(t, created)

	return created
}
