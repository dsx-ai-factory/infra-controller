// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"testing"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestAPISSHKeyCreateRequest_Validate(t *testing.T) {

	validPublicKey := "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICip4hl6WjuVHs60PeikVUs0sWE/kPhk2D0rRHWsIuyL jdoe@test.com"
	invalidPublicKey := "badpublickey"
	tests := []struct {
		desc      string
		obj       APISSHKeyCreateRequest
		expectErr bool
	}{
		{
			desc:      "ok when only required fields are provided",
			obj:       APISSHKeyCreateRequest{Name: "test", PublicKey: validPublicKey},
			expectErr: false,
		},
		{
			desc:      "ok when all fields are provided",
			obj:       APISSHKeyCreateRequest{Name: "test", PublicKey: validPublicKey, SSHKeyGroupID: cutil.GetPtr(uuid.New().String())},
			expectErr: false,
		},
		{
			desc:      "error when required fields are not provided",
			obj:       APISSHKeyCreateRequest{Name: "test"},
			expectErr: true,
		},
		{
			desc:      "error when sshkey group is invalid",
			obj:       APISSHKeyCreateRequest{Name: "test", SSHKeyGroupID: cutil.GetPtr("test"), PublicKey: validPublicKey},
			expectErr: true,
		},
		{
			desc:      "error when public key is invalid",
			obj:       APISSHKeyCreateRequest{Name: "test", SSHKeyGroupID: cutil.GetPtr(uuid.New().String()), PublicKey: invalidPublicKey},
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

func TestAPISSHKeyUpdateRequest_Validate(t *testing.T) {
	tests := []struct {
		desc      string
		obj       APISSHKeyUpdateRequest
		expectErr bool
	}{
		{
			desc:      "Success case",
			obj:       APISSHKeyUpdateRequest{Name: cutil.GetPtr("updatedname")},
			expectErr: false,
		},
		{
			desc:      "Failure case",
			obj:       APISSHKeyUpdateRequest{Name: cutil.GetPtr("e")},
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

func TestAPISSHKeyNew(t *testing.T) {
	dbSSHKey := &cdbm.SSHKey{
		ID:          uuid.New(),
		Name:        "test",
		Org:         "test",
		TenantID:    uuid.New(),
		PublicKey:   "testkey",
		Fingerprint: cutil.GetPtr("test"),
		Expires:     cutil.GetPtr(cdb.GetCurTime()),
		Created:     cdb.GetCurTime(),
		Updated:     cdb.GetCurTime(),
	}
	dbskas := []cdbm.SSHKeyAssociation{
		{
			ID:            uuid.New(),
			SSHKeyID:      uuid.New(),
			SSHKeyGroupID: uuid.New(),
			Created:       cdb.GetCurTime(),
			Updated:       cdb.GetCurTime(),
		},
	}
	tests := []struct {
		desc   string
		dbObj  *cdbm.SSHKey
		dbskas []cdbm.SSHKeyAssociation
	}{
		{
			desc:   "test creating API SecurityGroup",
			dbObj:  dbSSHKey,
			dbskas: dbskas,
		},
	}
	for _, tc := range tests {
		t.Run(tc.desc, func(t *testing.T) {
			got := NewAPISSHKey(tc.dbObj, tc.dbskas)
			assert.Equal(t, tc.dbObj.ID.String(), got.ID)
		})
	}
}
