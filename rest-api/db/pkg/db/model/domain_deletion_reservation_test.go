// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

func TestDomainSQLDAO_RestoreRejectedDeletion(t *testing.T) {
	ctx := context.Background()
	session := testDomainInitDB(t)
	defer session.Close()
	testDomainSetupSchema(t, session)
	user := testDomainBuildUser(t, session, "deletion-reservation-user")
	coreID := uuid.New()
	dao := NewDomainDAO(session)
	domain, err := dao.Create(ctx, nil, DomainCreateInput{
		Hostname:           "deletion-reservation.example.com",
		Org:                "deletion-reservation",
		ControllerDomainID: &coreID,
		Status:             DomainStatusReady,
		CreatedBy:          user.ID,
	})
	require.NoError(t, err)

	firstReservation, err := db.WithTxResult(ctx, session, func(tx *db.Tx) (*time.Time, error) {
		return dao.ReserveDeletionOwned(ctx, tx, domain.ID, coreID, DomainStatusReady, 90*time.Second)
	})
	require.NoError(t, err)
	require.NotNil(t, firstReservation)
	secondReservation, err := db.WithTxResult(ctx, session, func(tx *db.Tx) (*time.Time, error) {
		return dao.ReserveDeletionOwned(ctx, tx, domain.ID, coreID, DomainStatusDeleting, 90*time.Second)
	})
	require.NoError(t, err)
	require.NotNil(t, secondReservation)
	require.True(t, secondReservation.After(*firstReservation), "each reservation must get a distinct fencing timestamp")

	restored, err := db.WithTxResult(ctx, session, func(tx *db.Tx) (bool, error) {
		return dao.RestoreRejectedDeletion(ctx, tx, domain.ID, coreID, *firstReservation)
	})
	require.NoError(t, err)
	require.False(t, restored, "an older request must not restore over a newer deletion reservation")
	persisted, err := dao.GetByID(ctx, nil, domain.ID, nil)
	require.NoError(t, err)
	require.Equal(t, DomainStatusDeleting, persisted.Status)
	require.True(t, persisted.Updated.Equal(*secondReservation))
}
