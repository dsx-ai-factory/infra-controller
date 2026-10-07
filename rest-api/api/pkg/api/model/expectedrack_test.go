// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"strings"
	"testing"
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPIExpectedRackCreateRequest_Validate(t *testing.T) {
	validSiteID := "550e8400-e29b-41d4-a716-446655440000"
	validName := "rack-alpha"
	validDescription := "Primary compute rack"
	emptyString := ""

	tests := []struct {
		desc      string
		obj       APIExpectedRackCreateRequest
		expectErr bool
	}{
		{
			desc: "ok when all required fields are provided",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-001",
				RackProfileID: "rack-profile-001",
			},
			expectErr: false,
		},
		{
			desc: "ok when name, description, and valid labels are provided",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-002",
				RackProfileID: "rack-profile-001",
				Name:          &validName,
				Description:   &validDescription,
				Labels:        map[string]string{"env": "production", "zone": "us-east-1"},
			},
			expectErr: false,
		},
		{
			desc: "error when SiteID is empty",
			obj: APIExpectedRackCreateRequest{
				SiteID:        "",
				RackID:        "rack-001",
				RackProfileID: "rack-profile-001",
			},
			expectErr: true,
		},
		{
			desc: "error when SiteID is not a valid UUID",
			obj: APIExpectedRackCreateRequest{
				SiteID:        "not-a-valid-uuid",
				RackID:        "rack-001",
				RackProfileID: "rack-profile-001",
			},
			expectErr: true,
		},
		{
			desc: "error when RackID is missing",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "",
				RackProfileID: "rack-profile-001",
			},
			expectErr: true,
		},
		{
			desc: "error when RackID is whitespace only",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "   ",
				RackProfileID: "rack-profile-001",
			},
			expectErr: true,
		},
		{
			desc: "ok when RackProfileID is missing",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-001",
				RackProfileID: "",
			},
			expectErr: false,
		},
		{
			desc: "ignored legacy RackProfileID is not validated",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-001",
				RackProfileID: "   ",
			},
			expectErr: false,
		},
		{
			desc: "error when Name is provided but empty",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-001",
				RackProfileID: "rack-profile-001",
				Name:          &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "error when Description is provided but empty",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-001",
				RackProfileID: "rack-profile-001",
				Description:   &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "error when labels contain an empty key",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-001",
				RackProfileID: "rack-profile-001",
				Labels:        map[string]string{"": "value"},
			},
			expectErr: true,
		},
		{
			desc: "error when labels exceed the maximum count",
			obj: APIExpectedRackCreateRequest{
				SiteID:        validSiteID,
				RackID:        "rack-001",
				RackProfileID: "rack-profile-001",
				Labels: func() map[string]string {
					m := make(map[string]string, 11)
					for i := 0; i < 11; i++ {
						m[strings.Repeat("k", i+1)] = "v"
					}
					return m
				}(),
			},
			expectErr: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
		})
	}
}

func TestAPIExpectedRackUpdateRequest_Validate(t *testing.T) {
	emptyString := ""
	validRackID := "rack-renamed"
	validRackProfileID := "rack-profile-002"
	validName := "renamed"
	validUUID := "550e8400-e29b-41d4-a716-446655440000"
	invalidUUID := "not-a-uuid"

	tests := []struct {
		desc      string
		obj       APIExpectedRackUpdateRequest
		expectErr bool
	}{
		{
			desc:      "error when no mutable fields are provided (empty update rejected)",
			obj:       APIExpectedRackUpdateRequest{},
			expectErr: true,
		},
		{
			desc: "error when only ignored RackProfileID is provided",
			obj: APIExpectedRackUpdateRequest{
				RackProfileID: &validRackProfileID,
			},
			expectErr: true,
		},
		{
			desc: "error when ID and ignored RackProfileID are provided without mutable fields",
			obj: APIExpectedRackUpdateRequest{
				ID:            &validUUID,
				RackProfileID: &validRackProfileID,
			},
			expectErr: true,
		},
		{
			desc: "ok when RackID is provided (structural validation; immutability is enforced by the handler)",
			obj: APIExpectedRackUpdateRequest{
				RackID: &validRackID,
			},
			expectErr: false,
		},
		{
			desc: "error when ID is provided but empty",
			obj: APIExpectedRackUpdateRequest{
				ID: &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "error when ID is not a valid UUID",
			obj: APIExpectedRackUpdateRequest{
				ID: &invalidUUID,
			},
			expectErr: true,
		},
		{
			desc: "error when RackID is provided but empty",
			obj: APIExpectedRackUpdateRequest{
				RackID: &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "error when RackID is whitespace only",
			obj: APIExpectedRackUpdateRequest{
				RackID: func() *string { s := "   "; return &s }(),
			},
			expectErr: true,
		},
		{
			desc: "error when RackProfileID is provided but empty",
			obj: APIExpectedRackUpdateRequest{
				RackProfileID: &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "ok when name and valid labels are provided",
			obj: APIExpectedRackUpdateRequest{
				RackProfileID: &validRackProfileID,
				Name:          &validName,
				Labels:        map[string]string{"team": "infra"},
			},
			expectErr: false,
		},
		{
			desc: "error when Name is provided but empty",
			obj: APIExpectedRackUpdateRequest{
				Name: &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "error when Description is provided but empty",
			obj: APIExpectedRackUpdateRequest{
				Description: &emptyString,
			},
			expectErr: true,
		},
		{
			desc: "error when labels are invalid",
			obj: APIExpectedRackUpdateRequest{
				Labels: map[string]string{"": "v"},
			},
			expectErr: true,
		},
	}

	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			err := tc.obj.Validate()
			assert.Equal(t, tc.expectErr, err != nil)
		})
	}
}

func TestAPIReplaceAllExpectedRacksRequest_Validate(t *testing.T) {
	validSiteID := "550e8400-e29b-41d4-a716-446655440000"
	otherSiteID := "550e8400-e29b-41d4-a716-446655440001"

	tests := []struct {
		desc      string
		obj       APIReplaceAllExpectedRacksRequest
		expectErr bool
	}{
		{
			desc: "ok when site is valid and list is empty",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID:        validSiteID,
				ExpectedRacks: nil,
			},
			expectErr: false,
		},
		{
			desc: "ok when all entries match the top-level site and ids are unique",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID: validSiteID,
				ExpectedRacks: []*APIExpectedRackCreateRequest{
					{
						SiteID:        validSiteID,
						RackID:        "rack-001",
						RackProfileID: "rack-profile-001",
					},
					{
						SiteID:        validSiteID,
						RackID:        "rack-002",
						RackProfileID: "rack-profile-001",
					},
				},
			},
			expectErr: false,
		},
		{
			desc: "error when SiteID is missing",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID: "",
			},
			expectErr: true,
		},
		{
			desc: "error when SiteID is not a valid UUID",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID: "not-a-uuid",
			},
			expectErr: true,
		},
		{
			desc: "error when entry has a site that does not match the top-level site",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID: validSiteID,
				ExpectedRacks: []*APIExpectedRackCreateRequest{
					{
						SiteID:        otherSiteID,
						RackID:        "rack-001",
						RackProfileID: "rack-profile-001",
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when an entry is nil",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID:        validSiteID,
				ExpectedRacks: []*APIExpectedRackCreateRequest{nil},
			},
			expectErr: true,
		},
		{
			desc: "error when an entry has an invalid create request",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID: validSiteID,
				ExpectedRacks: []*APIExpectedRackCreateRequest{
					{
						SiteID:        validSiteID,
						RackID:        "",
						RackProfileID: "rack-profile-001",
					},
				},
			},
			expectErr: true,
		},
		{
			desc: "error when duplicate rack ids are present",
			obj: APIReplaceAllExpectedRacksRequest{
				SiteID: validSiteID,
				ExpectedRacks: []*APIExpectedRackCreateRequest{
					{
						SiteID:        validSiteID,
						RackID:        "rack-001",
						RackProfileID: "rack-profile-001",
					},
					{
						SiteID:        validSiteID,
						RackID:        "rack-001",
						RackProfileID: "rack-profile-002",
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
		})
	}
}

func TestNewAPIExpectedRack(t *testing.T) {
	rackUUID := uuid.New()
	siteID := uuid.New()
	created := time.Now()
	updated := time.Now()

	t.Run("returns nil when input is nil", func(t *testing.T) {
		got := NewAPIExpectedRack(nil)
		assert.Nil(t, got)
	})

	t.Run("maps all fields", func(t *testing.T) {
		dbRack := &cdbm.ExpectedRack{
			ID:            rackUUID,
			RackID:        "rack-001",
			SiteID:        siteID,
			RackProfileID: "rack-profile-001",
			Name:          "rack-alpha",
			Description:   "Primary compute rack",
			Labels:        map[string]string{"env": "production"},
			Created:       created,
			Updated:       updated,
		}

		got := NewAPIExpectedRack(dbRack)
		if assert.NotNil(t, got) {
			assert.Equal(t, rackUUID, got.ID)
			assert.Equal(t, "rack-001", got.RackID)
			assert.Equal(t, siteID, got.SiteID)
			assert.Equal(t, "rack-profile-001", got.RackProfileID)
			assert.Equal(t, "rack-alpha", got.Name)
			assert.Equal(t, "Primary compute rack", got.Description)
			assert.Equal(t, APILabels{"env": "production"}, got.Labels)
			assert.Equal(t, created, got.Created)
			assert.Equal(t, updated, got.Updated)
			assert.Nil(t, got.Site)
		}
	})

	t.Run("maps minimal db model with empty fields", func(t *testing.T) {
		dbRack := &cdbm.ExpectedRack{
			ID:            rackUUID,
			RackID:        "rack-002",
			SiteID:        siteID,
			RackProfileID: "rack-profile-002",
			Created:       created,
			Updated:       updated,
		}

		got := NewAPIExpectedRack(dbRack)
		if assert.NotNil(t, got) {
			assert.Equal(t, rackUUID, got.ID)
			assert.Equal(t, "rack-002", got.RackID)
			assert.Equal(t, siteID, got.SiteID)
			assert.Equal(t, "rack-profile-002", got.RackProfileID)
			assert.Empty(t, got.Name)
			assert.Empty(t, got.Description)
			assert.Empty(t, got.Labels)
			assert.Nil(t, got.Site)
		}
	})

	t.Run("maps Site when provided", func(t *testing.T) {
		site := &cdbm.Site{
			ID:                       siteID,
			Name:                     "Test Site",
			Org:                      "test-org",
			InfrastructureProviderID: uuid.New(),
			IsSerialConsoleEnabled:   false,
			Status:                   "active",
			Created:                  created,
			Updated:                  updated,
		}

		dbRack := &cdbm.ExpectedRack{
			ID:            rackUUID,
			RackID:        "rack-003",
			SiteID:        siteID,
			RackProfileID: "rack-profile-003",
			Site:          site,
			Created:       created,
			Updated:       updated,
		}

		got := NewAPIExpectedRack(dbRack)
		if assert.NotNil(t, got) && assert.NotNil(t, got.Site) {
			assert.Equal(t, siteID.String(), got.Site.ID)
			assert.Equal(t, "Test Site", got.Site.Name)
			assert.Equal(t, "test-org", got.Site.Org)
		}
	})
}
