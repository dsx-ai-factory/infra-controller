// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
	"google.golang.org/protobuf/encoding/protojson"
)

func TestAPIExpectedPowerShelfCreateRequest_Validate(t *testing.T) {
	emptyString := ""
	validShelfSerial := "SHELF123"
	validUsername := "admin"
	validPassword := "password123"
	validBmcIpAddress := "192.168.1.100"

	tests := []struct {
		desc      string
		obj       APIExpectedPowerShelfCreateRequest
		expectErr bool
	}{
		{
			desc: "ok when all required fields are provided",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: false,
		},
		{
			desc: "ok when required fields and optional fields are provided",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
				BmcIpAddress:       &validBmcIpAddress,
				Labels:             map[string]string{"env": "test", "zone": "us-west-1"},
			},
			expectErr: false,
		},
		{
			desc: "error when BmcMacAddress is missing",
			obj: APIExpectedPowerShelfCreateRequest{
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when ShelfSerialNumber is missing",
			obj: APIExpectedPowerShelfCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
			},
			expectErr: true,
		},
		{
			desc: "error when BmcMacAddress is wrong length (too short)",
			obj: APIExpectedPowerShelfCreateRequest{
				BmcMacAddress:      "00:11:22:33:44",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when ShelfSerialNumber is empty",
			obj: APIExpectedPowerShelfCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  emptyString,
			},
			expectErr: true,
		},
		// Boundary tests for BMC username (max 16 characters)
		{
			desc: "ok when BMC username is exactly 16 characters",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 16)),
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is 17 characters (over limit)",
			obj: APIExpectedPowerShelfCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 17)),
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: true,
		},
		// Boundary tests for BMC password (max 20 characters)
		{
			desc: "ok when BMC password is exactly 20 characters",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 20)),
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when BMC password is 21 characters (over limit)",
			obj: APIExpectedPowerShelfCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 21)),
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: true,
		},
		// Boundary tests for shelf serial number (max 32 characters)
		{
			desc: "ok when shelf serial number is exactly 32 characters",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  strings.Repeat("a", 32),
			},
			expectErr: false,
		},
		{
			desc: "error when shelf serial number is 33 characters (over limit)",
			obj: APIExpectedPowerShelfCreateRequest{
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  strings.Repeat("a", 33),
			},
			expectErr: true,
		},
		{
			desc: "ok when optional fields are empty",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &emptyString,
				DefaultBmcPassword: &emptyString,
				ShelfSerialNumber:  validShelfSerial,
				Labels:             map[string]string{},
			},
			expectErr: false,
		},
		{
			desc: "error when SiteID is empty string",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: true,
		},
		{
			desc: "ok when SiteID is valid UUID",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: false,
		},
		{
			desc: "error when SiteID is invalid UUID",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "not-a-valid-uuid",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: true,
		},
		{
			desc: "error when SiteID is partially valid UUID",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
			},
			expectErr: true,
		},
		// BmcIpAddress validation tests
		{
			desc: "valid IPv4 BmcIpAddress",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
				BmcIpAddress:       cutil.GetPtr("192.168.1.10"),
			},
			expectErr: false,
		},
		{
			desc: "valid IPv6 BmcIpAddress",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
				BmcIpAddress:       cutil.GetPtr("2001:db8::1"),
			},
			expectErr: false,
		},
		{
			desc: "invalid BmcIpAddress",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
				BmcIpAddress:       cutil.GetPtr("not-an-ip"),
			},
			expectErr: true,
		},
		{
			desc: "empty BmcIpAddress (pointer set, value empty)",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
				BmcIpAddress:       &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "nil BmcIpAddress (default)",
			obj: APIExpectedPowerShelfCreateRequest{
				SiteID:             "550e8400-e29b-41d4-a716-446655440000",
				BmcMacAddress:      "00:11:22:33:44:55",
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				ShelfSerialNumber:  validShelfSerial,
				BmcIpAddress:       nil,
			},
			expectErr: false,
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

func TestNewAPIExpectedPowerShelf(t *testing.T) {
	bmcIpAddress := "192.168.1.100"
	dbEPS := &cdbm.ExpectedPowerShelf{
		BmcMacAddress:     "00:11:22:33:44:55",
		ShelfSerialNumber: "SHELF123",
		BmcIpAddress:      &bmcIpAddress,
		Labels:            map[string]string{"env": "test", "zone": "us-west-1"},
		Created:           cdb.GetCurTime(),
		Updated:           cdb.GetCurTime(),
	}

	tests := []struct {
		desc  string
		dbObj *cdbm.ExpectedPowerShelf
	}{
		{
			desc:  "test creating API ExpectedPowerShelf",
			dbObj: dbEPS,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPIExpectedPowerShelf(tc.dbObj)

			// Verify all fields are properly mapped
			// Note: BmcUsername and BmcPassword are not included as they're not stored in DB
			assert.Equal(t, tc.dbObj.BmcMacAddress, got.BmcMacAddress)
			assert.Equal(t, tc.dbObj.ShelfSerialNumber, got.ShelfSerialNumber)
			assert.Equal(t, tc.dbObj.BmcIpAddress, got.BmcIpAddress)
			assert.Equal(t, APILabels(tc.dbObj.Labels), got.Labels)
			assert.Equal(t, tc.dbObj.Created, got.Created)
			assert.Equal(t, tc.dbObj.Updated, got.Updated)
		})
	}
}

func TestNewAPIExpectedPowerShelfWithNilFields(t *testing.T) {
	dbEPS := &cdbm.ExpectedPowerShelf{
		BmcMacAddress:     "00:11:22:33:44:55",
		ShelfSerialNumber: "SHELF123",
		BmcIpAddress:      nil,
		Labels:            nil,
		Created:           time.Now(),
		Updated:           time.Now(),
	}

	got := NewAPIExpectedPowerShelf(dbEPS)

	// Verify fields are properly handled when empty or nil
	assert.Equal(t, dbEPS.BmcMacAddress, got.BmcMacAddress)
	assert.Equal(t, dbEPS.ShelfSerialNumber, got.ShelfSerialNumber)
	assert.Nil(t, got.BmcIpAddress)
	assert.Nil(t, got.Labels)
}

func TestAPIExpectedPowerShelfUpdateRequest_Validate(t *testing.T) {
	emptyString := ""
	validShelfSerial := "SHELF123"
	validUsername := "admin"
	validPassword := "password123"

	tests := []struct {
		desc      string
		obj       APIExpectedPowerShelfUpdateRequest
		expectErr bool
	}{
		{
			desc: "ok when all fields are provided",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &validShelfSerial,
				BmcIpAddress:      cutil.GetPtr("192.168.1.100"),
				Labels:            map[string]string{"env": "production", "zone": "us-east-1"},
			},
			expectErr: false,
		},
		{
			desc: "ok when only shelf serial and labels are provided",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &validShelfSerial,
				Labels:            map[string]string{"team": "devops"},
			},
			expectErr: false,
		},
		{
			desc: "ok when only labels are provided",
			obj: APIExpectedPowerShelfUpdateRequest{
				Labels: map[string]string{"team": "devops"},
			},
			expectErr: false,
		},
		{
			desc: "ok with nil values for all optional fields",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  nil,
				BmcIpAddress:       nil,
				DefaultBmcUsername: nil,
				DefaultBmcPassword: nil,
				Labels:             nil,
			},
			expectErr: false,
		},
		{
			desc: "error when shelf serial number is empty string",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &emptyString,
				Labels:            map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "ok with valid BMC credentials",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  &validShelfSerial,
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is empty string",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  &validShelfSerial,
				DefaultBmcUsername: &emptyString,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		{
			desc: "error when BMC password is empty string",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  &validShelfSerial,
				DefaultBmcPassword: &emptyString,
				DefaultBmcUsername: &validUsername,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for BMC username (max 16 characters)
		{
			desc: "ok when BMC username is exactly 16 characters",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  &validShelfSerial,
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 16)),
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC username is 17 characters (over limit)",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  &validShelfSerial,
				DefaultBmcUsername: cutil.GetPtr(strings.Repeat("a", 17)),
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for BMC password (max 20 characters)
		{
			desc: "ok when BMC password is exactly 20 characters",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  &validShelfSerial,
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 20)),
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when BMC password is 21 characters (over limit)",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  &validShelfSerial,
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: cutil.GetPtr(strings.Repeat("a", 21)),
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// Boundary tests for shelf serial number (max 32 characters)
		{
			desc: "ok when shelf serial number is exactly 32 characters",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  cutil.GetPtr(strings.Repeat("a", 32)),
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: false,
		},
		{
			desc: "error when shelf serial number is 33 characters (over limit)",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber:  cutil.GetPtr(strings.Repeat("a", 33)),
				DefaultBmcUsername: &validUsername,
				DefaultBmcPassword: &validPassword,
				Labels:             map[string]string{"env": "test"},
			},
			expectErr: true,
		},
		// BmcIpAddress validation tests
		{
			desc: "valid IPv4 BmcIpAddress",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &validShelfSerial,
				BmcIpAddress:      cutil.GetPtr("192.168.1.10"),
			},
			expectErr: false,
		},
		{
			desc: "valid IPv6 BmcIpAddress",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &validShelfSerial,
				BmcIpAddress:      cutil.GetPtr("2001:db8::1"),
			},
			expectErr: false,
		},
		{
			desc: "invalid BmcIpAddress",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &validShelfSerial,
				BmcIpAddress:      cutil.GetPtr("not-an-ip"),
			},
			expectErr: true,
		},
		{
			desc: "empty BmcIpAddress (pointer set, value empty)",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &validShelfSerial,
				BmcIpAddress:      &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "nil BmcIpAddress (default)",
			obj: APIExpectedPowerShelfUpdateRequest{
				ShelfSerialNumber: &validShelfSerial,
				BmcIpAddress:      nil,
			},
			expectErr: false,
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

func TestNewAPIExpectedPowerShelfEdgeCases(t *testing.T) {
	t.Run("with empty strings in fields", func(t *testing.T) {
		dbEPS := &cdbm.ExpectedPowerShelf{
			BmcMacAddress:     "",
			ShelfSerialNumber: "",
			BmcIpAddress:      cutil.GetPtr(""),
			Labels:            map[string]string{"": ""},
			Created:           time.Now(),
			Updated:           time.Now(),
		}

		got := NewAPIExpectedPowerShelf(dbEPS)
		assert.NotNil(t, got)
		assert.Equal(t, "", got.BmcMacAddress)
		assert.Equal(t, "", got.ShelfSerialNumber)
	})

	t.Run("with special characters in labels", func(t *testing.T) {
		dbEPS := &cdbm.ExpectedPowerShelf{
			BmcMacAddress:     "00:11:22:33:44:55",
			ShelfSerialNumber: "SHELF-123",
			Labels: map[string]string{
				"app.kubernetes.io/name":    "cloud-api",
				"app.kubernetes.io/version": "v1.2.3",
				"special-chars":             "value!@#$%^&*()",
			},
			Created: time.Now(),
			Updated: time.Now(),
		}

		got := NewAPIExpectedPowerShelf(dbEPS)
		assert.NotNil(t, got)
		assert.Equal(t, APILabels(dbEPS.Labels), got.Labels)
		assert.Equal(t, "cloud-api", got.Labels["app.kubernetes.io/name"])
	})

	t.Run("with unicode characters in labels", func(t *testing.T) {
		dbEPS := &cdbm.ExpectedPowerShelf{
			BmcMacAddress:     "00:11:22:33:44:55",
			ShelfSerialNumber: "SHELF123",
			Labels: map[string]string{
				"location": "東京",
				"owner":    "José García",
				"emoji":    "🚀🔥",
			},
			Created: time.Now(),
			Updated: time.Now(),
		}

		got := NewAPIExpectedPowerShelf(dbEPS)
		assert.NotNil(t, got)
		assert.Equal(t, "東京", got.Labels["location"])
		assert.Equal(t, "José García", got.Labels["owner"])
		assert.Equal(t, "🚀🔥", got.Labels["emoji"])
	})

	t.Run("with zero time values", func(t *testing.T) {
		dbEPS := &cdbm.ExpectedPowerShelf{
			BmcMacAddress:     "00:11:22:33:44:55",
			ShelfSerialNumber: "SHELF123",
			Created:           time.Time{},
			Updated:           time.Time{},
		}

		got := NewAPIExpectedPowerShelf(dbEPS)
		assert.NotNil(t, got)
		assert.True(t, got.Created.IsZero())
		assert.True(t, got.Updated.IsZero())
	})
}

func TestNewAPIExpectedPowerShelfWithSite(t *testing.T) {
	siteID := uuid.New()
	epsID := uuid.New()

	t.Run("maps Site when provided", func(t *testing.T) {
		site := &cdbm.Site{
			ID:                       siteID,
			Name:                     "Test Site",
			Org:                      "test-org",
			InfrastructureProviderID: uuid.New(),
			IsSerialConsoleEnabled:   false,
			Status:                   "active",
			Created:                  time.Now(),
			Updated:                  time.Now(),
		}

		dbEPS := &cdbm.ExpectedPowerShelf{
			ID:                epsID,
			SiteID:            siteID,
			BmcMacAddress:     "00:11:22:33:44:55",
			ShelfSerialNumber: "SHELF123",
			BmcIpAddress:      cutil.GetPtr("192.168.1.100"),
			Labels:            map[string]string{},
			Site:              site,
			Created:           time.Now(),
			Updated:           time.Now(),
		}

		apiEPS := NewAPIExpectedPowerShelf(dbEPS)
		assert.NotNil(t, apiEPS)
		assert.NotNil(t, apiEPS.Site)
		assert.Equal(t, siteID.String(), apiEPS.Site.ID)
		assert.Equal(t, "Test Site", apiEPS.Site.Name)
		assert.Equal(t, "test-org", apiEPS.Site.Org)
	})

	t.Run("handles nil Site gracefully", func(t *testing.T) {
		dbEPS := &cdbm.ExpectedPowerShelf{
			ID:                epsID,
			SiteID:            siteID,
			BmcMacAddress:     "00:11:22:33:44:55",
			ShelfSerialNumber: "SHELF123",
			Labels:            map[string]string{},
			Site:              nil,
			Created:           time.Now(),
			Updated:           time.Now(),
		}

		apiEPS := NewAPIExpectedPowerShelf(dbEPS)
		assert.NotNil(t, apiEPS)
		assert.Nil(t, apiEPS.Site)
	})
}

func TestAPIExpectedPowerShelfUpdateRequest_ToProto(t *testing.T) {
	tests := []struct {
		name      string
		body      string
		wantPaths []string
	}{
		{name: "omitted fields preserve Core state", body: `{}`},
		{name: "null fields preserve Core state", body: `{"defaultBmcUsername":null,"defaultBmcPassword":null,"labels":null,"slotId":null}`},
		{name: "explicit zero and empty values remain selected", body: `{"slotId":0,"labels":{}}`, wantPaths: []string{"metadata.labels"}},
		{name: "slot ID alone selects derived labels", body: `{"slotId":0}`, wantPaths: []string{"metadata.labels"}},
		{name: "BMC username leaves the password unselected", body: `{"defaultBmcUsername":"admin","defaultBmcPassword":null}`, wantPaths: []string{"bmc_username"}},
		{name: "BMC password leaves the username unselected", body: `{"defaultBmcPassword":"secret"}`, wantPaths: []string{"bmc_password"}},
		{name: "BMC pair is selected together", body: `{"defaultBmcUsername":"admin","defaultBmcPassword":"secret"}`, wantPaths: []string{"bmc_username", "bmc_password"}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var request APIExpectedPowerShelfUpdateRequest
			require.NoError(t, json.Unmarshal([]byte(test.body), &request))
			require.NoError(t, request.Validate())
			patch := request.ToProto(&cdbm.ExpectedPowerShelf{SlotID: cutil.GetPtr(int32(0))})
			encoded, err := protojson.Marshal(patch)
			require.NoError(t, err)
			var decoded corev1.PatchExpectedPowerShelfRequest
			require.NoError(t, protojson.Unmarshal(encoded, &decoded))
			require.NotNil(t, decoded.UpdateMask)
			assert.ElementsMatch(t, test.wantPaths, decoded.GetUpdateMask().GetPaths())
			if request.SlotID != nil {
				labels := decoded.GetExpectedPowerShelf().GetMetadata().GetLabels()
				require.Len(t, labels, 1)
				assert.Equal(t, "slot_id", labels[0].GetKey())
				assert.Equal(t, "0", labels[0].GetValue())
			}
			if request.DefaultBmcUsername != nil {
				assert.Equal(t, *request.DefaultBmcUsername, decoded.GetExpectedPowerShelf().GetBmcUsername())
			}
			if request.DefaultBmcPassword != nil {
				assert.Equal(t, *request.DefaultBmcPassword, decoded.GetExpectedPowerShelf().GetBmcPassword())
			}
		})
	}
}
