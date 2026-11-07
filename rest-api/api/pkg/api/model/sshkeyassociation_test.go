// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"testing"
	"time"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
)

func TestNewAPISSHKeyAssociation(t *testing.T) {
	ska := cdbm.SSHKeyAssociation{
		ID:            uuid.New(),
		SSHKeyID:      uuid.New(),
		SSHKeyGroupID: uuid.New(),
		Created:       time.Now(),
		Updated:       time.Now(),
	}
	apiska := NewAPISSHKeyAssociation(&ska)
	assert.Equal(t, apiska.ID, ska.ID.String())
	assert.Equal(t, apiska.SSHKeyID, ska.SSHKeyID.String())
	assert.Equal(t, apiska.SSHKeyGroupID, ska.SSHKeyGroupID.String())
}
