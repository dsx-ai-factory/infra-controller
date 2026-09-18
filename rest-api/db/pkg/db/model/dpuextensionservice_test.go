// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	otrace "go.opentelemetry.io/otel/trace"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestDpuExtensionServiceVersionInfo_FromProto(t *testing.T) {
	fallbackTime := db.GetCurTime()
	obsName := "service-metrics"

	tests := []struct {
		desc                string
		protoVersionInfo    *corev1.DpuExtensionServiceVersionInfo
		expectedCreatedTime time.Time
	}{
		{
			desc: "parses created timestamp and observability from proto",
			protoVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
				Version:       "V1-T1761856992374052",
				Data:          "apiVersion: v1\nkind: Pod",
				HasCredential: true,
				Created:       "2026-03-31 12:34:56.123456 UTC",
				Observability: &corev1.DpuExtensionServiceObservability{
					Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
						{
							Name: &obsName,
							Config: &corev1.DpuExtensionServiceObservabilityConfig_Prometheus{
								Prometheus: &corev1.DpuExtensionServiceObservabilityConfigPrometheus{
									ScrapeIntervalSeconds: 30,
									Endpoint:              "service:9090",
								},
							},
						},
					},
				},
			},
			expectedCreatedTime: time.Date(2026, 3, 31, 12, 34, 56, 123456000, time.UTC),
		},
		{
			desc: "falls back to provided time when created timestamp is invalid",
			protoVersionInfo: &corev1.DpuExtensionServiceVersionInfo{
				Version:       "V2-T1761856992374053",
				Data:          "apiVersion: v1\nkind: Pod",
				HasCredential: false,
				Created:       "not-a-time",
			},
			expectedCreatedTime: fallbackTime,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := &DpuExtensionServiceVersionInfo{}

			got.FromProto(tc.protoVersionInfo, fallbackTime)

			assert.Equal(t, tc.protoVersionInfo.Version, got.Version)
			assert.Equal(t, tc.protoVersionInfo.Data, got.Data)
			assert.Equal(t, tc.protoVersionInfo.HasCredential, got.HasCredentials)
			assert.Equal(t, tc.expectedCreatedTime, got.Created)

			if tc.protoVersionInfo.Observability == nil {
				assert.Nil(t, got.Observability)
				return
			}

			if assert.NotNil(t, got.Observability) &&
				assert.Len(t, tc.protoVersionInfo.Observability.Configs, 1) &&
				assert.Len(t, got.Observability.Configs, 1) {
				assert.Equal(t,
					tc.protoVersionInfo.Observability.Configs[0].GetPrometheus().GetEndpoint(),
					got.Observability.Configs[0].GetPrometheus().GetEndpoint(),
				)
			}
		})
	}
}

// reset the tables needed for DpuExtensionService tests
func testDpuExtensionServiceSetupSchema(t *testing.T, dbSession *db.Session) {
	// create User table
	err := dbSession.DB.ResetModel(context.Background(), (*User)(nil))
	assert.Nil(t, err)
	// create InfrastructureProvider table
	err = dbSession.DB.ResetModel(context.Background(), (*InfrastructureProvider)(nil))
	assert.Nil(t, err)
	// create Site table
	err = dbSession.DB.ResetModel(context.Background(), (*Site)(nil))
	assert.Nil(t, err)
	// create Tenant table
	err = dbSession.DB.ResetModel(context.Background(), (*Tenant)(nil))
	assert.Nil(t, err)
	// create DpuExtensionService table
	err = dbSession.DB.ResetModel(context.Background(), (*DpuExtensionService)(nil))
	assert.Nil(t, err)
}

func TestDpuExtensionServiceSQLDAO_Create(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)
	tenant := TestBuildTenant(t, dbSession, "test-tenant", "test-org", user)

	dessd := NewDpuExtensionServiceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	version := "V1-T1761856992374052"
	obsName := "service-metrics"
	versionInfo := &DpuExtensionServiceVersionInfo{
		Version:        version,
		Data:           "test-data",
		HasCredentials: true,
		Created:        db.GetCurTime(),
		Observability: &DpuExtensionServiceObservability{
			DpuExtensionServiceObservability: &corev1.DpuExtensionServiceObservability{
				Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
					{
						Name: &obsName,
						Config: &corev1.DpuExtensionServiceObservabilityConfig_Prometheus{
							Prometheus: &corev1.DpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "http://service:9090/metrics",
							},
						},
					},
				},
			},
		},
	}
	description := "Test DPU extension service"

	tests := []struct {
		desc               string
		inputs             []DpuExtensionServiceCreateInput
		expectError        bool
		verifyChildSpanner bool
	}{
		{
			desc: "create one",
			inputs: []DpuExtensionServiceCreateInput{
				{
					DpuExtensionServiceID: cutil.GetPtr(uuid.New()),
					Name:                  "test-service-1",
					Description:           &description,
					ServiceType:           DpuExtensionServiceServiceTypeKubernetesPod,
					SiteID:                site.ID,
					TenantID:              tenant.ID,
					Version:               &version,
					VersionInfo:           versionInfo,
					ActiveVersions:        []string{version},
					Status:                DpuExtensionServiceStatusPending,
					CreatedBy:             user.ID,
				},
			},
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc: "create multiple, some with nullable fields",
			inputs: []DpuExtensionServiceCreateInput{
				{
					DpuExtensionServiceID: cutil.GetPtr(uuid.New()),
					Name:                  "test-service-2",
					Description:           &description,
					ServiceType:           DpuExtensionServiceServiceTypeKubernetesPod,
					SiteID:                site.ID,
					TenantID:              tenant.ID,
					Version:               &version,
					VersionInfo:           versionInfo,
					ActiveVersions:        []string{version},
					Status:                DpuExtensionServiceStatusReady,
					CreatedBy:             user.ID,
				},
				{
					DpuExtensionServiceID: nil,
					Name:                  "test-service-3",
					Description:           nil,
					ServiceType:           DpuExtensionServiceServiceTypeKubernetesPod,
					SiteID:                site.ID,
					TenantID:              tenant.ID,
					Version:               nil,
					VersionInfo:           nil,
					ActiveVersions:        []string{},
					Status:                DpuExtensionServiceStatusPending,
					CreatedBy:             user.ID,
				},
			},
			expectError: false,
		},
		{
			desc: "create with auto-generated UUID",
			inputs: []DpuExtensionServiceCreateInput{
				{
					Name:        "test-service-auto-uuid",
					ServiceType: DpuExtensionServiceServiceTypeKubernetesPod,
					SiteID:      site.ID,
					TenantID:    tenant.ID,
					Status:      DpuExtensionServiceStatusPending,
					CreatedBy:   user.ID,
				},
			},
			expectError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			for _, input := range tc.inputs {
				des, err := dessd.Create(ctx, nil, input)
				if err != nil {
					assert.True(t, tc.expectError)
					assert.Nil(t, des)
				} else {
					assert.False(t, tc.expectError)
					assert.NotNil(t, des)
					assert.Equal(t, input.Name, des.Name)
					assert.Equal(t, input.Description, des.Description)
					assert.Equal(t, input.ServiceType, des.ServiceType)
					assert.Equal(t, input.SiteID, des.SiteID)
					assert.Equal(t, input.TenantID, des.TenantID)
					assert.Equal(t, input.Status, des.Status)
					if input.Version != nil {
						assert.Equal(t, *input.Version, *des.Version)
					}
					if input.ActiveVersions != nil {
						assert.Equal(t, input.ActiveVersions, des.ActiveVersions)
					}
					if input.VersionInfo != nil && input.VersionInfo.Observability != nil {
						assert.Equal(t, input.VersionInfo.Observability.GetConfigs()[0].GetPrometheus().Endpoint, des.VersionInfo.Observability.GetConfigs()[0].GetPrometheus().Endpoint)
					}
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}

				if err != nil {
					t.Log(err.Error())
					return
				}
			}
		})
	}
}

func createSampleDpuExtensionServices(ctx context.Context, t *testing.T, dbSession *db.Session) (created []DpuExtensionService) {
	// Create test dependencies
	user := TestBuildUser(t, dbSession, "test-user", "test-org", []string{"admin"})
	ip := TestBuildInfrastructureProvider(t, dbSession, "test-provider", "test-org", user)
	site := TestBuildSite(t, dbSession, ip, "test-site", user)
	tenant := TestBuildTenant(t, dbSession, "test-tenant", "test-org", user)

	version1 := "V1-T1761856992374052"
	version2 := "V1-T1761856992374053"
	versionInfo1 := &DpuExtensionServiceVersionInfo{
		Version:        version1,
		Data:           "test-data-v1",
		HasCredentials: true,
		Created:        db.GetCurTime(),
	}
	versionInfo2 := &DpuExtensionServiceVersionInfo{
		Version:        version2,
		Data:           "test-data-v2",
		HasCredentials: false,
		Created:        db.GetCurTime(),
	}
	description1 := "Test DPU extension service 1"
	description2 := "Test DPU extension service 2"

	var createInputs []DpuExtensionServiceCreateInput
	{
		// DpuExtensionService set 1
		createInputs = append(createInputs, DpuExtensionServiceCreateInput{
			DpuExtensionServiceID: cutil.GetPtr(uuid.New()),
			Name:                  "dpu-service-1",
			Description:           &description1,
			ServiceType:           DpuExtensionServiceServiceTypeKubernetesPod,
			SiteID:                site.ID,
			TenantID:              tenant.ID,
			Version:               &version1,
			VersionInfo:           versionInfo1,
			ActiveVersions:        []string{version1},
			Status:                DpuExtensionServiceStatusPending,
			CreatedBy:             user.ID,
		})

		// DpuExtensionService set 2
		createInputs = append(createInputs, DpuExtensionServiceCreateInput{
			DpuExtensionServiceID: cutil.GetPtr(uuid.New()),
			Name:                  "dpu-service-2",
			Description:           &description2,
			ServiceType:           DpuExtensionServiceServiceTypeKubernetesPod,
			SiteID:                site.ID,
			TenantID:              tenant.ID,
			Version:               &version2,
			VersionInfo:           versionInfo2,
			ActiveVersions:        []string{version1, version2},
			Status:                DpuExtensionServiceStatusReady,
			CreatedBy:             user.ID,
		})

		// DpuExtensionService set 3
		createInputs = append(createInputs, DpuExtensionServiceCreateInput{
			DpuExtensionServiceID: cutil.GetPtr(uuid.New()),
			Name:                  "dpu-service-3",
			Description:           nil,
			ServiceType:           DpuExtensionServiceServiceTypeKubernetesPod,
			SiteID:                site.ID,
			TenantID:              tenant.ID,
			Version:               nil,
			VersionInfo:           nil,
			ActiveVersions:        []string{},
			Status:                DpuExtensionServiceStatusError,
			CreatedBy:             user.ID,
		})
	}

	dessd := NewDpuExtensionServiceDAO(dbSession)

	// DpuExtensionService created
	for _, input := range createInputs {
		desCre, err := dessd.Create(ctx, nil, input)
		assert.NoError(t, err)
		assert.NotNil(t, desCre)
		created = append(created, *desCre)
	}

	return
}

func TestDpuExtensionServiceSQLDAO_GetByID(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	dessExp := createSampleDpuExtensionServices(ctx, t, dbSession)
	dessd := NewDpuExtensionServiceDAO(dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		des                DpuExtensionService
		expectError        bool
		expectedErrVal     error
		verifyChildSpanner bool
	}{
		{
			desc:               "GetById success when DpuExtensionService exists on [0]",
			des:                dessExp[0],
			expectError:        false,
			verifyChildSpanner: true,
		},
		{
			desc:        "GetById success when DpuExtensionService exists on [1]",
			des:         dessExp[1],
			expectError: false,
		},
		{
			desc: "GetById success when DpuExtensionService not found",
			des: DpuExtensionService{
				ID: uuid.New(),
			},
			expectError:    true,
			expectedErrVal: db.ErrDoesNotExist,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tmp, err := dessd.GetByID(ctx, nil, tc.des.ID, nil)
			assert.Equal(t, tc.expectError, err != nil)
			if tc.expectError {
				assert.Equal(t, tc.expectedErrVal, err)
			}
			if err == nil {
				assert.Equal(t, tc.des.ID, tmp.ID)
				assert.Equal(t, tc.des.Name, tmp.Name)
				assert.Equal(t, tc.des.Description, tmp.Description)
				assert.Equal(t, tc.des.ServiceType, tmp.ServiceType)
				assert.Equal(t, tc.des.Status, tmp.Status)
			} else {
				t.Log(err.Error())
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDpuExtensionServiceSQLDAO_GetByID_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	created := createSampleDpuExtensionServices(ctx, t, dbSession)
	req := NewDpuExtensionServiceDAO(dbSession)

	got, err := req.GetByID(ctx, nil, created[0].ID, []string{SiteRelationName, TenantRelationName})
	assert.NoError(t, err)
	assert.NotNil(t, got)
	assert.NotNil(t, got.Site)
	assert.NotNil(t, got.Tenant)
}

func TestDpuExtensionServiceSQLDAO_GetAll(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	dessd := NewDpuExtensionServiceDAO(dbSession)

	// Create test data
	created := createSampleDpuExtensionServices(ctx, t, dbSession)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc                     string
		filter                   DpuExtensionServiceFilterInput
		pageInput                paginator.PageInput
		expectedCount            int
		expectedFirstOrderByName string
		expectedTotal            *int
		expectedError            bool
		verifyChildSpanner       bool
	}{
		{
			desc:               "GetAll with no filters returns all objects",
			expectedCount:      3,
			expectedTotal:      cutil.GetPtr(3),
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "GetAll with Name filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				Names: []string{created[0].Name},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with ServiceType filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				ServiceTypes: []string{DpuExtensionServiceServiceTypeKubernetesPod},
			},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc: "GetAll with Status filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				Statuses: []string{DpuExtensionServiceStatusPending, DpuExtensionServiceStatusReady},
			},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc: "GetAll with SiteID filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				SiteIDs: []uuid.UUID{created[0].SiteID},
			},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc: "GetAll with TenantID filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				TenantIDs: []uuid.UUID{created[0].TenantID},
			},
			expectedCount: 3,
			expectedError: false,
		},
		{
			desc: "GetAll with Version filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				Versions: []string{created[0].VersionInfo.Version},
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with search query filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				SearchQuery: cutil.GetPtr("dpu-service-1"),
			},
			expectedCount: 1,
			expectedError: false,
		},
		{
			desc: "GetAll with DpuExtensionServiceIDs filter returns objects",
			filter: DpuExtensionServiceFilterInput{
				DpuExtensionServiceIDs: []uuid.UUID{created[0].ID, created[2].ID},
			},
			expectedCount: 2,
			expectedError: false,
		},
		{
			desc: "GetAll with limit returns objects",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(0),
				Limit:  cutil.GetPtr(2),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with offset returns objects",
			pageInput: paginator.PageInput{
				Offset: cutil.GetPtr(1),
			},
			expectedCount: 2,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with order by returns objects",
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "created",
					Order: paginator.OrderDescending,
				},
			},
			expectedCount: 3,
			expectedTotal: cutil.GetPtr(3),
			expectedError: false,
		},
		{
			desc: "GetAll with order by name returns objects",
			pageInput: paginator.PageInput{
				OrderBy: &paginator.OrderBy{
					Field: "name",
					Order: paginator.OrderDescending,
				},
			},
			expectedCount:            3,
			expectedTotal:            cutil.GetPtr(3),
			expectedError:            false,
			expectedFirstOrderByName: "dpu-service-3",
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, total, err := dessd.GetAll(ctx, nil, tc.filter, tc.pageInput, nil)
			if err != nil {
				t.Log(err.Error())
			}

			assert.Equal(t, tc.expectedError, err != nil)
			if tc.expectedError {
				assert.Equal(t, nil, got)
			} else {
				assert.Equal(t, tc.expectedCount, len(got))
			}

			if tc.expectedTotal != nil {
				assert.Equal(t, *tc.expectedTotal, total)
			}

			if tc.expectedFirstOrderByName != "" {
				assert.Equal(t, tc.expectedFirstOrderByName, got[0].Name)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDpuExtensionServiceSQLDAO_GetAll_includeRelations(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	req := NewDpuExtensionServiceDAO(dbSession)
	_ = createSampleDpuExtensionServices(ctx, t, dbSession)

	got, _, err := req.GetAll(ctx, nil, DpuExtensionServiceFilterInput{}, paginator.PageInput{}, []string{SiteRelationName, TenantRelationName})
	assert.NoError(t, err)
	assert.Equal(t, 3, len(got))

	for _, des := range got {
		assert.NotNil(t, des.Site)
		assert.NotNil(t, des.Tenant)
	}
}

func TestDpuExtensionServiceSQLDAO_Update(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	dessExp := createSampleDpuExtensionServices(ctx, t, dbSession)
	dessd := NewDpuExtensionServiceDAO(dbSession)
	assert.NotNil(t, dessd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	newVersion := "V1-T1761856992374053"
	newObsName := "updated-metrics"
	newVersionInfo := &DpuExtensionServiceVersionInfo{
		Version:        newVersion,
		Data:           "updated-data",
		HasCredentials: true,
		Created:        db.GetCurTime(),
		Observability: &DpuExtensionServiceObservability{
			DpuExtensionServiceObservability: &corev1.DpuExtensionServiceObservability{
				Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
					{
						Name: &newObsName,
						Config: &corev1.DpuExtensionServiceObservabilityConfig_Logging{
							Logging: &corev1.DpuExtensionServiceObservabilityConfigLogging{
								Path: "/var/log/service.log",
							},
						},
					},
				},
			},
		},
	}
	updatedDescription := "Updated description"

	tests := []struct {
		desc               string
		input              DpuExtensionServiceUpdateInput
		expectedError      bool
		verifyChildSpanner bool
	}{
		{
			desc: "Update name",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[0].ID,
				Name:                  cutil.GetPtr("updated-service-name"),
			},
			expectedError:      false,
			verifyChildSpanner: true,
		},
		{
			desc: "Update description",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[1].ID,
				Description:           &updatedDescription,
			},
			expectedError: false,
		},
		{
			desc: "Update version",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[2].ID,
				Version:               &newVersion,
			},
			expectedError: false,
		},
		{
			desc: "Update version info",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[0].ID,
				VersionInfo:           newVersionInfo,
			},
			expectedError: false,
		},
		{
			desc: "Update active versions",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[1].ID,
				ActiveVersions:        []string{dessExp[1].VersionInfo.Version, "V1-T1761856992374053", "V1-T1761856992374054"},
			},
			expectedError: false,
		},
		{
			desc: "Update status",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[0].ID,
				Status:                cutil.GetPtr(DpuExtensionServiceStatusReady),
			},
			expectedError: false,
		},
		{
			desc: "Update is missing on site",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[0].ID,
				IsMissingOnSite:       cutil.GetPtr(true),
			},
			expectedError: false,
		},
		{
			desc: "Update multiple fields",
			input: DpuExtensionServiceUpdateInput{
				DpuExtensionServiceID: dessExp[2].ID,
				Name:                  cutil.GetPtr("multi-update-service"),
				Status:                cutil.GetPtr(DpuExtensionServiceStatusReady),
				Version:               cutil.GetPtr("V1-T1761856992374055"),
				VersionInfo: &DpuExtensionServiceVersionInfo{
					Version:        "V1-T1761856992374055",
					Data:           "updated-data",
					HasCredentials: true,
					Created:        db.GetCurTime(),
				},
				ActiveVersions:  []string{"V1-T1761856992374055"},
				IsMissingOnSite: cutil.GetPtr(true),
			},
			expectedError: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got, err := dessd.Update(ctx, nil, tc.input)
			assert.Equal(t, tc.expectedError, err != nil)
			if err != nil {
				t.Log(err.Error())
			}
			if !tc.expectedError {
				assert.Nil(t, err)
				assert.NotNil(t, got)
				if tc.input.Name != nil {
					assert.Equal(t, *tc.input.Name, got.Name)
				}
				if tc.input.Description != nil {
					assert.Equal(t, *tc.input.Description, *got.Description)
				}
				if tc.input.Version != nil {
					assert.Equal(t, *tc.input.Version, *got.Version)
				}
				if tc.input.VersionInfo != nil {
					assert.Equal(t, tc.input.VersionInfo.Version, got.VersionInfo.Version)
					assert.Equal(t, tc.input.VersionInfo.Data, got.VersionInfo.Data)
					assert.Equal(t, tc.input.VersionInfo.Observability, got.VersionInfo.Observability)
				}
				if tc.input.ActiveVersions != nil {
					assert.Equal(t, tc.input.ActiveVersions, got.ActiveVersions)
				}
				if tc.input.Status != nil {
					assert.Equal(t, *tc.input.Status, got.Status)
				}
				if tc.input.IsMissingOnSite != nil {
					assert.Equal(t, *tc.input.IsMissingOnSite, got.IsMissingOnSite)
				}

				if tc.verifyChildSpanner {
					span := otrace.SpanFromContext(ctx)
					assert.True(t, span.SpanContext().IsValid())
					_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
					assert.True(t, ok)
				}
			}
		})
	}
}

func TestDpuExtensionServiceSQLDAO_Clear(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	dessExp := createSampleDpuExtensionServices(ctx, t, dbSession)
	dessd := NewDpuExtensionServiceDAO(dbSession)
	assert.NotNil(t, dessd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		des                DpuExtensionService
		input              DpuExtensionServiceClearInput
		expectedUpdate     bool
		verifyChildSpanner bool
	}{
		{
			desc: "can clear Description",
			des:  dessExp[0],
			input: DpuExtensionServiceClearInput{
				Description: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear Version",
			des:  dessExp[1],
			input: DpuExtensionServiceClearInput{
				Version: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear VersionInfo",
			des:  dessExp[1],
			input: DpuExtensionServiceClearInput{
				VersionInfo: true,
			},
			expectedUpdate: true,
		},
		{
			desc: "can clear multiple fields",
			des:  dessExp[0],
			input: DpuExtensionServiceClearInput{
				Description: true,
				Version:     true,
				VersionInfo: true,
			},
			expectedUpdate: true,
		},
		{
			desc:           "nop when no cleared fields are specified",
			des:            dessExp[2],
			input:          DpuExtensionServiceClearInput{},
			expectedUpdate: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			tc.input.DpuExtensionServiceID = tc.des.ID
			tmp, err := dessd.Clear(ctx, nil, tc.input)
			assert.Nil(t, err)
			assert.NotNil(t, tmp)
			if tc.input.Description {
				assert.Nil(t, tmp.Description)
			}
			if tc.input.Version {
				assert.Nil(t, tmp.Version)
			}
			if tc.input.VersionInfo {
				assert.Nil(t, tmp.VersionInfo)
			}

			if tc.expectedUpdate {
				assert.True(t, tmp.Updated.After(tc.des.Updated))
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDpuExtensionServiceSQLDAO_Delete(t *testing.T) {
	ctx := context.Background()
	dbSession := testInitDB(t)
	defer dbSession.Close()
	testDpuExtensionServiceSetupSchema(t, dbSession)

	dessExp := createSampleDpuExtensionServices(ctx, t, dbSession)
	dessd := NewDpuExtensionServiceDAO(dbSession)
	assert.NotNil(t, dessd)

	// OTEL Spanner configuration
	_, _, ctx = testCommonTraceProviderSetup(t, ctx)

	tests := []struct {
		desc               string
		desID              uuid.UUID
		checkSoftDelete    bool
		wantErr            bool
		verifyChildSpanner bool
	}{
		{
			desc:               "can delete existing object success",
			desID:              dessExp[1].ID,
			checkSoftDelete:    true,
			wantErr:            false,
			verifyChildSpanner: true,
		},
		{
			desc:    "delete non-existent object success",
			desID:   uuid.New(),
			wantErr: false,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := dessd.Delete(ctx, nil, tc.desID)

			if tc.wantErr {
				assert.Error(t, err)
				return
			}

			var res DpuExtensionService

			err = dbSession.DB.NewSelect().Model(&res).Where("des.id = ?", tc.desID).Scan(ctx)
			assert.ErrorIs(t, err, sql.ErrNoRows)

			if tc.checkSoftDelete {
				err = dbSession.DB.NewSelect().Model(&res).Where("des.id = ?", tc.desID).WhereAllWithDeleted().Scan(ctx)
				assert.NoError(t, err)
				assert.NotNil(t, res.Deleted)
			}

			if tc.verifyChildSpanner {
				span := otrace.SpanFromContext(ctx)
				assert.True(t, span.SpanContext().IsValid())
				_, ok := ctx.Value(stracer.TracerKey).(otrace.Tracer)
				assert.True(t, ok)
			}
		})
	}
}

func TestDpuExtensionService_ToDeletionRequestProto(t *testing.T) {
	id := uuid.New()
	des := &DpuExtensionService{ID: id}
	req := des.ToDeletionRequestProto()
	assert.NotNil(t, req)
	assert.Equal(t, id.String(), req.ServiceId)
	assert.Empty(t, req.Versions)
}

func TestDpuExtensionService_ToVersionDeletionRequestProto(t *testing.T) {
	id := uuid.New()
	des := &DpuExtensionService{ID: id}
	req := des.ToVersionDeletionRequestProto("v1.2.3")
	assert.NotNil(t, req)
	assert.Equal(t, id.String(), req.ServiceId)
	assert.Equal(t, []string{"v1.2.3"}, req.Versions)
}
