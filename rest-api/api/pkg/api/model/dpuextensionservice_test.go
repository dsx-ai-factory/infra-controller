// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

func TestAPIDpuExtensionServiceCreateRequest_Validate(t *testing.T) {
	validUUID := uuid.New().String()

	tests := []struct {
		desc      string
		obj       APIDpuExtensionServiceCreateRequest
		expectErr bool
	}{
		{
			desc: "ok when only required fields are provided",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: false,
		},
		{
			desc: "ok when all fields are provided",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				Description: cutil.GetPtr("test description"),
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: false,
		},
		{
			desc: "ok when credentials are provided",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "https://registry.hub.docker.com",
					Username:    cutil.GetPtr("testuser"),
					Password:    cutil.GetPtr("testpass"),
				},
			},
			expectErr: false,
		},
		{
			desc: "ok when observability is provided",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "busybox:9090",
							},
						},
					},
				},
			},
			expectErr: false,
		},
		{
			desc: "ok when observability is provided as an empty config list",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{},
				},
			},
			expectErr: false,
		},
		{
			desc: "error when name is missing",
			obj: APIDpuExtensionServiceCreateRequest{
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when name is too short",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "t",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when name is too long",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        strings.Repeat("a", 257),
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when name has invalid characters",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        " test_service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when serviceType is missing",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:   "test-service",
				SiteID: validUUID,
				Data:   "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when serviceType is invalid",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: "InvalidType",
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when siteId is missing",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when siteId is not a valid UUID",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      "invalid-uuid",
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
			},
			expectErr: true,
		},
		{
			desc: "error when data is missing",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
			},
			expectErr: true,
		},
		{
			desc: "error when credentials are invalid - missing username",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "https://registry.hub.docker.com",
					Password:    cutil.GetPtr("testpass"),
				},
			},
			expectErr: true,
		},
		{
			desc: "error when credentials are invalid - missing password",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "https://registry.hub.docker.com",
					Username:    cutil.GetPtr("testuser"),
				},
			},
			expectErr: true,
		},
		{
			desc: "error when credentials have invalid registry URL",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "not-a-valid-url",
					Username:    cutil.GetPtr("testuser"),
					Password:    cutil.GetPtr("testpass"),
				},
			},
			expectErr: true,
		},
		{
			desc: "ok when observability configs contain different oneof variants across entries",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "busybox:9090",
							},
						},
						{
							Logging: &APIDpuExtensionServiceObservabilityConfigLogging{
								Path: "/var/log/busybox.log",
							},
						},
					},
				},
			},
			expectErr: false,
		},
		{
			desc: "error when a single observability config contains both prometheus and logging",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "busybox:9090",
							},
							Logging: &APIDpuExtensionServiceObservabilityConfigLogging{
								Path: "/var/log/busybox.log",
							},
						},
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability contains too many configs",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: func() []APIDpuExtensionServiceObservabilityConfig {
						configs := make([]APIDpuExtensionServiceObservabilityConfig, DpuExtensionServiceMaxObservabilityConfigs+1)
						for i := range configs {
							configs[i] = APIDpuExtensionServiceObservabilityConfig{
								Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
									ScrapeIntervalSeconds: 30,
									Endpoint:              "busybox:9090",
								},
							}
						}
						return configs
					}(),
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability config name is too long",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Name: cutil.GetPtr(strings.Repeat("a", DpuExtensionServiceMaxObservabilityConfigNameLength+1)),
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "busybox:9090",
							},
						},
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability config name is empty",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Name: cutil.GetPtr(""),
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "busybox:9090",
							},
						},
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability config name is whitespace only",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Name: cutil.GetPtr("   "),
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "busybox:9090",
							},
						},
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability prometheus scrapeIntervalSeconds is zero",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 0,
								Endpoint:              "busybox:9090",
							},
						},
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability endpoint contains invalid characters",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Prometheus: &APIDpuExtensionServiceObservabilityConfigPrometheus{
								ScrapeIntervalSeconds: 30,
								Endpoint:              "http://busybox:9090/metrics",
							},
						},
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability path contains invalid characters",
			obj: APIDpuExtensionServiceCreateRequest{
				Name:        "test-service",
				ServiceType: DpuExtensionServiceTypeKubernetesPod,
				SiteID:      validUUID,
				Data:        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: test",
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Logging: &APIDpuExtensionServiceObservabilityConfigLogging{
								Path: "/var/log/busybox$.log",
							},
						},
					},
				},
			},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPIDpuExtensionServiceUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIDpuExtensionServiceUpdateRequest
		expectErr bool
	}{
		{
			desc: "ok when name is updated",
			obj: APIDpuExtensionServiceUpdateRequest{
				Name: cutil.GetPtr("updated-name"),
			},
			expectErr: false,
		},
		{
			desc: "ok when description is updated",
			obj: APIDpuExtensionServiceUpdateRequest{
				Description: cutil.GetPtr("updated description"),
			},
			expectErr: false,
		},
		{
			desc: "ok when data is updated",
			obj: APIDpuExtensionServiceUpdateRequest{
				Data: cutil.GetPtr("apiVersion: v1\nkind: Pod\nmetadata:\n  name: updated"),
			},
			expectErr: false,
		},
		{
			desc: "ok when all fields are updated",
			obj: APIDpuExtensionServiceUpdateRequest{
				Name:        cutil.GetPtr("updated-name"),
				Description: cutil.GetPtr("updated description"),
				Data:        cutil.GetPtr("apiVersion: v1\nkind: Pod\nmetadata:\n  name: updated"),
			},
			expectErr: false,
		},
		{
			desc: "ok when credentials are updated",
			obj: APIDpuExtensionServiceUpdateRequest{
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "https://registry.hub.docker.com",
					Username:    cutil.GetPtr("newuser"),
					Password:    cutil.GetPtr("newpass"),
				},
			},
			expectErr: false,
		},
		{
			desc: "ok when observability is updated",
			obj: APIDpuExtensionServiceUpdateRequest{
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{
						{
							Logging: &APIDpuExtensionServiceObservabilityConfigLogging{
								Path: "/var/log/busybox.log",
							},
						},
					},
				},
			},
			expectErr: false,
		},
		{
			desc: "ok when observability is updated to an empty config list",
			obj: APIDpuExtensionServiceUpdateRequest{
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{},
				},
			},
			expectErr: false,
		},
		{
			desc: "error when name is empty string",
			obj: APIDpuExtensionServiceUpdateRequest{
				Name: cutil.GetPtr(""),
			},
			expectErr: true,
		},
		{
			desc: "error when name is too short",
			obj: APIDpuExtensionServiceUpdateRequest{
				Name: cutil.GetPtr("t"),
			},
			expectErr: true,
		},
		{
			desc: "error when name is too long",
			obj: APIDpuExtensionServiceUpdateRequest{
				Name: cutil.GetPtr(strings.Repeat("a", 257)),
			},
			expectErr: true,
		},
		{
			desc: "error when name has invalid characters",
			obj: APIDpuExtensionServiceUpdateRequest{
				Name: cutil.GetPtr(" test_service"),
			},
			expectErr: true,
		},
		{
			desc: "error when credentials are invalid - missing username",
			obj: APIDpuExtensionServiceUpdateRequest{
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "https://registry.hub.docker.com",
					Password:    cutil.GetPtr("testpass"),
				},
			},
			expectErr: true,
		},
		{
			desc: "error when credentials are invalid - missing password",
			obj: APIDpuExtensionServiceUpdateRequest{
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "https://registry.hub.docker.com",
					Username:    cutil.GetPtr("testuser"),
				},
			},
			expectErr: true,
		},
		{
			desc: "error when credentials have invalid registry URL",
			obj: APIDpuExtensionServiceUpdateRequest{
				Credentials: &APIDpuExtensionServiceCredentials{
					RegistryURL: "not-a-valid-url",
					Username:    cutil.GetPtr("testuser"),
					Password:    cutil.GetPtr("testpass"),
				},
			},
			expectErr: true,
		},
		{
			desc: "error when observability is missing config type",
			obj: APIDpuExtensionServiceUpdateRequest{
				Observability: &APIDpuExtensionServiceObservability{
					Configs: []APIDpuExtensionServiceObservabilityConfig{{}},
				},
			},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestAPIDpuExtensionServiceCredentials_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APIDpuExtensionServiceCredentials
		expectErr bool
	}{
		{
			desc: "ok when all fields are provided with valid values",
			obj: APIDpuExtensionServiceCredentials{
				RegistryURL: "https://registry.hub.docker.com",
				Username:    cutil.GetPtr("testuser"),
				Password:    cutil.GetPtr("testpass"),
			},
			expectErr: false,
		},
		{
			desc: "ok with different valid registry URL",
			obj: APIDpuExtensionServiceCredentials{
				RegistryURL: "https://nvcr.io",
				Username:    cutil.GetPtr("$oauthtoken"),
				Password:    cutil.GetPtr("secret-token"),
			},
			expectErr: false,
		},
		{
			desc: "error when registry URL is missing",
			obj: APIDpuExtensionServiceCredentials{
				Username: cutil.GetPtr("testuser"),
				Password: cutil.GetPtr("testpass"),
			},
			expectErr: true,
		},
		{
			desc: "error when registry URL is invalid",
			obj: APIDpuExtensionServiceCredentials{
				RegistryURL: "not-a-valid-url",
				Username:    cutil.GetPtr("testuser"),
				Password:    cutil.GetPtr("testpass"),
			},
			expectErr: true,
		},
		{
			desc: "error when username is missing",
			obj: APIDpuExtensionServiceCredentials{
				RegistryURL: "https://registry.hub.docker.com",
				Password:    cutil.GetPtr("testpass"),
			},
			expectErr: true,
		},
		{
			desc: "error when password is missing",
			obj: APIDpuExtensionServiceCredentials{
				RegistryURL: "https://registry.hub.docker.com",
				Username:    cutil.GetPtr("testuser"),
			},
			expectErr: true,
		},
		{
			desc: "error when both username and password are missing",
			obj: APIDpuExtensionServiceCredentials{
				RegistryURL: "https://registry.hub.docker.com",
			},
			expectErr: true,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
			if err != nil {
				fmt.Println(err.Error())
			}
		})
	}
}

func TestNewAPIDpuExtensionService(t *testing.T) {
	obsName := "busybox-metrics"
	dbdes := &cdbm.DpuExtensionService{
		ID:          uuid.New(),
		Name:        "test-service",
		ServiceType: "test-type",
		Version:     cutil.GetPtr("v1"),
		VersionInfo: &cdbm.DpuExtensionServiceVersionInfo{
			Version:        "v1",
			Data:           "apiVersion: v1\nkind: Pod",
			HasCredentials: true,
			Created:        time.Now(),
			Observability: &cdbm.DpuExtensionServiceObservability{
				DpuExtensionServiceObservability: &corev1.DpuExtensionServiceObservability{
					Configs: []*corev1.DpuExtensionServiceObservabilityConfig{
						{
							Name: &obsName,
							Config: &corev1.DpuExtensionServiceObservabilityConfig_Prometheus{
								Prometheus: &corev1.DpuExtensionServiceObservabilityConfigPrometheus{
									ScrapeIntervalSeconds: 30,
									Endpoint:              "busybox:9090",
								},
							},
						},
					},
				},
			},
		},
		Status:  cdbm.DpuExtensionServiceStatusReady,
		Created: time.Now(),
		Updated: time.Now(),
	}

	dbdesds := []cdbm.StatusDetail{
		{
			ID:       uuid.New(),
			EntityID: dbdes.ID.String(),
			Status:   dbdes.Status,
			Created:  time.Now(),
			Updated:  time.Now(),
		},
	}

	tests := []struct {
		desc    string
		dbdes   *cdbm.DpuExtensionService
		dbdesds []cdbm.StatusDetail
	}{
		{
			desc:    "ok when all required fields are provided",
			dbdes:   dbdes,
			dbdesds: dbdesds,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			ades := NewAPIDpuExtensionService(tc.dbdes, tc.dbdesds)
			assert.Equal(t, tc.dbdes.ID.String(), ades.ID)
			assert.Equal(t, tc.dbdes.Name, ades.Name)
			assert.Equal(t, tc.dbdes.ServiceType, ades.ServiceType)
			assert.Equal(t, tc.dbdes.SiteID.String(), ades.SiteID)
			assert.Equal(t, tc.dbdes.TenantID.String(), ades.TenantID)
			assert.Equal(t, tc.dbdes.Version, ades.Version)
			assert.Equal(t, tc.dbdes.VersionInfo.Version, ades.VersionInfo.Version)
			assert.Equal(t, tc.dbdes.VersionInfo.Data, ades.VersionInfo.Data)
			assert.Equal(t, tc.dbdes.VersionInfo.HasCredentials, ades.VersionInfo.HasCredentials)
			assert.Equal(t, tc.dbdes.VersionInfo.Created, ades.VersionInfo.Created)
			assert.Equal(t, tc.dbdes.VersionInfo.Observability.GetConfigs()[0].GetPrometheus().Endpoint, ades.VersionInfo.Observability.Configs[0].Prometheus.Endpoint)
			assert.Equal(t, tc.dbdes.ActiveVersions, ades.ActiveVersions)
			assert.Equal(t, tc.dbdes.Status, ades.Status)
			assert.Equal(t, tc.dbdes.Created, ades.Created)
			assert.Equal(t, tc.dbdes.Updated, ades.Updated)
			assert.Equal(t, len(tc.dbdesds), len(ades.StatusHistory))
		})
	}
}

func TestAPIDpuExtensionServiceCredentials_ToProto(t *testing.T) {
	t.Run("nil receiver yields nil", func(t *testing.T) {
		var c *APIDpuExtensionServiceCredentials
		assert.Nil(t, c.ToProto())
	})
	t.Run("populates UsernamePassword type", func(t *testing.T) {
		user := "u"
		pass := "p"
		c := &APIDpuExtensionServiceCredentials{RegistryURL: "https://reg/", Username: &user, Password: &pass}
		got := c.ToProto()
		require.NotNil(t, got)
		assert.Equal(t, "https://reg/", got.RegistryUrl)
		up, ok := got.Type.(*corev1.DpuExtensionServiceCredential_UsernamePassword)
		require.True(t, ok)
		require.NotNil(t, up.UsernamePassword)
		assert.Equal(t, "u", up.UsernamePassword.Username)
		assert.Equal(t, "p", up.UsernamePassword.Password)
	})
	t.Run("nil username/password yields no Type", func(t *testing.T) {
		c := &APIDpuExtensionServiceCredentials{RegistryURL: "https://reg/"}
		got := c.ToProto()
		require.NotNil(t, got)
		assert.Equal(t, "https://reg/", got.RegistryUrl)
		assert.Nil(t, got.Type)
	})
}

func TestAPIDpuExtensionServiceCreateRequest_ToProto(t *testing.T) {
	t.Run("populated request maps fields through", func(t *testing.T) {
		desc := "primary"
		user := "u"
		pass := "p"
		descr := APIDpuExtensionServiceCreateRequest{
			Name:        "svc-a",
			Description: &desc,
			ServiceType: DpuExtensionServiceTypeKubernetesPod,
			SiteID:      uuid.NewString(),
			Data:        "kind: Pod",
			Credentials: &APIDpuExtensionServiceCredentials{
				RegistryURL: "https://reg/",
				Username:    &user,
				Password:    &pass,
			},
		}
		req := descr.ToProto("svc-id-1", "org-1")
		assert.NotNil(t, req)
		assert.Equal(t, "svc-id-1", *req.ServiceId)
		assert.Equal(t, "svc-a", req.ServiceName)
		assert.Equal(t, &desc, req.Description)
		assert.Equal(t, "org-1", req.TenantOrganizationId)
		assert.Equal(t, "kind: Pod", req.Data)
		assert.Equal(t, corev1.DpuExtensionServiceType_KUBERNETES_POD, req.ServiceType)
		assert.NotNil(t, req.Credential)
	})
	t.Run("nil Credentials and Observability yield nil proto fields", func(t *testing.T) {
		// Confirms the nested ToProto calls' nil-receiver behavior is
		// what keeps the request mapper clean.
		descr := APIDpuExtensionServiceCreateRequest{
			Name:        "svc-c",
			ServiceType: DpuExtensionServiceTypeKubernetesPod,
			Data:        "kind: Pod",
		}
		req := descr.ToProto("svc-id-3", "org-1")
		assert.NotNil(t, req)
		assert.Nil(t, req.Credential)
		assert.Nil(t, req.Observability)
	})
}

func TestAPIDpuExtensionServiceUpdateRequest_ToProto(t *testing.T) {
	t.Run("populated request maps fields through", func(t *testing.T) {
		name := "svc-b"
		data := "kind: Pod V2"
		desur := APIDpuExtensionServiceUpdateRequest{
			Name: &name,
			Data: &data,
		}
		req := desur.ToProto("svc-id-2")
		assert.NotNil(t, req)
		assert.Equal(t, "svc-id-2", req.ServiceId)
		assert.Equal(t, &name, req.ServiceName)
		assert.Equal(t, "kind: Pod V2", req.Data)
		assert.Nil(t, req.Credential)
		assert.Nil(t, req.Observability)
	})
	t.Run("nil Data leaves proto Data at the zero value", func(t *testing.T) {
		// Update's Data is *string on the request, but a required
		// string on the wire; nil request Data maps to "".
		desur := APIDpuExtensionServiceUpdateRequest{}
		req := desur.ToProto("svc-id-4")
		assert.NotNil(t, req)
		assert.Equal(t, "", req.Data)
		assert.Nil(t, req.Credential)
		assert.Nil(t, req.Observability)
	})
}
