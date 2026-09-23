// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package authentication

import (
	"bytes"
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	config2 "github.com/NVIDIA/infra-controller/rest-api/auth/pkg/config"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/core/claim"
	"github.com/NVIDIA/infra-controller/rest-api/auth/pkg/processors"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/config"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbu "github.com/NVIDIA/infra-controller/rest-api/db/pkg/util"
	"github.com/golang-jwt/jwt/v5"
	"github.com/google/uuid"
	"github.com/labstack/echo/v4"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	temporalClient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
)

const (
	// Header constants for tests
	starfleetIDHeader       = "NV-Actor-Id"
	legacyStarfleetIDHeader = "X-Starfleet-Id"
	ngcUserNameHeader       = "NV-Ngc-User-Name"
	ngcUserEmailHeader      = "X-Ngc-Email-Id"
	ngcRolesHeader          = "NV-Ngc-User-Roles"
	ngcOrgDisplayNameHeader = "NV-Ngc-Org-Display-Name"
)

var (
	// Test data for middleware tests
	testStarfleetID = "Cc1Y0zHND50CGarJ58C8V97fLA5deZ4_CDDwpDkrVec"

	// Sample JWT tokens that can be used in auth header
	sampleSsaToken       = "eyJraWQiOiIyYzU4ZTE4MC0xNDlhLTQ4MTgtOWJmYy01ZjJhNmI2ZGJkOGEiLCJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJudnNzYS1zdGctUHpEZzdFWUNHNkpJUW9WSHQtZHZEYndFTzg3d2FmeDBrV3dYN1I2b1lBayIsImF1ZCI6WyJudnNzYS1zdGctUHpEZzdFWUNHNkpJUW9WSHQtZHZEYndFTzg3d2FmeDBrV3dYN1I2b1lBayIsInM6eXR5bnhzZWZmeGw0dTRqc3dwbDhrNndmY2JqenVkaDFrOWRtbWxmbnF1dyJdLCJhenAiOiJudnNzYS1zdGctUHpEZzdFWUNHNkpJUW9WSHQtZHZEYndFTzg3d2FmeDBrV3dYN1I2b1lBayIsInNlcnZpY2UiOnsibmFtZSI6Ik5HQyBLYWl6ZW4gQVBJIFNlcnZpY2UgKEtBUyB2MikiLCJpZCI6InZucmR6bGxhZmszOGVxdGtrbmNzejZ5b2FhY3JuczlldDZ3MWRrZ2tqdWkifSwiaXNzIjoiaHR0cHM6Ly95dHlueHNlZmZ4bDR1NGpzd3BsOGs2d2ZjYmp6dWRoMWs5ZG1tbGZucXV3LnN0Zy5zc2EubnZpZGlhLmNvbSIsInNjb3BlcyI6WyJrYXMiXSwiZXhwIjoxNjk5Mzk1OTA5LCJ0b2tlbl90eXBlIjoic2VydmljZV9hY2NvdW50IiwiaWF0IjoxNjk5MzkyMzA5LCJqdGkiOiJjZmM4MTE1Yy0xYWE2LTRiYjAtOTcxZC04MmI1YmE2M2IyOGUifQ.aURkxSfBDM5vNLWxNhjcQ9dQRNWYNk3Ka005cCtysrFYZX2vMNm5LAvzAFInYuHKHJnBhYjPlvXsfRruoif_EQ"
	samplelegacyKasToken = "eyJraWQiOiJCM0QyOlpRSEI6TTVZWDpORkJKOkRGSTQ6VTRXWDpQSDVFOjRKWEg6Q05BWTpXWVRKOkFaV0M6UkFMSyIsImFsZyI6IlJTMjU2In0.eyJzdWIiOiJzdGctajhncHJpaTZzbWtlb2hrcDVqbWNmcmVlZG8iLCJhdWQiOiJuZ2MiLCJhY2Nlc3MiOlt7InR5cGUiOiJncm91cC9uZ2Mtc3RnIiwibmFtZSI6Indka3NhaGV3MXJxdiIsImFjdGlvbnMiOlsiZmxlZXRfY29tbWFuZF9hZG1pbiIsImZvcmdlX3Byb3ZpZGVyX2FkbWluIiwicmVnaXN0cnktcmVhZCIsInVzZXItYWRtaW4iLCJ1c2VyLXJlYWQiLCJ1c2VyLXdyaXRlIiwidXNlcl9hZG1pbiJdLCJtZXRhZGF0YSI6bnVsbH1dLCJpc3MiOiJzdGcuYXV0aC5uZ2MubnZpZGlhLmNvbSIsIm9wdGlvbnMiOltdLCJleHAiOjE3MDAxNzY5MDQsImlhdCI6MTcwMDE3MzMwNCwianRpIjoiZmQ3N2Y3MWEtMjc5Mi00YzZhLTlhZjktOWRhYjIzZTc1NGI0In0.HPvoFqVHjns8gQg82LHgAQX6eg3HOrHnqVrN5FNIsw_oz0K4qrd8lQ6PEooLXkUSv9fAaxwNwvbsqKnOpsy11suCx0kAPXvFR1cCQr0bJZYiGSO-LU7dUIv8LcQZ1VDHXvLaKzHvHuvd3eApzMZOj8zl8Eo0N5vN9cnwX-znqhhSLP_bb1qOWhzV3_CNT4_cxreMfo7ibvKC-Vj0li9yUfle2ohW5knSwuwqkc2Sr0kK40MC4X5ugFS34fa5aDQcJEJsXa0ycAYWDAFyeNwRRWHad6vhoDY37rNHAtlTEEROWw7ypSB6g-3seLB6CvYPSwBofQipe2CY96T31DGCJdRs63OfMkl8wIzkgferU-FvNuu50kZEzwuSqHYbwfseqcfyVcgMR9Tv_Z1fhpdF_i3kQS5_V70_SO3d-LygbnQyvASP36eJt1EcO2lXjx-XkGuqkEw2gRR1QsmQtAITyPmU3D_xvjj6PwwseIbWTTFzykv1bOuFO5M3DdY4g9tnC7Z2UvVg0eTeRa3Z3cm0xlLgowxYI7RvRG-gzn5ruRMdnPPTB0NCj3rAp5SsqNeJWgKC1qGQ-LDNvIsk4WeT7IXfK4FqJ2_9BzBnh3BkYaCxOCPNiq5nN-Tk5I8GQYdkVhyrQ8d9i1PjzUr1h57K3JzrL5z7XPJbhEvf6Yb7Ce4"

	// Sample JWKS
	legacyKasJwks = `{"keys":[{"kty":"RSA","e":"AQAB","use":"sig","kid":"B3D2:ZQHB:M5YX:NFBJ:DFI4:U4WX:PH5E:4JXH:CNAY:WYTJ:AZWC:RALK","n":"qcqYV-iYUV6JNcMh58qLlMt8d_6AzkJcqlR77hUzR-fismhwoerT2K9vIBOno30mjKsgJjoT4zhPA8q28Sqq_AMWh7wqoBr99O75YdUawjfcngvHKCvfihN2E1Z4f-C8ihtn8T6rh9VcldLDaEhUlCIisRBTY3lnw4recPKE-cC0ejgFeOnV5Ds5a_xb1sP9Dhwv_hqIR_1Khh_H6M6WfF3Tv3eAgMQWycjCQkAY47qwXi9DCkAOhJJwlP0djsHPYKfykMKe5MUfnbPE-bCYg7rQlZfdzd58zL2G9VUyOLzZtFhGwPCA6oRyqlKTKO1dN0_wjMXa_86L0GswW-etl0HRL1KlP8ctF1m99xQ3M5leE8JOeio0eUPJNLgssClxHEW75JSXYB6T8YJek41FjQttW2sZpw1L-iQYLWVA5bIx7QEqcu85EmQQik4mvq_azX53Mug6_5tJPitdox_LQf38RIANa5zhPYcwqObjTr8W0rxMjXFN0bRrZ5f_RaXqbSdh5vVWmdzsZu0xu0otujz50ZlR5rf0W5leTs1xTLwpHh1CC2jhThwcOFkXT46zqWaKE7rsik3bp79yKHA9wkqzQOK4TE_DGp8aPrfa_8CAR1iVkbpW4diHgV-XuHLhFFjQco3I6SzPt4Ael_JoldaH2bINKvPaJXKCi_Bm9L8"}]}`
	ssaJwks       = `{"keys":[{"kty":"EC","use":"sig","crv":"P-256","kid":"2c58e180-149a-4818-9bfc-5f2a6b6dbd8a","x":"d4Sa5NYfomfkYkSdQEUrTKHXEET2dNhyQVnEViA97L0","y":"dQTndo4VhAy1G3i0Z9V6tEq7Ii2ey59pAM-GFoaI5M8","alg":"ES256"}]}`
)

// Test issuers that should map to different token origins
const (
	ssaIssuer      = "https://ytynxseffxl4u4jswpl8k6wfcbjzudh1k9dmmlfnquw.stg.ssa.nvidia.com"
	kasIssuer      = "stg.auth.ngc.nvidia.com"
	keycloakIssuer = "http://localhost:8082/realms/nico"
)

func TestAuthProcessor(t *testing.T) {
	var logs bytes.Buffer
	previousLogger := log.Logger
	log.Logger = zerolog.New(&logs)
	t.Cleanup(func() {
		log.Logger = previousLogger
	})

	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	// Add user entry
	user := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		Email:       cutil.GetPtr("jdoe@test.com"),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
	}

	_, err = dbSession.DB.NewInsert().Model(user).Exec(context.Background())
	require.NoError(t, err)

	e := echo.New()

	tc := &tmocks.Client{}

	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/kas") {
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(legacyKasJwks))
		} else if strings.Contains(req.URL.Path, "/ssa") {
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(ssaJwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer func() { testServer.Close() }()

	toCfg := config2.NewTokenOriginConfig()
	toCfg.AddConfig("ssa", "ssa.nvidia.com", testServer.URL+"/ssa", config2.TokenOriginKasSsa, false, nil, nil)
	toCfg.AddConfig("kas", "authn.nvidia.com", testServer.URL+"/kas", config2.TokenOriginKasLegacy, false, nil, nil)

	// Initialize JWKS data for testing
	if err := toCfg.UpdateAllJWKS(); err != nil {
		t.Fatal(err)
	}

	// Initialize processors for testing
	encCfg := config.NewPayloadEncryptionConfig("test-encryption-key")
	processors.InitializeProcessors(toCfg, dbSession, tc, encCfg, nil)

	type args struct {
		ds           *cdb.Session
		toCfg        *config2.TokenOriginConfig
		tc           temporalClient.Client
		path         string
		pathOverride *string
		headers      map[string]string
		org          string
	}

	tests := []struct {
		name             string
		args             args
		wantErrMsg       string
		wantRespCode     int
		excludedLogValue string
	}{
		{
			name: "test auth processor error, empty org name",
			args: args{
				ds:    dbSession,
				toCfg: toCfg,
				tc:    tc,
				path:  "/v2/org/testorg/user/current",
				org:   "",
			},
			wantErrMsg:   "Organization name is required in request path",
			wantRespCode: http.StatusBadRequest,
		},
		{
			name: "test auth processor error, no authorization header",
			args: args{
				ds:    dbSession,
				toCfg: toCfg,
				tc:    tc,
				path:  "/v2/org/testorg/user/current",
				headers: map[string]string{
					starfleetIDHeader: testStarfleetID,
				},
				org: "testorg",
			},
			wantErrMsg:   "Request is missing authorization header",
			wantRespCode: http.StatusUnauthorized,
		},
		{
			name: "test auth processor error, malformed authorization header",
			args: args{
				ds:    dbSession,
				toCfg: toCfg,
				tc:    tc,
				path:  "/v2/org/testorg/user/current",
				headers: map[string]string{
					"Authorization": "nvapi-secret-payload",
				},
				org: "testorg",
			},
			wantErrMsg:       "Invalid Authorization header format",
			wantRespCode:     http.StatusUnauthorized,
			excludedLogValue: "secret-payload",
		},
		{
			name: "test auth processor error, invalid authorization header",
			args: args{
				ds:    dbSession,
				toCfg: toCfg,
				tc:    tc,
				path:  "/v2/org/testorg/user/current",
				headers: map[string]string{
					starfleetIDHeader: testStarfleetID,
					"Authorization":   "Bearer invalid-token",
				},
				org: "testorg",
			},
			wantErrMsg:   "Error parsing the token claims",
			wantRespCode: http.StatusUnauthorized,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			logs.Reset()
			rec := httptest.NewRecorder()

			req := httptest.NewRequest(http.MethodGet, tt.args.path, nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			for k, v := range tt.args.headers {
				req.Header.Set(k, v)
			}

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)

			if tt.args.pathOverride != nil {
				ec.SetPath(*tt.args.pathOverride)
			} else {
				ec.SetPath(tt.args.path)
			}

			apiErr := AuthProcessor(ec, tt.args.toCfg)

			assert.Equal(t, tt.wantRespCode, apiErr.Code)
			assert.Equal(t, tt.wantErrMsg, apiErr.Message)
			if tt.excludedLogValue != "" {
				assert.NotContains(t, logs.String(), tt.excludedLogValue)
			}
		})
	}
}

func Test_getUpdatedUserFromHeaders(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	// Add user who has no first name, last name, email or NGC org data
	noDataUser := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
	}

	_, err = dbSession.DB.NewInsert().Model(noDataUser).Exec(context.Background())
	require.NoError(t, err)

	// Add user who has first name, last name, email and NGC org data
	dataUser1 := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		Email:       cutil.GetPtr("john@test.com"),
		OrgData: cdbm.OrgData{
			"test-org": cdbm.Org{
				Name:        "test-org",
				DisplayName: "Test Org",
				Roles:       []string{"NICO_PROVIDER_ADMIN", "REGISTRY_READ", "USER_ADMIN"},
				Teams:       []cdbm.Team{},
			},
		},
	}

	dataUser2 := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Doe"),
		Email:       cutil.GetPtr("john@test.com"),
		OrgData: cdbm.OrgData{
			"test-org": cdbm.Org{
				Name:        "test-org",
				DisplayName: "Test Org",
				Roles:       []string{"REGISTRY_READ", "USER_ADMIN"},
				Teams:       []cdbm.Team{},
			},
		},
	}

	dataUser3 := &cdbm.User{
		ID:          uuid.New(),
		StarfleetID: cutil.GetPtr(uuid.New().String()),
		FirstName:   cutil.GetPtr("John"),
		LastName:    cutil.GetPtr("Dalton"),
		Email:       cutil.GetPtr("jdalton@test.com"),
		OrgData: cdbm.OrgData{
			"test-org": cdbm.Org{
				Name:        "test-org",
				DisplayName: "Test Org",
				Roles:       []string{"NICO_PROVIDER_ADMIN", "REGISTRY_READ", "USER_ADMIN"},
				Teams:       []cdbm.Team{},
			},
		},
	}
	_, err = dbSession.DB.NewInsert().Model(dataUser1).Exec(context.Background())
	assert.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(dataUser2).Exec(context.Background())
	assert.NoError(t, err)
	_, err = dbSession.DB.NewInsert().Model(dataUser3).Exec(context.Background())
	assert.NoError(t, err)

	e := echo.New()

	type args struct {
		headers      map[string]string
		existingUser cdbm.User
		org          string
		logger       zerolog.Logger
	}
	tests := []struct {
		name    string
		args    args
		want    *cdbm.User
		wantErr bool
	}{
		{
			name: "test user data update, existing values nil",
			args: args{
				headers: map[string]string{
					ngcUserNameHeader:       base64.StdEncoding.EncodeToString([]byte("Jane Smith")),
					ngcUserEmailHeader:      base64.StdEncoding.EncodeToString([]byte("jane@test.com")),
					ngcRolesHeader:          "nico_provider_admin,registry-read,user-admin",
					ngcOrgDisplayNameHeader: base64.StdEncoding.EncodeToString([]byte("Test Org")),
				},
				existingUser: *noDataUser,
				org:          "test-org",
				logger:       zerolog.Logger{},
			},
			want: &cdbm.User{
				FirstName: cutil.GetPtr("Jane"),
				LastName:  cutil.GetPtr("Smith"),
				Email:     cutil.GetPtr("jane@test.com"),
				OrgData: cdbm.OrgData{
					"test-org": cdbm.Org{
						Name:        "test-org",
						DisplayName: "Test Org",
						Roles:       []string{"NICO_PROVIDER_ADMIN", "REGISTRY_READ", "USER_ADMIN"},
						Teams:       []cdbm.Team{},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test user data update, existing values not nil",
			args: args{
				headers: map[string]string{
					ngcUserNameHeader:       base64.StdEncoding.EncodeToString([]byte("John Robert Smith")),
					ngcUserEmailHeader:      base64.StdEncoding.EncodeToString([]byte("jrsmith@test.com")),
					ngcRolesHeader:          "nico_provider_admin,registry-read,user-admin",
					ngcOrgDisplayNameHeader: base64.StdEncoding.EncodeToString([]byte("Test Organization")),
				},
				existingUser: *dataUser1,
				org:          "test-org",
				logger:       zerolog.Logger{},
			},
			want: &cdbm.User{
				LastName: cutil.GetPtr("Robert Smith"),
				Email:    cutil.GetPtr("jrsmith@test.com"),
				OrgData: cdbm.OrgData{
					"test-org": cdbm.Org{
						Name:        "test-org",
						DisplayName: "Test Organization",
						Roles:       []string{"NICO_PROVIDER_ADMIN", "REGISTRY_READ", "USER_ADMIN"},
						Teams:       []cdbm.Team{},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test user data update, existing values not nil",
			args: args{
				headers: map[string]string{
					ngcUserNameHeader:       base64.StdEncoding.EncodeToString([]byte("John Doe")),
					ngcUserEmailHeader:      base64.StdEncoding.EncodeToString([]byte("jdoe@test.com")),
					ngcRolesHeader:          "nico_provider_admin,registry-read,user-admin",
					ngcOrgDisplayNameHeader: base64.StdEncoding.EncodeToString([]byte("SRE Org")),
				},
				existingUser: *dataUser2,
				org:          "sre-org",
				logger:       zerolog.Logger{},
			},
			want: &cdbm.User{
				Email: cutil.GetPtr("jdoe@test.com"),
				OrgData: cdbm.OrgData{
					"test-org": cdbm.Org{
						Name:        "test-org",
						DisplayName: "Test Org",
						Roles:       []string{"REGISTRY_READ", "USER_ADMIN"},
						Teams:       []cdbm.Team{},
					},
					"sre-org": cdbm.Org{
						Name:        "sre-org",
						DisplayName: "SRE Org",
						Roles:       []string{"NICO_PROVIDER_ADMIN", "REGISTRY_READ", "USER_ADMIN"},
						Teams:       []cdbm.Team{},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test user data update, no name or email headers",
			args: args{
				headers: map[string]string{
					ngcRolesHeader:          "nico_provider_admin,registry-read,user-admin",
					ngcOrgDisplayNameHeader: base64.StdEncoding.EncodeToString([]byte("Test Organization")),
				},
				existingUser: *dataUser3,
				org:          "test-org",
				logger:       zerolog.Logger{},
			},
			want: &cdbm.User{
				OrgData: cdbm.OrgData{
					"test-org": cdbm.Org{
						Name:        "test-org",
						DisplayName: "Test Organization",
						Roles:       []string{"NICO_PROVIDER_ADMIN", "REGISTRY_READ", "USER_ADMIN"},
						Teams:       []cdbm.Team{},
					},
				},
			},
			wantErr: false,
		},
		{
			name: "test user data update, invalid user name header",
			args: args{
				headers: map[string]string{
					ngcUserNameHeader:       "invalid-base64-encoded-name",
					ngcRolesHeader:          "nico_provider_admin,registry-read,user-admin",
					ngcOrgDisplayNameHeader: base64.StdEncoding.EncodeToString([]byte("Test Organization")),
				},
				existingUser: *dataUser3,
				org:          "test-org",
				logger:       zerolog.Logger{},
			},
			want:    nil,
			wantErr: true,
		},
		{
			name: "test user data update, no NGC roles header",
			args: args{
				headers: map[string]string{
					ngcOrgDisplayNameHeader: base64.StdEncoding.EncodeToString([]byte("Test Organization")),
				},
				existingUser: *dataUser3,
				org:          "test-org",
				logger:       zerolog.Logger{},
			},
			want:    nil,
			wantErr: true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := httptest.NewRecorder()

			req := httptest.NewRequest(http.MethodGet, "/v2/org/test-org/user/current", nil)
			req.Header.Set(echo.HeaderContentType, echo.MIMEApplicationJSON)
			for k, v := range tt.args.headers {
				req.Header.Set(k, v)
			}

			ec := e.NewContext(req, rec)
			ec.SetParamNames("orgName")
			ec.SetParamValues(tt.args.org)

			got, apiErr := processors.GetUpdatedUserFromHeaders(ec, tt.args.existingUser, tt.args.org, tt.args.logger)
			if tt.wantErr {
				assert.NotNil(t, apiErr)
				return
			} else {
				require.Nil(t, apiErr, fmt.Sprintf("Unexpected API error: %v", apiErr))
				if tt.want != nil {
					require.NotNil(t, got)
				}
			}

			if got == nil {
				t.Errorf("Got error: %v, but expected user", err)
			}

			if tt.want.FirstName != nil {
				assert.Equal(t, *tt.want.FirstName, *got.FirstName)
			}
			if tt.want.LastName != nil {
				assert.Equal(t, *tt.want.LastName, *got.LastName)
			}
			if tt.want.Email != nil {
				assert.Equal(t, *tt.want.Email, *got.Email)
			}
			if tt.want.OrgData != nil {
				// Compare OrgData fields individually, ignoring the Updated timestamp
				assert.Equal(t, len(tt.want.OrgData), len(got.OrgData), "OrgData should have same number of orgs")
				for orgName, expectedOrg := range tt.want.OrgData {
					actualOrg, exists := got.OrgData[orgName]
					assert.True(t, exists, "Org %s should exist in result", orgName)
					if exists {
						assert.Equal(t, expectedOrg.Name, actualOrg.Name, "Org name should match")
						assert.Equal(t, expectedOrg.DisplayName, actualOrg.DisplayName, "DisplayName should match")
						assert.Equal(t, expectedOrg.Roles, actualOrg.Roles, "Roles should match")
						assert.Equal(t, expectedOrg.Teams, actualOrg.Teams, "Teams should match")
						// Note: Updated timestamp is intentionally not compared as it's set dynamically
					}
				}
			}
		})
	}
}

// TestValidateJWTTokens_WithoutExpiration tests JWT token validation bypassing expiration checks
func TestValidateJWTTokens_WithoutExpiration(t *testing.T) {
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		if strings.Contains(req.URL.Path, "/kas") {
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(legacyKasJwks))
		} else if strings.Contains(req.URL.Path, "/ssa") {
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(ssaJwks))
		} else {
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer func() { testServer.Close() }()

	cfg := config2.NewTokenOriginConfig()
	cfg.AddConfig("ssa", "ssa.nvidia.com", testServer.URL+"/ssa", config2.TokenOriginKasSsa, false, nil, nil)
	cfg.AddConfig("kas", "authn.nvidia.com", testServer.URL+"/kas", config2.TokenOriginKasLegacy, false, nil, nil)

	// Initialize JWKS data for testing
	err := cfg.UpdateAllJWKS()
	require.NoError(t, err)

	tests := []struct {
		name        string
		token       string
		origin      string
		jwksConfig  *config2.JwksConfig
		expectValid bool
	}{
		{
			name:        "validate SSA token signature bypassing expiration",
			token:       sampleSsaToken,
			origin:      config2.TokenOriginKasSsa,
			jwksConfig:  cfg.GetFirstConfigByOrigin(config2.TokenOriginKasSsa),
			expectValid: true,
		},
		{
			name:        "validate legacy KAS token signature bypassing expiration",
			token:       samplelegacyKasToken,
			origin:      config2.TokenOriginKasLegacy,
			jwksConfig:  cfg.GetFirstConfigByOrigin(config2.TokenOriginKasLegacy),
			expectValid: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Parse the token without standard claims validation (including expiration)
			// Use comprehensive algorithm list instead of dynamic detection
			allCommonAlgorithms := []string{
				"RS256", "RS384", "RS512", // RSA with SHA
				"PS256", "PS384", "PS512", // RSA-PSS with SHA
				"ES256", "ES384", "ES512", // ECDSA with SHA
				"HS256", "HS384", "HS512", // HMAC with SHA
				"EdDSA", // Ed25519/Ed448
			}
			jwtParser := jwt.NewParser(jwt.WithValidMethods(allCommonAlgorithms), jwt.WithoutClaimsValidation())

			var claims jwt.Claims
			switch tt.origin {
			case config2.TokenOriginKasLegacy:
				claims = &claim.NgcKasClaims{}
			case config2.TokenOriginKasSsa:
				claims = &claim.SsaClaims{}
			default:
				t.Fatalf("unsupported token origin: %s", tt.origin)
			}

			// Create a key function that retrieves the public key from JWKS
			keyFunc := func(token *jwt.Token) (interface{}, error) {
				// Retrieve the public key from JWKS
				kidIfc, ok := token.Header["kid"]
				if !ok {
					return nil, fmt.Errorf("could not find KID in JWT header")
				}
				kid, ok := kidIfc.(string)
				if !ok {
					return nil, fmt.Errorf("could not convert KID in JWT header to string")
				}
				return tt.jwksConfig.GetKeyByID(kid)
			}

			token, err := jwtParser.ParseWithClaims(tt.token, claims, keyFunc)

			if tt.expectValid {
				assert.NoError(t, err, "Token parsing should succeed when bypassing expiration")
				assert.NotNil(t, token, "Token should not be nil")
				assert.NotNil(t, token.Claims, "Token claims should not be nil")

				// Verify that the signature is valid (token.Valid should be true when signature is correct)
				assert.True(t, token.Valid, "Token signature should be valid")

				// Additional checks based on token type
				switch tt.origin {
				case config2.TokenOriginKasSsa:
					ssaClaims, ok := token.Claims.(*claim.SsaClaims)
					assert.True(t, ok, "Claims should be SSA claims")
					assert.NotEmpty(t, ssaClaims.Subject, "SSA token should have subject")
					assert.NotEmpty(t, ssaClaims.Issuer, "SSA token should have issuer")
					t.Logf("SSA Token - Subject: %s, Issuer: %s", ssaClaims.Subject, ssaClaims.Issuer)
				case config2.TokenOriginKasLegacy:
					kasClaims, ok := token.Claims.(*claim.NgcKasClaims)
					assert.True(t, ok, "Claims should be NGC KAS claims")
					assert.NotEmpty(t, kasClaims.Subject, "KAS token should have subject")
					assert.NotEmpty(t, kasClaims.Issuer, "KAS token should have issuer")
					t.Logf("KAS Token - Subject: %s, Issuer: %s", kasClaims.Subject, kasClaims.Issuer)
				}
			} else {
				assert.Error(t, err, "Token parsing should fail")
			}
		})
	}
}

// TestJWTTokenStructureAndClaims tests the structure and claims of the sample tokens
func TestJWTTokenStructureAndClaims(t *testing.T) {
	tests := []struct {
		name        string
		token       string
		origin      string
		expectedKID string
		expectedAlg string
	}{
		{
			name:        "SSA token structure",
			token:       sampleSsaToken,
			origin:      config2.TokenOriginKasSsa,
			expectedKID: "2c58e180-149a-4818-9bfc-5f2a6b6dbd8a",
			expectedAlg: "ES256",
		},
		{
			name:        "Legacy KAS token structure",
			token:       samplelegacyKasToken,
			origin:      config2.TokenOriginKasLegacy,
			expectedKID: "B3D2:ZQHB:M5YX:NFBJ:DFI4:U4WX:PH5E:4JXH:CNAY:WYTJ:AZWC:RALK",
			expectedAlg: "RS256",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Parse token without verification to examine structure
			token, _, err := new(jwt.Parser).ParseUnverified(tt.token, jwt.MapClaims{})
			require.NoError(t, err, "Token should be parseable")

			// Check header claims
			assert.Equal(t, tt.expectedKID, token.Header["kid"], "Token should have expected key ID")
			assert.Equal(t, tt.expectedAlg, token.Header["alg"], "Token should have expected algorithm")

			// Check that claims are accessible
			claims, ok := token.Claims.(jwt.MapClaims)
			require.True(t, ok, "Claims should be MapClaims")

			// Verify basic JWT claims exist
			assert.Contains(t, claims, "sub", "Token should have subject claim")
			assert.Contains(t, claims, "iss", "Token should have issuer claim")
			assert.Contains(t, claims, "exp", "Token should have expiration claim")
			assert.Contains(t, claims, "iat", "Token should have issued at claim")

			// Log token details for visibility
			sub, _ := claims["sub"].(string)
			iss, _ := claims["iss"].(string)
			exp, _ := claims["exp"].(float64)
			iat, _ := claims["iat"].(float64)

			t.Logf("Token Details - KID: %s, Alg: %s, Subject: %s, Issuer: %s",
				tt.expectedKID, tt.expectedAlg, sub, iss)
			t.Logf("Token Timestamps - Issued At: %v, Expires At: %v",
				time.Unix(int64(iat), 0), time.Unix(int64(exp), 0))
		})
	}
}

// TestHandlerInterface_TokenOriginRouting tests our new handler interface with different token origins
func TestHandlerInterface_TokenOriginRouting(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	e := echo.New()
	tc := &tmocks.Client{}

	// Setup test server that serves different JWKS for different paths
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		switch {
		case strings.Contains(req.URL.Path, "/ssa"):
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(ssaJwks))
		case strings.Contains(req.URL.Path, "/kas"):
			res.Header().Set("Content-Type", "application/json")
			res.WriteHeader(http.StatusOK)
			res.Write([]byte(legacyKasJwks))
		default:
			res.WriteHeader(http.StatusNotFound)
		}
	}))
	defer testServer.Close()

	// Setup token origin config with multiple token origins
	toCfg := config2.NewTokenOriginConfig()
	toCfg.AddConfig("ssa", ssaIssuer, testServer.URL+"/ssa", config2.TokenOriginKasSsa, false, nil, nil)
	toCfg.AddConfig("kas", kasIssuer, testServer.URL+"/kas", config2.TokenOriginKasLegacy, false, nil, nil)

	// Initialize JWKS data
	require.NoError(t, toCfg.UpdateAllJWKS())

	// Initialize processors
	encCfg := config.NewPayloadEncryptionConfig("test-encryption-key")
	processors.InitializeProcessors(toCfg, dbSession, tc, encCfg, nil)

	tests := []struct {
		name          string
		token         string
		issuer        string
		origin        string
		expectedError string
		headers       map[string]string
		shouldPass    bool
		validateError func(*testing.T, error)
	}{
		{
			name:       "SSA token routing to SSA handler",
			token:      sampleSsaToken,
			issuer:     ssaIssuer,
			origin:     config2.TokenOriginKasSsa,
			shouldPass: false, // Should fail due to expiration
			headers: map[string]string{
				starfleetIDHeader: testStarfleetID,
			},
			validateError: func(t *testing.T, err error) {
				assert.True(t, errors.Is(err, jwt.ErrTokenExpired), "Should be token expired error")
			},
		},
		{
			name:       "KAS token routing to KAS handler",
			token:      samplelegacyKasToken,
			issuer:     kasIssuer,
			origin:     config2.TokenOriginKasLegacy,
			shouldPass: false, // Should fail due to expiration
			validateError: func(t *testing.T, err error) {
				assert.True(t, errors.Is(err, jwt.ErrTokenExpired), "Should be token expired error")
			},
		},
		{
			name:          "Unknown issuer - no handler found",
			token:         "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJodHRwczovL3Vua25vd24uaXNzdWVyLmNvbSIsInN1YiI6InVzZXItMTIzIiwiZXhwIjo5OTk5OTk5OTk5fQ.invalid-signature", // Fake token with unknown issuer
			issuer:        "https://unknown.issuer.com",
			shouldPass:    false,
			expectedError: "Invalid authorization token in request", // This is what AuthProcessor returns for no handler found
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Verify handler exists for known origins
			if tt.origin != "" {
				handler := toCfg.GetProcessorByOrigin(tt.origin)
				assert.NotNil(t, handler, "Handler should exist for origin %s", tt.origin)

				// Test handler directly
				req := httptest.NewRequest(http.MethodGet, "/v2/org/testorg/user/current", nil)
				for k, v := range tt.headers {
					req.Header.Set(k, v)
				}

				rec := httptest.NewRecorder()
				c := e.NewContext(req, rec)
				c.SetParamNames("orgName")
				c.SetParamValues("testorg")
				c.Set("orgName", "testorg")

				logger := log.With().Str("test", tt.name).Logger()
				_, apiErr := handler.ProcessToken(c, tt.token, toCfg.GetConfig(tt.issuer), logger)

				if tt.shouldPass {
					assert.Nil(t, apiErr, "Handler should succeed")
				} else {
					assert.NotNil(t, apiErr, "Handler should fail")
					if tt.validateError != nil {
						// Extract the underlying JWT error from the API error
						// API errors contain the original error information
						assert.Contains(t, apiErr.Message, "expired", "Should contain expired message")
					}
				}
			}

			// Test via AuthProcessor
			req := httptest.NewRequest(http.MethodGet, "/v2/org/testorg/user/current", nil)
			req.Header.Set("Authorization", "Bearer "+tt.token)
			for k, v := range tt.headers {
				req.Header.Set(k, v)
			}

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)
			c.SetParamNames("orgName")
			c.SetParamValues("testorg")
			c.SetPath("/v2/org/testorg/user/current")

			apiErr := AuthProcessor(c, toCfg)

			if tt.shouldPass {
				assert.Nil(t, apiErr, "AuthProcessor should succeed")
				user := c.Get("user")
				assert.NotNil(t, user, "User should be set in context")
			} else {
				assert.NotNil(t, apiErr, "AuthProcessor should fail")
				if tt.expectedError != "" {
					assert.Contains(t, apiErr.Message, tt.expectedError)
				}
			}
		})
	}
}

// TestProcessorInterface_ErrorScenarios tests specific JWT error scenarios with our processors
func TestProcessorInterface_ErrorScenarios(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	// Create user table
	err := dbSession.DB.ResetModel(context.Background(), (*cdbm.User)(nil))
	require.NoError(t, err)

	e := echo.New()
	tc := &tmocks.Client{}
	encCfg := config.NewPayloadEncryptionConfig("test-encryption-key")

	tests := []struct {
		name          string
		setupServer   func() *httptest.Server
		issuer        string
		origin        string
		token         string
		expectedError error
		headers       map[string]string
	}{
		{
			name: "SSA token with expired timestamp",
			setupServer: func() *httptest.Server {
				return httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
					res.Header().Set("Content-Type", "application/json")
					res.WriteHeader(http.StatusOK)
					res.Write([]byte(ssaJwks))
				}))
			},
			issuer:        ssaIssuer,
			origin:        config2.TokenOriginKasSsa,
			token:         sampleSsaToken, // This token is expired
			expectedError: jwt.ErrTokenExpired,
			headers: map[string]string{
				starfleetIDHeader: testStarfleetID,
			},
		},
		{
			name: "KAS token with expired timestamp",
			setupServer: func() *httptest.Server {
				return httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
					res.Header().Set("Content-Type", "application/json")
					res.WriteHeader(http.StatusOK)
					res.Write([]byte(legacyKasJwks))
				}))
			},
			issuer:        kasIssuer,
			origin:        config2.TokenOriginKasLegacy,
			token:         samplelegacyKasToken, // This token is expired
			expectedError: jwt.ErrTokenExpired,
		},
		{
			name: "Malformed token - invalid structure",
			setupServer: func() *httptest.Server {
				return httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
					res.Header().Set("Content-Type", "application/json")
					res.WriteHeader(http.StatusOK)
					res.Write([]byte(ssaJwks))
				}))
			},
			issuer:        ssaIssuer,
			origin:        config2.TokenOriginKasSsa,
			token:         "invalid.token.structure", // Malformed token
			expectedError: jwt.ErrTokenMalformed,
			headers: map[string]string{
				starfleetIDHeader: testStarfleetID,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			server := tt.setupServer()
			defer server.Close()

			// Setup token origin config
			cfg := config2.NewTokenOriginConfig()

			// Add appropriate config based on the test case origin
			switch tt.origin {
			case config2.TokenOriginKasSsa:
				cfg.AddConfig("ssa", tt.issuer, server.URL, tt.origin, false, nil, nil)
			case config2.TokenOriginKasLegacy:
				cfg.AddConfig("kas", tt.issuer, server.URL, tt.origin, false, nil, nil)
			default:
				cfg.AddConfig("default", tt.issuer, server.URL, tt.origin, false, nil, nil)
			}

			require.NoError(t, cfg.UpdateAllJWKS())

			// Initialize processors
			processors.InitializeProcessors(cfg, dbSession, tc, encCfg, nil)

			// Test handler directly
			handler := cfg.GetProcessorByOrigin(tt.origin)
			require.NotNil(t, handler, "Handler should exist for origin %d", tt.origin)

			req := httptest.NewRequest(http.MethodGet, "/v2/org/testorg/user/current", nil)
			for k, v := range tt.headers {
				req.Header.Set(k, v)
			}

			rec := httptest.NewRecorder()
			c := e.NewContext(req, rec)
			c.SetParamNames("orgName")
			c.SetParamValues("testorg")
			c.Set("orgName", "testorg")

			logger := log.With().Str("test", tt.name).Logger()

			// Call handler directly and verify error type
			_, apiErr := handler.ProcessToken(c, tt.token, cfg.GetConfig(tt.issuer), logger)

			require.NotNil(t, apiErr, "Handler should return an error")

			// Check that the API error contains information about the specific JWT error
			switch tt.expectedError {
			case jwt.ErrTokenExpired:
				assert.Contains(t, apiErr.Message, "expired", "API error should mention expiration")
			case jwt.ErrTokenMalformed:
				assert.Contains(t, apiErr.Message, "Invalid", "API error should mention invalid token")
			case jwt.ErrTokenSignatureInvalid:
				assert.Contains(t, apiErr.Message, "Invalid", "API error should mention invalid token")
			}

			t.Logf("Handler returned expected error for %s: %s", tt.name, apiErr.Message)
		})
	}
}

// TestProcessorInterface_Mapping tests that processors are properly mapped and retrieved by origin/issuer
func TestProcessorInterface_Mapping(t *testing.T) {
	dbSession := cdbu.GetTestDBSession(t, false)
	defer dbSession.Close()

	tc := &tmocks.Client{}
	encCfg := config.NewPayloadEncryptionConfig("test-encryption-key")

	// Setup test server
	testServer := httptest.NewServer(http.HandlerFunc(func(res http.ResponseWriter, req *http.Request) {
		res.Header().Set("Content-Type", "application/json")
		res.WriteHeader(http.StatusOK)
		res.Write([]byte(ssaJwks)) // Use valid JWKS
	}))
	defer testServer.Close()

	// Setup token origin config
	cfg := config2.NewTokenOriginConfig()
	cfg.AddConfig("ssa", ssaIssuer, testServer.URL, config2.TokenOriginKasSsa, false, nil, nil)
	cfg.AddConfig("kas", kasIssuer, testServer.URL, config2.TokenOriginKasLegacy, false, nil, nil)
	cfg.AddConfig("keycloak", keycloakIssuer, testServer.URL, config2.TokenOriginKeycloak, true, nil, nil)

	require.NoError(t, cfg.UpdateAllJWKS())

	// Initialize processors
	processors.InitializeProcessors(cfg, dbSession, tc, encCfg, nil)

	t.Run("verify_processor_mapping_by_origin", func(t *testing.T) {
		ssaProcessor := cfg.GetProcessorByOrigin(config2.TokenOriginKasSsa)
		kasProcessor := cfg.GetProcessorByOrigin(config2.TokenOriginKasLegacy)
		keycloakProcessor := cfg.GetProcessorByOrigin(config2.TokenOriginKeycloak)

		assert.NotNil(t, ssaProcessor, "SSA processor should be initialized")
		assert.NotNil(t, kasProcessor, "KAS processor should be initialized")
		assert.NotNil(t, keycloakProcessor, "Keycloak processor should be initialized")

		// Verify they're different processors
		assert.NotEqual(t, ssaProcessor, kasProcessor, "SSA and KAS processors should be different")
		assert.NotEqual(t, ssaProcessor, keycloakProcessor, "SSA and Keycloak processors should be different")
		assert.NotEqual(t, kasProcessor, keycloakProcessor, "KAS and Keycloak processors should be different")
	})

	t.Run("verify_processor_mapping_by_issuer", func(t *testing.T) {
		ssaProcessor := cfg.GetProcessorByOrigin(config2.TokenOriginKasSsa)
		kasProcessor := cfg.GetProcessorByOrigin(config2.TokenOriginKasLegacy)
		keycloakProcessor := cfg.GetProcessorByOrigin(config2.TokenOriginKeycloak)

		// Test exact issuer matching
		assert.Equal(t, ssaProcessor, cfg.GetProcessorByIssuer(ssaIssuer), "Should route SSA issuer to SSA processor")
		assert.Equal(t, kasProcessor, cfg.GetProcessorByIssuer(kasIssuer), "Should route KAS issuer to KAS processor")
		assert.Equal(t, keycloakProcessor, cfg.GetProcessorByIssuer(keycloakIssuer), "Should route Keycloak issuer to Keycloak processor")

		// Test unknown issuer
		assert.Nil(t, cfg.GetProcessorByIssuer("https://completely.unknown.issuer.com"), "Should return nil for unknown issuer")
	})
}
