// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"slices"
	"strings"
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/google/uuid"
	"github.com/stretchr/testify/mock"
	"github.com/stretchr/testify/require"
	tmocks "go.temporal.io/sdk/mocks"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
)

func TestManageSitePrefixInventory_DiscoverSitePrefixInventory(t *testing.T) {
	firstID := &corev1.SitePrefixId{Value: "00000000-0000-0000-0000-000000000001"}
	secondID := &corev1.SitePrefixId{Value: "00000000-0000-0000-0000-000000000002"}
	thirdID := &corev1.SitePrefixId{Value: "00000000-0000-0000-0000-000000000003"}

	tests := []struct {
		name                  string
		itemCount             int
		sitePrefixIDs         []*corev1.SitePrefixId
		findErr               error
		versionErr            error
		findByIDsErr          error
		findByIDsErrorID      string
		maxFindByIDs          uint32
		cloudPageSize         int
		wantErr               bool
		wantErrText           string
		wantCode              codes.Code
		wantStatus            corev1.InventoryStatus
		wantStatusMessage     string
		wantPageItems         []int
		wantPageSizes         []int32
		wantTotalPages        int32
		wantCompleteItemCount int
		wantIncomplete        bool
	}{
		{
			name:                  "empty inventory",
			maxFindByIDs:          100,
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			wantStatusMessage:     "No items reported by Site Controller",
			wantPageItems:         []int{0},
			wantPageSizes:         []int32{25},
			wantCompleteItemCount: 0,
		},
		{
			name:                  "inventory spans REST pages",
			itemCount:             26,
			maxFindByIDs:          100,
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			wantStatusMessage:     "Successfully retrieved from Site Controller",
			wantPageItems:         []int{25, 1},
			wantPageSizes:         []int32{25, 1},
			wantTotalPages:        2,
			wantCompleteItemCount: 26,
		},
		{
			name:                  "dedicated Core API is unsupported",
			findErr:               status.Error(codes.Unimplemented, "unsupported"),
			maxFindByIDs:          100,
			wantErr:               true,
			wantCode:              codes.Unimplemented,
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			wantStatusMessage:     "failed to retrieve SitePrefix IDs",
			wantPageItems:         []int{0},
			wantCompleteItemCount: 0,
		},
		{
			name:                  "Core configuration lookup fails",
			versionErr:            status.Error(codes.Unavailable, "version unavailable"),
			maxFindByIDs:          100,
			wantErr:               true,
			wantErrText:           "read Core max_find_by_ids",
			wantCode:              codes.Unavailable,
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			wantStatusMessage:     "version unavailable",
			wantPageItems:         []int{0},
			wantCompleteItemCount: 0,
		},
		{
			name:                  "Core page size follows max_find_by_ids",
			itemCount:             3,
			maxFindByIDs:          2,
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			wantStatusMessage:     "Successfully retrieved from Site Controller",
			wantPageItems:         []int{3},
			wantPageSizes:         []int32{3},
			wantTotalPages:        1,
			wantCompleteItemCount: 3,
		},
		{
			name:                  "zero Core page limit",
			wantErr:               true,
			wantErrText:           "max_find_by_ids must be greater than zero",
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			wantStatusMessage:     "max_find_by_ids must be greater than zero",
			wantPageItems:         []int{0},
			wantCompleteItemCount: 0,
		},
		{
			name:                  "duplicate IDs cross Core request boundary",
			sitePrefixIDs:         []*corev1.SitePrefixId{secondID, firstID, secondID},
			maxFindByIDs:          2,
			wantErr:               true,
			wantErrText:           "duplicate SitePrefix ID",
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_FAILED,
			wantStatusMessage:     `received duplicate SitePrefix ID "00000000-0000-0000-0000-000000000002" from Core`,
			wantPageItems:         []int{0},
			wantCompleteItemCount: 0,
		},
		{
			name:                  "later Core batch failure leaves an incomplete run",
			sitePrefixIDs:         []*corev1.SitePrefixId{thirdID, firstID, secondID},
			findByIDsErr:          status.Error(codes.Unavailable, "later batch unavailable"),
			findByIDsErrorID:      thirdID.GetValue(),
			maxFindByIDs:          2,
			cloudPageSize:         1,
			wantErr:               true,
			wantErrText:           "later batch unavailable",
			wantCode:              codes.Unavailable,
			wantStatus:            corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
			wantStatusMessage:     "Successfully retrieved from Site Controller",
			wantPageItems:         []int{1},
			wantPageSizes:         []int32{1},
			wantTotalPages:        3,
			wantCompleteItemCount: 3,
			wantIncomplete:        true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			mockCoreGrpcClient := cClient.NewMockCoreGrpcClient()
			coreGrpcAtomicClient := cClient.NewCoreGrpcAtomicClient(&cClient.CoreGrpcClientConfig{})
			coreGrpcAtomicClient.SwapClient(mockCoreGrpcClient)

			siteID := uuid.New()
			publishClient := &tmocks.Client{}
			publishClient.On(
				"ExecuteWorkflow",
				mock.Anything,
				mock.Anything,
				"UpdateSitePrefixInventory",
				siteID,
				mock.AnythingOfType("*core.SitePrefixInventory"),
			).Return(&tmocks.WorkflowRun{}, nil)

			cloudPageSize := tt.cloudPageSize
			if cloudPageSize == 0 {
				cloudPageSize = 25
			}
			manager := NewManageSitePrefixInventory(ManageInventoryConfig{
				SiteID:                siteID,
				CoreGrpcAtomicClient:  coreGrpcAtomicClient,
				TemporalPublishClient: publishClient,
				TemporalPublishQueue:  "test-queue",
				SitePageSize:          100,
				CloudPageSize:         cloudPageSize,
			})

			ctx := cClient.WithMockSitePrefixCount(context.Background(), tt.itemCount)
			ctx = cClient.WithMockSitePrefixMaxFindByIDs(ctx, tt.maxFindByIDs)
			if tt.sitePrefixIDs != nil {
				ctx = cClient.WithMockSitePrefixIDs(ctx, tt.sitePrefixIDs)
			}
			if tt.findErr != nil {
				ctx = cClient.WithMockSitePrefixError(ctx, tt.findErr)
			}
			if tt.versionErr != nil {
				ctx = cClient.WithMockSitePrefixVersionError(ctx, tt.versionErr)
			}
			if tt.findByIDsErr != nil {
				ctx = cClient.WithMockSitePrefixFindByIDsError(ctx, tt.findByIDsErrorID, tt.findByIDsErr)
			}

			err := manager.DiscoverSitePrefixInventory(ctx)
			if tt.wantErr {
				require.Error(t, err)
				if tt.wantCode != codes.OK {
					require.Equal(t, tt.wantCode, status.Code(err))
				}
				if tt.wantErrText != "" {
					require.ErrorContains(t, err, tt.wantErrText)
				}
			} else {
				require.NoError(t, err)
			}

			require.Len(t, publishClient.Calls, len(tt.wantPageItems))

			firstInventory, ok := publishClient.Calls[0].Arguments[4].(*corev1.SitePrefixInventory)
			require.True(t, ok)
			runTimestamp := firstInventory.GetTimestamp()
			require.NotNil(t, runTimestamp)
			require.NoError(t, runTimestamp.CheckValid())
			var completeIDs []string
			publishedIDs := make([]string, 0, tt.wantCompleteItemCount)
			seenIDs := make(map[string]struct{})
			for pageIndex, call := range publishClient.Calls {
				inventory, ok := call.Arguments[4].(*corev1.SitePrefixInventory)
				require.True(t, ok)
				require.Equal(t, tt.wantStatus, inventory.GetInventoryStatus())
				require.Contains(t, inventory.GetStatusMsg(), tt.wantStatusMessage)
				require.Len(t, inventory.GetSitePrefixes(), tt.wantPageItems[pageIndex])
				require.True(t, proto.Equal(runTimestamp, inventory.GetTimestamp()))

				if tt.wantStatus == corev1.InventoryStatus_INVENTORY_STATUS_FAILED {
					require.Nil(t, inventory.GetInventoryPage())
					continue
				}

				page := inventory.GetInventoryPage()
				require.NotNil(t, page)
				require.Equal(t, int32(pageIndex+1), page.GetCurrentPage())
				require.Equal(t, tt.wantTotalPages, page.GetTotalPages())
				require.Equal(t, tt.wantPageSizes[pageIndex], page.GetPageSize())
				require.Equal(t, int32(tt.wantCompleteItemCount), page.GetTotalItems())
				if pageIndex == 0 {
					completeIDs = slices.Clone(page.GetItemIds())
					require.Len(t, completeIDs, tt.wantCompleteItemCount)
					require.True(t, slices.IsSorted(completeIDs))
				} else {
					require.Equal(t, completeIDs, page.GetItemIds())
				}
				require.True(t, slices.IsSortedFunc(inventory.GetSitePrefixes(), func(left, right *corev1.SitePrefix) int {
					return strings.Compare(left.GetId().GetValue(), right.GetId().GetValue())
				}))
				for _, sitePrefix := range inventory.GetSitePrefixes() {
					id := sitePrefix.GetId().GetValue()
					require.Contains(t, completeIDs, id)
					require.NotContains(t, seenIDs, id)
					seenIDs[id] = struct{}{}
					publishedIDs = append(publishedIDs, id)
				}
			}
			if tt.wantIncomplete {
				lastInventory, ok := publishClient.Calls[len(publishClient.Calls)-1].Arguments[4].(*corev1.SitePrefixInventory)
				require.True(t, ok)
				lastPage := lastInventory.GetInventoryPage()
				require.Less(t, lastPage.GetCurrentPage(), lastPage.GetTotalPages())
				require.NotEqual(t, completeIDs, publishedIDs)
			} else if tt.wantStatus == corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS {
				require.Equal(t, completeIDs, publishedIDs)
			}
		})
	}
}

func TestSitePrefixFindIDs(t *testing.T) {
	tests := []struct {
		name    string
		ids     []*corev1.SitePrefixId
		wantErr string
	}{
		{
			name:    "rejects an empty ID",
			ids:     []*corev1.SitePrefixId{nil},
			wantErr: "empty SitePrefix ID",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := cClient.WithMockSitePrefixIDs(context.Background(), tt.ids)
			_, err := sitePrefixFindIDs(ctx, cClient.NewMockCoreGrpcClient())
			require.ErrorContains(t, err, tt.wantErr)
		})
	}
}

func TestSitePrefixFindByIDs(t *testing.T) {
	id := func(value string) *corev1.SitePrefixId {
		return &corev1.SitePrefixId{Value: value}
	}
	sitePrefix := func(value string) *corev1.SitePrefix {
		return &corev1.SitePrefix{Id: id(value)}
	}

	tests := []struct {
		name         string
		requestedIDs []*corev1.SitePrefixId
		sitePrefixes []*corev1.SitePrefix
		wantErr      string
	}{
		{
			name:         "exact set in a different order",
			requestedIDs: []*corev1.SitePrefixId{id("first"), id("second")},
			sitePrefixes: []*corev1.SitePrefix{sitePrefix("second"), sitePrefix("first")},
		},
		{
			name:         "missing requested ID",
			requestedIDs: []*corev1.SitePrefixId{id("first"), id("second")},
			sitePrefixes: []*corev1.SitePrefix{sitePrefix("first")},
			wantErr:      "did not return requested SitePrefix ID \"second\"",
		},
		{
			name:         "duplicate returned ID",
			requestedIDs: []*corev1.SitePrefixId{id("first")},
			sitePrefixes: []*corev1.SitePrefix{sitePrefix("first"), sitePrefix("first")},
			wantErr:      "returned duplicate SitePrefix ID \"first\"",
		},
		{
			name:         "unrequested returned ID",
			requestedIDs: []*corev1.SitePrefixId{id("first")},
			sitePrefixes: []*corev1.SitePrefix{sitePrefix("other")},
			wantErr:      "returned unrequested SitePrefix ID \"other\"",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			ctx := cClient.WithMockSitePrefixResponse(context.Background(), tt.sitePrefixes)
			got, err := sitePrefixFindByIDs(ctx, cClient.NewMockCoreGrpcClient(), tt.requestedIDs)
			if tt.wantErr != "" {
				require.ErrorContains(t, err, tt.wantErr)
				return
			}
			require.NoError(t, err)
			require.True(t, slices.IsSortedFunc(got, func(left, right *corev1.SitePrefix) int {
				return strings.Compare(left.GetId().GetValue(), right.GetId().GetValue())
			}))
		})
	}
}
