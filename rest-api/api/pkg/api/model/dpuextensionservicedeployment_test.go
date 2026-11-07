// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPIDpuExtensionServiceDeploymentRequest_Validate(t *testing.T) {
	validUUID := uuid.New().String()

	tests := []struct {
		desc      string
		obj       APIDpuExtensionServiceDeploymentRequest
		expectErr bool
	}{
		{
			desc: "ok when all required fields are provided",
			obj: APIDpuExtensionServiceDeploymentRequest{
				DpuExtensionServiceID: validUUID,
				Version:               "V1-T1761856992374052",
			},
			expectErr: false,
		},
		{
			desc: "error when dpuExtensionServiceId is missing",
			obj: APIDpuExtensionServiceDeploymentRequest{
				Version: "V1-T1761856992374052",
			},
			expectErr: true,
		},
		{
			desc: "error when dpuExtensionServiceId is not a valid UUID",
			obj: APIDpuExtensionServiceDeploymentRequest{
				DpuExtensionServiceID: "invalid-uuid",
				Version:               "V1-T1761856992374052",
			},
			expectErr: true,
		},
		{
			desc: "error when dpuExtensionServiceId is empty",
			obj: APIDpuExtensionServiceDeploymentRequest{
				DpuExtensionServiceID: "",
				Version:               "V1-T1761856992374052",
			},
			expectErr: true,
		},
		{
			desc: "error when version is missing",
			obj: APIDpuExtensionServiceDeploymentRequest{
				DpuExtensionServiceID: validUUID,
			},
			expectErr: true,
		},
		{
			desc: "error when version is empty",
			obj: APIDpuExtensionServiceDeploymentRequest{
				DpuExtensionServiceID: validUUID,
				Version:               "",
			},
			expectErr: true,
		},
		{
			desc:      "error when both fields are missing",
			obj:       APIDpuExtensionServiceDeploymentRequest{},
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

func TestNewAPIDpuExtensionServiceDeployment(t *testing.T) {
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
		},
		Status:  cdbm.DpuExtensionServiceStatusReady,
		Created: time.Now(),
		Updated: time.Now(),
	}

	dbdesd := &cdbm.DpuExtensionServiceDeployment{
		ID:                  uuid.New(),
		DpuExtensionService: dbdes,
		Version:             *dbdes.Version,
		Status:              cdbm.DpuExtensionServiceDeploymentStatusRunning,
		Created:             time.Now(),
		Updated:             time.Now(),
	}

	tests := []struct {
		desc   string
		dbdesd *cdbm.DpuExtensionServiceDeployment
	}{
		{
			desc:   "ok when all required fields are provided",
			dbdesd: dbdesd,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			adesd := NewAPIDpuExtensionServiceDeployment(tc.dbdesd)
			assert.Equal(t, tc.dbdesd.ID.String(), adesd.ID)
			assert.Equal(t, tc.dbdesd.DpuExtensionService.ID.String(), adesd.DpuExtensionService.ID)
			assert.Equal(t, tc.dbdesd.DpuExtensionService.Name, adesd.DpuExtensionService.Name)
			assert.Equal(t, tc.dbdesd.DpuExtensionService.ServiceType, adesd.DpuExtensionService.ServiceType)
			assert.Equal(t, tc.dbdesd.DpuExtensionService.Version, adesd.DpuExtensionService.LatestVersion)
			assert.Equal(t, tc.dbdesd.DpuExtensionService.Status, adesd.DpuExtensionService.Status)
			assert.Equal(t, tc.dbdesd.Version, adesd.Version)
			assert.Equal(t, tc.dbdesd.Status, adesd.Status)
			assert.Equal(t, tc.dbdesd.Created, adesd.Created)
			assert.Equal(t, tc.dbdesd.Updated, adesd.Updated)
		})
	}
}
