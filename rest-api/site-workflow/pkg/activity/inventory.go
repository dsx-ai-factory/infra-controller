// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	cClient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	"github.com/google/uuid"
	"github.com/rs/zerolog"
	"github.com/rs/zerolog/log"
	tClient "go.temporal.io/sdk/client"
	"go.temporal.io/sdk/converter"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"
)

const (
	// sitePageSizeStep is how much the Core fetch page shrinks after a response that exceeds
	// the gRPC client receive limit.
	sitePageSizeStep = 10
	// minSitePageSize floors the Core fetch ladder at a single item, mirroring the publish
	// ladder. Stopping any higher would give up while a smaller page would still have fit, and
	// that failure repeats every run, so inventory for the type stays down until the ceiling is
	// raised and redeployed. Only one item that does not fit leaves nothing left to try.
	minSitePageSize = 1

	// cloudPageSizeStep is how much the published page shrinks when a page exceeds the budget.
	cloudPageSizeStep = 5

	// statusPublishTimeout bounds the detached publish that reports a collection failure, so
	// telling Cloud the run failed cannot outlive the run by much.
	statusPublishTimeout = 30 * time.Second

	// maxPublishPayloadBytes budgets one published inventory page at 1.9 MiB, under the 2 MiB
	// blob Temporal rejects (limit.blobSize.error). The remaining margin is not accounting for
	// anything: payloadSize already measures the Site ID argument and the payload framing. It
	// covers a deployment that configures a lower limit than the Site Agent can see, and the
	// cost of guessing wrong, since a rejected page fails the activity and leaves inventory
	// stale until the next cron tick.
	maxPublishPayloadBytes = 1945 * 1024
)

type ManageInventoryConfig struct {
	SiteID                uuid.UUID
	CoreGrpcAtomicClient  *cClient.CoreGrpcAtomicClient
	TemporalPublishClient tClient.Client
	TemporalPublishQueue  string
	SitePageSize          int
	CloudPageSize         int
}

type manageInventoryImpl[K any, R any, P any] struct {
	itemType               string
	config                 ManageInventoryConfig
	internalFindIDs        func(context.Context, *cClient.CoreGrpcClient) ([]K, error)
	internalFindByIDs      func(context.Context, *cClient.CoreGrpcClient, []K) ([]R, error)
	internalPagedInventory func([]K, []R, *pagedInventoryInput) P
	// post-processing function that can optionally be used to attach additional inventory data
	// based on the data in the inventory.  This will only be called for pages with inventory.
	internalPagedInventoryPostProcess func(context.Context, *cClient.CoreGrpcClient, P) (P, error)
	// fallback function to get all the items when pagination is not supported
	internalFindFallback func(ctx context.Context, client *cClient.CoreGrpcClient) ([]K, []R, error)
}

type pagedInventoryInput struct {
	// total number of items we need to publish
	totalItems int
	// total number of pages we need to publish
	totalPages int
	// size of the page
	pageSize int
	// current page number
	pageNumber int
	// status
	status        corev1.InventoryStatus
	statusMessage string
}

func (pii *pagedInventoryInput) buildPage() *corev1.InventoryPage {
	// if we are reporting an error, we do not want to include paging information
	if pii.status != corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS {
		return nil
	}
	return &corev1.InventoryPage{
		TotalPages:  int32(pii.totalPages),
		CurrentPage: int32(pii.pageNumber),
		PageSize:    int32(pii.pageSize),
		TotalItems:  int32(pii.totalItems),
	}
}

// CollectAndPublishInventory fetches the inventory from Core a site page at a time and publishes
// each page onwards to Cloud, so only one site page is held in memory. Both page sizes adapt: the
// Core fetch shrinks when a response outgrows the gRPC receive limit, and a published page shrinks
// until it fits Temporal's blob limit. A failure before any page goes out is reported to Cloud as
// a FAILED status page.
func (impl *manageInventoryImpl[K, R, P]) CollectAndPublishInventory(ctx context.Context, logger *zerolog.Logger) error {
	// get Core gRPC client
	grpcClient := impl.config.CoreGrpcAtomicClient.GetClient()
	if grpcClient == nil {
		return cClient.ErrCoreGrpcClientNotConnected
	}

	collector := impl.newCollector(grpcClient)

	// find IDs
	allIDs, err := impl.internalFindIDs(ctx, grpcClient)
	if err != nil {
		// The fallback reports its own failures, so returning here keeps a failed fallback from
		// publishing a second FAILED page under the same workflow ID.
		if status.Code(err) == codes.Unimplemented && impl.internalFindFallback != nil {
			log.Info().Msg("Using fallback API to get inventory")
			return impl.collectAndPublishFallback(ctx, logger, grpcClient)
		}
		logger.Warn().Err(err).Msg("Failed to retrieve IDs using Core gRPC API")
		collector.reportFailure(ctx, logger, err)
		return err
	}

	collector.allIDs = allIDs
	if len(allIDs) == 0 {
		logger.Info().Msg("Publishing empty inventory page to Cloud")
		err = collector.publishStatusOnly(ctx, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, "No items reported by Site Controller")
		if err != nil {
			logger.Error().Err(err).Msg("Failed to publish inventory to Cloud")
			return err
		}
		return nil
	}

	// Fetch the inventory a site page at a time and publish each site page onwards to Cloud, so
	// only one site page is held in memory at a time. The cursor advances by however many IDs the
	// fetch consumed, which shrinks when the site page ladder steps down.
	remainingIDs := allIDs
	for len(remainingIDs) > 0 {
		siteItems, consumed, err := collector.fetchSitePage(ctx, logger, remainingIDs)
		if err != nil {
			logger.Warn().Err(err).Int("Site Page", collector.sitePagesFetched+1).Msg("Failed to retrieve using Core gRPC API")
			collector.reportFailure(ctx, logger, err)
			return err
		}
		remainingIDs = remainingIDs[consumed:]

		// We could return an error, but that might get us caught in
		// a scenerio where deletes on site between FindIDs and FindByIDs
		// calls creates a mismatch that fails.  If we log the error, we could
		// alert on it without letting it break inventory entirely.
		if len(siteItems) != consumed {
			logger.Error().Msg("size of FindByIDs set does not match size of FindIDs set")
			// The page total is derived from the ID count, so an item Core did not return would
			// leave the last page short of its total and skip the Cloud deletion sweep for the
			// whole run. Discount what is missing to keep the last page recognizable.
			collector.itemsMissing += consumed - len(siteItems)
		}

		// A post-processing or publish failure aborts the run just as a fetch failure does, and
		// leaves Cloud with the same partial picture, so it is reported the same way.
		err = collector.bufferItems(ctx, logger, siteItems)
		if err != nil {
			collector.reportFailure(ctx, logger, err)
			return err
		}
	}

	err = collector.flush(ctx, logger)
	if err != nil {
		collector.reportFailure(ctx, logger, err)
		return err
	}
	return nil
}

// collectAndPublishFallback collects the whole inventory in one call, for a Core that does not
// implement the paged find API. It owns reporting its own failures to Cloud, so callers return its
// result rather than publishing a status page of their own.
func (impl *manageInventoryImpl[K, R, P]) collectAndPublishFallback(ctx context.Context, logger *zerolog.Logger,
	grpcClient *cClient.CoreGrpcClient) error {
	if impl.internalFindFallback == nil {
		return errors.New("no fallback find function defined")
	}
	collector := impl.newCollector(grpcClient)

	allIDs, siteItems, err := impl.internalFindFallback(ctx, grpcClient)
	if err != nil {
		logger.Warn().Err(err).Msg("Failed to retrieve using Site Controller fallback API")
		collector.reportFailure(ctx, logger, err)
		return err
	}

	collector.allIDs = allIDs
	if len(allIDs) == 0 {
		logger.Info().Msg("Publishing empty inventory page to Cloud")
		err = collector.publishStatusOnly(ctx, corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS, "No items reported by Site Controller")
		if err != nil {
			logger.Error().Err(err).Msg("Failed to publish inventory to Cloud")
			return err
		}
		return nil
	}

	err = collector.bufferItems(ctx, logger, siteItems)
	if err == nil {
		err = collector.flush(ctx, logger)
	}
	if err != nil {
		collector.reportFailure(ctx, logger, err)
		return err
	}
	return nil
}

// reportFailure tells Cloud the run failed, but only when no page has gone out yet. After a
// partial publish the pages already sent carry the run, and the unpaged workflow ID a status
// page uses would say nothing about where it stopped. A failure to report is logged rather
// than returned, so the caller still surfaces the underlying cause.
func (col *inventoryCollector[K, R, P]) reportFailure(ctx context.Context, logger *zerolog.Logger, cause error) {
	if col.pagesPublished > 0 {
		return
	}
	err := col.publishStatusOnly(ctx, corev1.InventoryStatus_INVENTORY_STATUS_FAILED, cause.Error())
	if err != nil {
		logger.Error().Err(err).Msg("Failed to publish inventory error to Cloud")
	}
}

// inventoryCollector fetches inventory from Core and publishes it to Cloud a page at a time.
// Both page sizes only ever decrease, so each ladder is walked once per run rather than on
// every page.
type inventoryCollector[K any, R any, P any] struct {
	impl          *manageInventoryImpl[K, R, P]
	grpcClient    *cClient.CoreGrpcClient
	workflowName  string
	workflowID    string
	dataConverter converter.DataConverter
	allIDs        []K

	sitePageSize     int
	cloudPageSize    int
	sitePagesFetched int
	pagesPublished   int
	itemsPublished   int
	// itemsMissing counts IDs that FindIDs reported but FindByIDs did not return, which is what
	// a delete arriving between the two calls looks like.
	itemsMissing int
	// pending holds items fetched but not yet published. It never exceeds one page, because the
	// tail stays here until the caller flushes it.
	pending []R
}

// newCollector prepares the per-run state the collector carries across site pages.
func (impl *manageInventoryImpl[K, R, P]) newCollector(grpcClient *cClient.CoreGrpcClient) *inventoryCollector[K, R, P] {
	return &inventoryCollector[K, R, P]{
		impl:          impl,
		grpcClient:    grpcClient,
		workflowName:  fmt.Sprintf("Update%sInventory", impl.itemType),
		workflowID:    fmt.Sprintf("update-%s-inventory-%s", strings.ToLower(impl.itemType), impl.config.SiteID.String()),
		dataConverter: util.NewTemporalDataConverter(),
		// Both loops advance by the page size, so a caller that left either unset would spin
		// forever on empty pages instead of failing.
		sitePageSize:  max(1, impl.config.SitePageSize),
		cloudPageSize: max(1, impl.config.CloudPageSize),
	}
}

// fetchSitePage retrieves one page of items from Core and reports how many IDs it consumed. A
// response larger than the client receive limit comes back as ResourceExhausted, so the page
// size steps down and the same IDs are requested again.
func (col *inventoryCollector[K, R, P]) fetchSitePage(ctx context.Context, logger *zerolog.Logger, ids []K) ([]R, int, error) {
	for {
		pageIDs := ids[:min(col.sitePageSize, len(ids))]
		items, err := col.impl.internalFindByIDs(ctx, col.grpcClient, pageIDs)
		if err == nil {
			col.sitePagesFetched++
			return items, len(pageIDs), nil
		}
		if status.Code(err) != codes.ResourceExhausted || len(pageIDs) <= minSitePageSize {
			return nil, 0, err
		}
		col.sitePageSize = max(minSitePageSize, len(pageIDs)-sitePageSizeStep)
		logger.Warn().Err(err).Int("Site Page Size", col.sitePageSize).
			Msg("Core response exceeded the gRPC receive limit, retrying with a smaller site page")
	}
}

// bufferItems takes one site page of items and publishes whole pages from everything buffered
// so far, always holding at least one item back. Retaining the tail is what lets flush know the
// page carrying it is the last one, which is how Cloud recognizes a complete run. It also fills
// pages across site page boundaries instead of flushing a short page at each one.
func (col *inventoryCollector[K, R, P]) bufferItems(ctx context.Context, logger *zerolog.Logger, items []R) error {
	col.pending = append(col.pending, items...)
	// Strictly greater, so a full buffer still leaves something for flush to carry.
	for len(col.pending) > col.cloudPageSize {
		err := col.publishNextPage(ctx, logger)
		if err != nil {
			return err
		}
	}
	return nil
}

// flush publishes whatever is still buffered once every site page has been fetched, so the last
// page it sends reports itself as the final page.
func (col *inventoryCollector[K, R, P]) flush(ctx context.Context, logger *zerolog.Logger) error {
	for len(col.pending) > 0 {
		err := col.publishNextPage(ctx, logger)
		if err != nil {
			return err
		}
	}

	// Core reported IDs but returned no object for any of them. Publishing the ID list with no
	// items lets Cloud reconcile against a complete run instead of receiving nothing and holding
	// the previous inventory. Every reported ID is present, so nothing gets deleted.
	if col.pagesPublished == 0 {
		return col.publishNextPage(ctx, logger)
	}
	return nil
}

// publishNextPage builds one page from the front of the buffer and sends it to Cloud.
func (col *inventoryCollector[K, R, P]) publishNextPage(ctx context.Context, logger *zerolog.Logger) error {
	page, pageItems, err := col.buildPageWithinBudget(ctx, logger, col.pending)
	if err != nil {
		return err
	}

	logger.Info().Msgf("Publishing inventory page %d to Cloud", col.pagesPublished+1)
	err = col.execute(ctx, page)
	if err != nil {
		logger.Error().Err(err).Int("Cloud Page", col.pagesPublished+1).Msg("Failed to publish inventory to Cloud")
		return err
	}

	col.pagesPublished++
	col.itemsPublished += pageItems
	col.pending = col.pending[pageItems:]
	return nil
}

// buildPageWithinBudget builds the next page from the front of items, stepping the publish page
// size down until the serialized page fits the budget. A single item over the budget is
// published anyway, because dropping it would silently lose inventory.
func (col *inventoryCollector[K, R, P]) buildPageWithinBudget(ctx context.Context, logger *zerolog.Logger, items []R) (P, int, error) {
	pageSize := col.cloudPageSize
	for {
		pageItems := min(pageSize, len(items))
		page, err := col.buildInventoryPage(ctx, items[:pageItems], pageItems)
		if err != nil {
			return page, 0, err
		}

		size, err := col.payloadSize(page)
		if err != nil {
			logger.Error().Err(err).Msg("Failed to measure the size of an inventory page")
			return page, 0, err
		}
		if size <= maxPublishPayloadBytes {
			return page, pageItems, nil
		}
		// At one item there is nothing left to split, and at zero the page is carrying only the
		// ID list, which no page size can shrink. Publishing either anyway beats dropping
		// inventory or looping on a size that cannot come down.
		if pageItems <= 1 {
			logger.Error().Int("Payload Bytes", size).Int("Budget Bytes", maxPublishPayloadBytes).
				Int("Page Items", pageItems).Msg("An inventory page exceeds the publish budget, publishing it anyway")
			return page, pageItems, nil
		}

		// Step down from what this page actually held rather than from the configured size, so a
		// short trailing page reaches a fitting size in one step instead of several.
		pageSize = max(1, pageItems-cloudPageSizeStep)
		// Carry the reduction into the rest of the run only when the page that failed was full.
		// A short tail fragment that does not fit says nothing about whether a full page of
		// typical items would, and lowering the run's size on its account would split every
		// page that follows.
		if pageItems == col.cloudPageSize {
			col.cloudPageSize = pageSize
		}
		logger.Warn().Int("Payload Bytes", size).Int("Page Size", pageSize).
			Msg("Inventory page exceeded the publish budget, rebuilding with fewer items")
	}
}

// buildInventoryPage assembles one page and its paging metadata, then applies whatever post
// processing the resource type asked for.
func (col *inventoryCollector[K, R, P]) buildInventoryPage(ctx context.Context, items []R, pageItems int) (P, error) {
	input := &pagedInventoryInput{
		totalItems:    len(col.allIDs),
		pageSize:      pageItems,
		pageNumber:    col.pagesPublished + 1,
		status:        corev1.InventoryStatus_INVENTORY_STATUS_SUCCESS,
		statusMessage: "Successfully retrieved from Site Controller",
	}
	// The Cloud worker runs its deletion sweep on the page where CurrentPage equals TotalPages, so
	// the total is derived from what is left rather than fixed up front, which the publish page
	// size stepping down would invalidate. This reduces to unfetched IDs plus buffered items, so
	// it reaches zero only on the page that carries the last of both. Discounting itemsMissing is
	// what keeps an item Core never returned from holding the total above the page number.
	remainingItems := input.totalItems - col.itemsMissing - col.itemsPublished - pageItems
	input.totalPages = input.pageNumber + ceilDiv(remainingItems, col.cloudPageSize)

	page := col.impl.internalPagedInventory(col.allIDs, items, input)

	// Handle any requested post processing
	if col.impl.internalPagedInventoryPostProcess != nil {
		return col.impl.internalPagedInventoryPostProcess(ctx, col.grpcClient, page)
	}
	return page, nil
}

// payloadSize reports how many bytes the workflow arguments occupy once serialized, measured
// with the converter the publish client uses so it matches what Temporal receives.
func (col *inventoryCollector[K, R, P]) payloadSize(page P) (int, error) {
	payloads, err := col.dataConverter.ToPayloads(col.impl.config.SiteID, page)
	if err != nil {
		return 0, err
	}
	return proto.Size(payloads), nil
}

// publishStatusOnly publishes a single page carrying a status and no items, used when the Site
// reported nothing and when collection failed before any page went out. It keeps the unpaged
// workflow ID and reports the configured page size, which is what Cloud has always received for
// these two cases.
//
// The failure it reports is often the activity deadline expiring, which would leave the caller's
// context already dead, so this detaches from cancellation and takes its own deadline. Otherwise
// the one case where Cloud most needs to hear that collection failed is the case where the
// message could never be sent.
func (col *inventoryCollector[K, R, P]) publishStatusOnly(ctx context.Context, inventoryStatus corev1.InventoryStatus,
	statusMessage string) error {
	page := col.impl.internalPagedInventory([]K{}, []R{}, &pagedInventoryInput{
		pageSize:      col.cloudPageSize,
		pageNumber:    1,
		status:        inventoryStatus,
		statusMessage: statusMessage,
	})

	ctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), statusPublishTimeout)
	defer cancel()

	_, err := col.impl.config.TemporalPublishClient.ExecuteWorkflow(ctx, tClient.StartWorkflowOptions{
		ID:        col.workflowID,
		TaskQueue: col.impl.config.TemporalPublishQueue,
	}, col.workflowName, col.impl.config.SiteID, page)
	return err
}

// execute starts the Cloud workflow for one published page.
func (col *inventoryCollector[K, R, P]) execute(ctx context.Context, page P) error {
	_, err := col.impl.config.TemporalPublishClient.ExecuteWorkflow(ctx, tClient.StartWorkflowOptions{
		ID:        fmt.Sprintf("%v-%v", col.workflowID, col.pagesPublished+1),
		TaskQueue: col.impl.config.TemporalPublishQueue,
	}, col.workflowName, col.impl.config.SiteID, page)
	return err
}

// ceilDiv divides and rounds up, reporting zero for a non-positive dividend or divisor.
func ceilDiv(dividend, divisor int) int {
	if divisor <= 0 || dividend <= 0 {
		return 0
	}
	return (dividend + divisor - 1) / divisor
}
