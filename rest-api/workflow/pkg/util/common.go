// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"context"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/google/uuid"
)

var (
	// ErrMsgSiteControllerRowNotFound is returned when an entity is not found in Site Controller
	ErrMsgSiteControllerRowNotFound = "row not found"
	// ErrMsgSiteControllerNoRowsReturned is returned when lookup for entity returns nothing in Site Controller
	ErrMsgSiteControllerNoRowsReturned = "no rows returned"
	// ErrMsgSiteControllerMarkedForDeletion is returned when an entity is marked for deletion in Site Controller
	ErrMsgSiteControllerMarkedForDeletion = "marked for deletion"
	// ErrMsgSiteControllerCouldNotFind is returned when an entity is not found in Site Controller
	ErrMsgSiteControllerCouldNotFind = "could not find"
	// ErrMsgSiteControllerDuplicateEntryFound is returned when an entity is found in Site Controller
	ErrMsgSiteControllerDuplicateEntryFound = "duplicate key value violates unique constraint"
)

func PtrsEqual[T comparable](i1 *T, i2 *T) bool {
	// They're either both nil or both non-nil
	// Otherwise, they certainly don't match.
	if (i1 == nil) != (i2 == nil) {
		return false
	}

	// We know their nil-ness is the same,
	// so if one is non-nil, then we can
	// compare the actual values being pointed
	// to by both.
	if i1 != nil && *i1 != *i2 {
		return false
	}

	return true
}

// UpdateNVLinkLogicalPartitionStatusInDB updates the NVLinkLogicalPartition status in the DB and creates a new StatusDetail
func UpdateNVLinkLogicalPartitionStatusInDB(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, nvlinklogicalpartitionID uuid.UUID, status *cdbm.NVLinkLogicalPartitionStatus, statusMessage *string) (*cdbm.NVLinkLogicalPartition, *cdbm.StatusDetail, error) {
	var updatedNVLinkLogicalPartition *cdbm.NVLinkLogicalPartition
	var err error
	var newSSD *cdbm.StatusDetail
	if status != nil {
		nvlinklogicalpartitionDAO := cdbm.NewNVLinkLogicalPartitionDAO(dbSession)
		updatedNVLinkLogicalPartition, err = nvlinklogicalpartitionDAO.Update(
			ctx,
			tx,
			cdbm.NVLinkLogicalPartitionUpdateInput{
				NVLinkLogicalPartitionID: nvlinklogicalpartitionID,
				Status:                   status,
			},
		)
		if err != nil {
			return updatedNVLinkLogicalPartition, newSSD, err
		}

		statusDetailDAO := cdbm.NewStatusDetailDAO(dbSession)
		newSSD, err = statusDetailDAO.Create(ctx, tx, cdbm.StatusDetailCreateInput{EntityID: nvlinklogicalpartitionID.String(), Status: string(*status), Message: statusMessage})
		if err != nil {
			return updatedNVLinkLogicalPartition, newSSD, err
		}
	}
	return updatedNVLinkLogicalPartition, newSSD, err
}
