// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package nicoapi

import (
	"context"
	"errors"
	"fmt"
	"os"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/certs"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/grpclog"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/common/utils"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/rs/zerolog/log"
	"go.opentelemetry.io/contrib/instrumentation/google.golang.org/grpc/otelgrpc"
	"go.opentelemetry.io/otel"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/emptypb"
	"google.golang.org/protobuf/types/known/timestamppb"
)

const (
	healthProbeIDMaintenance               = "Maintenance"
	classificationSuppressExternalAlerting = "SuppressExternalAlerting"

	// healthProbeIDHostUpdateInProgress is the canonical probe id Core
	// expects in the precondition health alert checked by
	// `trigger_dpu_reprovisioning`. The string is intentionally
	// duplicated rather than imported from Core's Rust constants because
	// Flow does not link against Core's binaries; the string itself is
	// the API contract. See crates/api-model/src/machine_update_module.rs::HOST_UPDATE_HEALTH_PROBE_ID.
	healthProbeIDHostUpdateInProgress = "HostUpdateInProgress"

	// healthReportSourceHostUpdate is the source tag matching Core's
	// HOST_UPDATE_HEALTH_REPORT_SOURCE constant. RemoveMachineHealthReport
	// deletes by (machine_id, source), so this must agree byte-for-byte
	// with Core or the cleanup path leaks the override.
	healthReportSourceHostUpdate = "host-update"

	// classificationPreventAllocations is the alert classification Core
	// requires on the HostUpdateInProgress alert before
	// `trigger_dpu_reprovisioning` will accept the request. Mirrors
	// HealthAlertClassification::prevent_allocations() in
	// crates/health-report/src/lib.rs.
	classificationPreventAllocations = "PreventAllocations"

	// flowFindByIDsBatchSize bounds the number of full protobuf resources Flow
	// retains in one detail-lookup batch. Core's max_find_by_ids remains the
	// authoritative server maximum; Flow uses the smaller non-zero limit. A
	// zero Core value means the server is unlimited, not that Flow should build
	// one unbounded response.
	flowFindByIDsBatchSize = 100

	// coreGRPCMaxRecvMsgSize raises the Go gRPC 4 MiB default for Core responses.
	// Core's expected-inventory RPCs return complete snapshots in one response,
	// which can exceed that default for larger sites.
	coreGRPCMaxRecvMsgSize = 32 * 1024 * 1024
)

type grpcClient struct {
	gclient     *batchingForgeClient
	grpcTimeout time.Duration
}

// batchingForgeClient keeps limit handling below the Flow client methods so
// direct ForgeClient calls use the same batching behavior as the convenience
// methods in this package.
type batchingForgeClient struct {
	corev1.ForgeClient

	maxFindByIDsMu     sync.Mutex
	maxFindByIDs       uint32
	maxFindByIDsLoaded bool
}

func newBatchingForgeClient(client corev1.ForgeClient) *batchingForgeClient {
	return &batchingForgeClient{ForgeClient: client}
}

func (c *batchingForgeClient) FindMachinesByIds(
	ctx context.Context,
	request *corev1.MachinesByIdsRequest,
	options ...grpc.CallOption,
) (*corev1.MachineList, error) {
	machines := make([]*corev1.Machine, 0, len(request.GetMachineIds()))
	err := c.visitMachineBatches(ctx, request, func(batch []*corev1.Machine) error {
		machines = append(machines, batch...)
		return nil
	}, options...)
	if err != nil {
		return nil, err
	}
	return &corev1.MachineList{Machines: machines}, nil
}

func (c *batchingForgeClient) FindSwitchesByIds(
	ctx context.Context,
	request *corev1.SwitchesByIdsRequest,
	options ...grpc.CallOption,
) (*corev1.SwitchList, error) {
	switches := make([]*corev1.Switch, 0, len(request.GetSwitchIds()))
	err := c.visitSwitchBatches(ctx, request, func(batch []*corev1.Switch) error {
		switches = append(switches, batch...)
		return nil
	}, options...)
	if err != nil {
		return nil, err
	}
	return &corev1.SwitchList{Switches: switches}, nil
}

func (c *batchingForgeClient) FindPowerShelvesByIds(
	ctx context.Context,
	request *corev1.PowerShelvesByIdsRequest,
	options ...grpc.CallOption,
) (*corev1.PowerShelfList, error) {
	shelves := make([]*corev1.PowerShelf, 0, len(request.GetPowerShelfIds()))
	err := c.visitPowerShelfBatches(ctx, request, func(batch []*corev1.PowerShelf) error {
		shelves = append(shelves, batch...)
		return nil
	}, options...)
	if err != nil {
		return nil, err
	}
	return &corev1.PowerShelfList{PowerShelves: shelves}, nil
}

func (c *batchingForgeClient) visitMachineBatches(
	ctx context.Context,
	request *corev1.MachinesByIdsRequest,
	visit func([]*corev1.Machine) error,
	options ...grpc.CallOption,
) error {
	return visitFindByIDBatches(ctx, "FindMachinesByIds", c.loadMaxFindByIDs, protoIDsToStrings(request.GetMachineIds()),
		func(ctx context.Context, batch []string) ([]*corev1.Machine, error) {
			return c.fetchMachinesByIDs(ctx, request, batch, options...)
		}, func(machine *corev1.Machine) string {
			return machine.GetId().GetId()
		}, visit)
}

func (c *batchingForgeClient) visitSwitchBatches(
	ctx context.Context,
	request *corev1.SwitchesByIdsRequest,
	visit func([]*corev1.Switch) error,
	options ...grpc.CallOption,
) error {
	return visitFindByIDBatches(ctx, "FindSwitchesByIds", c.loadMaxFindByIDs, protoIDsToStrings(request.GetSwitchIds()),
		func(ctx context.Context, batch []string) ([]*corev1.Switch, error) {
			return c.fetchSwitchesByIDs(ctx, request, batch, options...)
		}, func(sw *corev1.Switch) string {
			return sw.GetId().GetId()
		}, visit)
}

func (c *batchingForgeClient) visitPowerShelfBatches(
	ctx context.Context,
	request *corev1.PowerShelvesByIdsRequest,
	visit func([]*corev1.PowerShelf) error,
	options ...grpc.CallOption,
) error {
	return visitFindByIDBatches(ctx, "FindPowerShelvesByIds", c.loadMaxFindByIDs, protoIDsToStrings(request.GetPowerShelfIds()),
		func(ctx context.Context, batch []string) ([]*corev1.PowerShelf, error) {
			return c.fetchPowerShelvesByIDs(ctx, request, batch, options...)
		}, func(shelf *corev1.PowerShelf) string {
			return shelf.GetId().GetId()
		}, visit)
}

var testingMsgOnce sync.Once

func coreGRPCDialOptions(transportCredentials credentials.TransportCredentials) []grpc.DialOption {
	return []grpc.DialOption{
		grpc.WithTransportCredentials(transportCredentials),
		grpc.WithDefaultCallOptions(grpc.MaxCallRecvMsgSize(coreGRPCMaxRecvMsgSize)),
		grpc.WithStatsHandler(otelgrpc.NewClientHandler(otelgrpc.WithPropagators(otel.GetTextMapPropagator()))),
		grpc.WithChainUnaryInterceptor(grpclog.UnaryClientInterceptor("nico-core-api")),
	}
}

// NewClient creates a GRPC connection pool to nico-core-api.  Returning success does not mean that we have yet made an actual connection;
// that happens when making an actual request.
func NewClient(grpcTimeout time.Duration) (Client, error) {
	if testing.Testing() {
		testingMsgOnce.Do(func() {
			log.Info().Msg("Running unit tests, forcing mock GRPC client")
		})
		return NewMockClient(), nil
	}

	nicoURL := os.Getenv("NICO_CORE_API_URL")
	if nicoURL == "" {
		return nil, errors.New("NICO_CORE_API_URL not set, cannot make connections to NICo Core")
	}

	tlsConfig, _, err := certs.TLSConfig()
	if err != nil {
		if err == certs.ErrNotPresent {
			return nil, errors.New("Certificates not present, unable to authenticate with nico-core-api")
		}
		return nil, err
	}

	conn, err := grpc.NewClient(nicoURL, coreGRPCDialOptions(credentials.NewTLS(tlsConfig))...)
	if err != nil {
		return nil, fmt.Errorf("Unable to connect to nico-core-api: %w", err)
	}

	return &grpcClient{
		gclient:     newBatchingForgeClient(corev1.NewForgeClient(conn)),
		grpcTimeout: grpcTimeout,
	}, nil
}

// GetMachines retrieves all machines known by nico-core-api
// (FindMachineIds + FindMachinesByIds).
func (c *grpcClient) GetMachines(ctx context.Context) ([]MachineDetail, error) {
	idsCtx, idsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	machineIDs, err := c.gclient.FindMachineIds(idsCtx, &corev1.MachineSearchConfig{})
	idsCancel()
	if err != nil {
		return nil, err
	}

	req := &corev1.MachinesByIdsRequest{}
	for _, machineID := range machineIDs.MachineIds {
		req.MachineIds = append(req.MachineIds, machineID)
	}

	if len(req.MachineIds) == 0 {
		return nil, nil
	}

	detailsCtx, detailsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer detailsCancel()
	result := make([]MachineDetail, 0, len(req.MachineIds))
	if err := c.gclient.visitMachineBatches(detailsCtx, req, func(batch []*corev1.Machine) error {
		for _, machine := range batch {
			result = append(result, machineDetailFromPb(machine))
		}
		return nil
	}); err != nil {
		return nil, err
	}
	return result, nil
}

// GetSwitches retrieves a complete active-switch snapshot. ID discovery and
// detail lookup each receive the configured RPC timeout so a slow first call
// cannot starve the second call. FindSwitchesByIds is routed through the shared
// batching client, which also verifies response completeness.
func (c *grpcClient) GetSwitches(ctx context.Context) ([]ObservedControllerDevice, error) {
	idsCtx, idsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	idsResponse, err := c.gclient.FindSwitchIds(idsCtx, &corev1.SwitchSearchFilter{})
	idsCancel()
	if err != nil {
		return nil, fmt.Errorf("FindSwitchIds for actual inventory: %w", err)
	}

	switchIDs := idsResponse.GetIds()
	if len(switchIDs) == 0 {
		return []ObservedControllerDevice{}, nil
	}

	detailsCtx, detailsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer detailsCancel()
	request := &corev1.SwitchesByIdsRequest{
		SwitchIds: switchIDs,
	}
	result := make([]ObservedControllerDevice, 0, len(switchIDs))
	if err := c.gclient.visitSwitchBatches(detailsCtx, request, func(batch []*corev1.Switch) error {
		for _, sw := range batch {
			result = append(result, ObservedControllerDevice{
				ID:     sw.GetId().GetId(),
				BmcMac: sw.GetBmcInfo().GetMac(),
			})
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindSwitchesByIds for actual inventory: %w", err)
	}
	return result, nil
}

// GetPowerShelves is the power-shelf equivalent of GetSwitches, including the
// independent per-RPC timeout and complete batched detail lookup.
func (c *grpcClient) GetPowerShelves(ctx context.Context) ([]ObservedControllerDevice, error) {
	idsCtx, idsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	idsResponse, err := c.gclient.FindPowerShelfIds(idsCtx, &corev1.PowerShelfSearchFilter{})
	idsCancel()
	if err != nil {
		return nil, fmt.Errorf("FindPowerShelfIds for actual inventory: %w", err)
	}

	shelfIDs := idsResponse.GetIds()
	if len(shelfIDs) == 0 {
		return []ObservedControllerDevice{}, nil
	}

	detailsCtx, detailsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer detailsCancel()
	request := &corev1.PowerShelvesByIdsRequest{
		PowerShelfIds: shelfIDs,
	}
	result := make([]ObservedControllerDevice, 0, len(shelfIDs))
	if err := c.gclient.visitPowerShelfBatches(detailsCtx, request, func(batch []*corev1.PowerShelf) error {
		for _, shelf := range batch {
			result = append(result, ObservedControllerDevice{
				ID:     shelf.GetId().GetId(),
				BmcMac: shelf.GetBmcInfo().GetMac(),
			})
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindPowerShelvesByIds for actual inventory: %w", err)
	}
	return result, nil
}

// GetLeakingMachineIds retrieves IDs of all machines which are leaking and are powered on.
// The search filter passed in to FindMachineIds limits the results to these two conditions.
func (c *grpcClient) GetLeakingMachineIds(ctx context.Context) ([]string, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	alert := "hardware-health.tray-leak-detection"
	powerState := "on"
	searchConfig := corev1.MachineSearchConfig{
		OnlyWithHealthAlert: &alert,
		OnlyWithPowerState:  &powerState,
	}

	machineIDs, err := c.gclient.FindMachineIds(ctx, &searchConfig)
	if err != nil {
		return nil, err
	}

	ids := make([]string, 0, len(machineIDs.GetMachineIds()))
	for _, machineID := range machineIDs.GetMachineIds() {
		ids = append(ids, machineID.GetId())
	}
	return ids, nil
}

// GetLeakingSwitchIds retrieves IDs of all switches which are leaking.
// The search filter passed in to FindSwitchIds limits the results to this condition.
// Once we have the ability to limit the results to powered on switches,
// we can add that condition to the search filter.
func (c *grpcClient) GetLeakingSwitchIds(ctx context.Context) ([]string, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	alert := "hardware-health.tray-leak-detection"
	searchConfig := corev1.SwitchSearchFilter{
		OnlyWithHealthAlert: &alert,
	}

	switchIDs, err := c.gclient.FindSwitchIds(ctx, &searchConfig)
	if err != nil {
		return nil, err
	}

	ids := make([]string, 0, len(switchIDs.GetIds()))
	for _, switchID := range switchIDs.GetIds() {
		ids = append(ids, switchID.GetId())
	}
	return ids, nil
}

// Version returns the version string of nico-core-api, mainly as a "ping"
func (c *grpcClient) Version(ctx context.Context) (string, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	res, err := c.gclient.Version(ctx, &corev1.VersionRequest{})
	if err != nil {
		return "", err
	}
	return res.GetBuildVersion(), nil
}

// loadMaxFindByIDs lazily loads and caches Core's effective request limit.
// Failed loads are not cached, so a transient Version failure can recover on a
// later lookup. The mutex also coalesces concurrent first loads into one RPC.
func (c *batchingForgeClient) loadMaxFindByIDs(ctx context.Context) (uint32, error) {
	c.maxFindByIDsMu.Lock()
	defer c.maxFindByIDsMu.Unlock()

	if c.maxFindByIDsLoaded {
		return c.maxFindByIDs, nil
	}

	response, err := c.ForgeClient.Version(ctx, &corev1.VersionRequest{DisplayConfig: true})
	if err != nil {
		return 0, fmt.Errorf("get Core runtime config: %w", err)
	}
	c.maxFindByIDs = response.GetRuntimeConfig().GetMaxFindByIds()
	c.maxFindByIDsLoaded = true
	return c.maxFindByIDs, nil
}

// visitFindByIDBatches loads the shared server limit and visits each complete
// batch before fetching the next one. This lets callers project full protobuf
// resources into their smaller result shape without retaining earlier batches.
// The caller's context covers limit discovery and all batch RPCs.
func visitFindByIDBatches[T any](
	ctx context.Context,
	rpcName string,
	loadLimit func(context.Context) (uint32, error),
	ids []string,
	fetch func(context.Context, []string) ([]T, error),
	identity func(T) string,
	visit func([]T) error,
) error {
	if len(ids) == 0 {
		return nil
	}
	if err := validateByIDsRequest(ids, rpcName); err != nil {
		return err
	}

	limit, err := loadLimit(ctx)
	if err != nil {
		return err
	}

	batchSize := min(len(ids), flowFindByIDsBatchSize)
	if limit > 0 && uint64(limit) < uint64(batchSize) {
		batchSize = int(limit)
	}

	for batch := range slices.Chunk(ids, batchSize) {
		values, err := fetch(ctx, batch)
		if err != nil {
			return err
		}
		returnedIDs := make([]string, 0, len(values))
		for _, value := range values {
			returnedIDs = append(returnedIDs, identity(value))
		}
		if err := validateByIDsResponse(batch, returnedIDs, rpcName); err != nil {
			return err
		}
		if err := visit(values); err != nil {
			return err
		}
	}
	return nil
}

// validateByIDsRequest rejects identities that cannot form an exact result set.
func validateByIDsRequest(ids []string, rpcName string) error {
	seen := make(map[string]struct{}, len(ids))
	for _, id := range ids {
		if id == "" {
			return fmt.Errorf("%s request contains an empty ID", rpcName)
		}
		if _, ok := seen[id]; ok {
			return fmt.Errorf("%s request contains duplicate ID: %s", rpcName, id)
		}
		seen[id] = struct{}{}
	}
	return nil
}

// validateByIDsResponse requires the response identities to exactly match the request.
func validateByIDsResponse(requested, returned []string, rpcName string) error {
	requestedSet := make(map[string]struct{}, len(requested))
	for _, id := range requested {
		requestedSet[id] = struct{}{}
	}

	returnedSet := make(map[string]struct{}, len(returned))
	for _, id := range returned {
		if id == "" {
			return fmt.Errorf("%s returned an empty ID", rpcName)
		}
		if _, ok := requestedSet[id]; !ok {
			return fmt.Errorf("%s returned unrequested ID: %s", rpcName, id)
		}
		if _, ok := returnedSet[id]; ok {
			return fmt.Errorf("%s returned duplicate ID: %s", rpcName, id)
		}
		returnedSet[id] = struct{}{}
	}

	missing := make([]string, 0)
	for _, id := range requested {
		if _, ok := returnedSet[id]; !ok {
			missing = append(missing, id)
		}
	}
	if len(missing) > 0 {
		return fmt.Errorf("%s returned an incomplete response; missing IDs: %s", rpcName, strings.Join(missing, ", "))
	}
	return nil
}

// protoID is the common generated-protobuf ID contract used by Core resources.
type protoID interface {
	GetId() string
}

// protoIDsToStrings extracts ID values while preserving request order.
func protoIDsToStrings[T protoID](ids []T) []string {
	result := make([]string, 0, len(ids))
	for _, id := range ids {
		result = append(result, id.GetId())
	}
	return result
}

// fetchMachinesByIDs clones the caller's request for one raw Core batch.
func (c *batchingForgeClient) fetchMachinesByIDs(
	ctx context.Context,
	request *corev1.MachinesByIdsRequest,
	batch []string,
	options ...grpc.CallOption,
) ([]*corev1.Machine, error) {
	batchRequest := proto.Clone(request).(*corev1.MachinesByIdsRequest)
	batchRequest.MachineIds = stringsToMachineIds(batch)
	response, err := c.ForgeClient.FindMachinesByIds(ctx, batchRequest, options...)
	if err != nil {
		return nil, fmt.Errorf("FindMachinesByIds: %w", err)
	}

	return response.GetMachines(), nil
}

// GetPowerStates returns the power states of the given machines (all machines if given an empty machineIds)
func (c *grpcClient) GetPowerStates(ctx context.Context, machineIds []string) (ret []MachinePowerState, err error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.PowerOptionRequest{MachineId: stringsToMachineIds(machineIds)}
	res, err := c.gclient.GetPowerOptions(ctx, req)
	if err != nil {
		return nil, err
	}
	for _, cur := range res.Response {
		ret = append(ret, machinePowerStateFromPb(cur))
	}

	return ret, nil
}

// SetFirmwareUpdateTimeWindow sets the firmware update time window for the given machines
func (c *grpcClient) SetFirmwareUpdateTimeWindow(ctx context.Context, machineIds []string, startTime, endTime time.Time) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.SetFirmwareUpdateTimeWindowRequest{
		MachineIds:     stringsToMachineIds(machineIds),
		StartTimestamp: timestamppb.New(startTime),
		EndTimestamp:   timestamppb.New(endTime),
	}

	_, err := c.gclient.SetFirmwareUpdateTimeWindow(ctx, req)
	if err != nil {
		return fmt.Errorf("failed to set firmware update time window: %w", err)
	}

	return nil
}

// AdminPowerControl performs power control operations on a machine
func (c *grpcClient) AdminPowerControl(ctx context.Context, machineID string, action SystemPowerControl) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.AdminPowerControlRequest{
		MachineId: &machineID,
		Action:    action.toPb(),
	}

	_, err := c.gclient.AdminPowerControl(ctx, req)
	if err != nil {
		return fmt.Errorf("failed to perform power control on machine %s: %w", machineID, err)
	}

	return nil
}

// UpdatePowerOption sets the desired power state for a machine in NICo's power manager.
func (c *grpcClient) UpdatePowerOption(ctx context.Context, machineID string, desiredState PowerState) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.PowerOptionUpdateRequest{
		MachineId:  &corev1.MachineId{Id: machineID},
		PowerState: powerStateToPb(desiredState),
	}

	_, err := c.gclient.UpdatePowerOption(ctx, req)
	if err != nil {
		return fmt.Errorf("failed to update power option for machine %s: %w", machineID, err)
	}

	return nil
}

// FindInterfaces returns all machine interfaces known by nico-core-api, keyed by MAC address
func (c *grpcClient) FindInterfaces(ctx context.Context) (map[string]MachineInterface, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	// Empty query returns all interfaces
	req := &corev1.InterfaceSearchQuery{}
	res, err := c.gclient.FindInterfaces(ctx, req)
	if err != nil {
		return nil, err
	}

	interfaces := make(map[string]MachineInterface)
	for _, iface := range res.Interfaces {
		mi := machineInterfaceFromPb(iface)
		interfaces[utils.NormalizeMAC(mi.MacAddress)] = mi
	}
	return interfaces, nil
}

// FindMachinesByIds returns detailed machine information for the given machine IDs
func (c *grpcClient) FindMachinesByIds(ctx context.Context, machineIds []string) ([]MachineDetail, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	if len(machineIds) == 0 {
		return nil, nil
	}

	req := &corev1.MachinesByIdsRequest{
		MachineIds: stringsToMachineIds(machineIds),
	}

	result := make([]MachineDetail, 0, len(machineIds))
	if err := c.gclient.visitMachineBatches(ctx, req, func(batch []*corev1.Machine) error {
		for _, machine := range batch {
			result = append(result, machineDetailFromPb(machine))
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("failed to find machines by IDs: %w", err)
	}
	return result, nil
}

// FindHostMachineIdsByRack queries Core for host machines (DPUs excluded) on
// the given rack and returns their machine IDs.
func (c *grpcClient) FindHostMachineIdsByRack(ctx context.Context, rackID string) ([]string, error) {
	if rackID == "" {
		return nil, errors.New("rack ID is required")
	}

	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	cfg := &corev1.MachineSearchConfig{
		RackId: &corev1.RackId{Id: rackID},
		// include_dpus defaults to false; exclude_hosts defaults to false.
		// We want hosts only because Assigned is a host-only state.
	}

	res, err := c.gclient.FindMachineIds(ctx, cfg)
	if err != nil {
		return nil, fmt.Errorf("FindMachineIds for rack %s: %w", rackID, err)
	}

	ids := make([]string, 0, len(res.GetMachineIds()))
	for _, mid := range res.GetMachineIds() {
		if id := mid.GetId(); id != "" {
			ids = append(ids, id)
		}
	}
	return ids, nil
}

// fetchSwitchesByIDs clones the caller's request for one raw Core batch and
// verifies that Core returned every requested switch.
func (c *batchingForgeClient) fetchSwitchesByIDs(
	ctx context.Context,
	request *corev1.SwitchesByIdsRequest,
	batch []string,
	options ...grpc.CallOption,
) ([]*corev1.Switch, error) {
	batchRequest := proto.Clone(request).(*corev1.SwitchesByIdsRequest)
	batchRequest.SwitchIds = make([]*corev1.SwitchId, 0, len(batch))
	for _, id := range batch {
		batchRequest.SwitchIds = append(batchRequest.SwitchIds, &corev1.SwitchId{Id: id})
	}

	response, err := c.ForgeClient.FindSwitchesByIds(ctx, batchRequest, options...)
	if err != nil {
		return nil, fmt.Errorf("FindSwitchesByIds: %w", err)
	}

	return response.GetSwitches(), nil
}

// fetchPowerShelvesByIDs clones the caller's request for one raw Core batch and
// verifies that Core returned every requested power shelf.
func (c *batchingForgeClient) fetchPowerShelvesByIDs(
	ctx context.Context,
	request *corev1.PowerShelvesByIdsRequest,
	batch []string,
	options ...grpc.CallOption,
) ([]*corev1.PowerShelf, error) {
	batchRequest := proto.Clone(request).(*corev1.PowerShelvesByIdsRequest)
	batchRequest.PowerShelfIds = make([]*corev1.PowerShelfId, 0, len(batch))
	for _, id := range batch {
		batchRequest.PowerShelfIds = append(batchRequest.PowerShelfIds, &corev1.PowerShelfId{Id: id})
	}

	response, err := c.ForgeClient.FindPowerShelvesByIds(ctx, batchRequest, options...)
	if err != nil {
		return nil, fmt.Errorf("FindPowerShelvesByIds: %w", err)
	}

	return response.GetPowerShelves(), nil
}

// FindSwitchRackIDs returns the rack assignment of each given switch.
func (c *grpcClient) FindSwitchRackIDs(ctx context.Context, switchIds []string) (map[string]string, error) {
	if len(switchIds) == 0 {
		return nil, nil
	}

	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.SwitchesByIdsRequest{
		SwitchIds: make([]*corev1.SwitchId, 0, len(switchIds)),
	}
	for _, id := range switchIds {
		req.SwitchIds = append(req.SwitchIds, &corev1.SwitchId{Id: id})
	}

	result := make(map[string]string, len(switchIds))
	if err := c.gclient.visitSwitchBatches(ctx, req, func(batch []*corev1.Switch) error {
		for _, sw := range batch {
			sid := sw.GetId().GetId()
			if sid == "" {
				continue
			}
			if rid := sw.GetRackId().GetId(); rid != "" {
				result[sid] = rid
			}
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindSwitchesByIds: %w", err)
	}
	return result, nil
}

// FindSwitchControllerStates returns the raw controller_state string Core
// reports for each switch. Switches without a controller_state (e.g. legacy
// rows or transient errors) are simply omitted from the result map.
func (c *grpcClient) FindSwitchControllerStates(ctx context.Context, switchIds []string) (map[string]string, error) {
	if len(switchIds) == 0 {
		return nil, nil
	}

	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.SwitchesByIdsRequest{
		SwitchIds: make([]*corev1.SwitchId, 0, len(switchIds)),
	}
	for _, id := range switchIds {
		req.SwitchIds = append(req.SwitchIds, &corev1.SwitchId{Id: id})
	}

	result := make(map[string]string, len(switchIds))
	if err := c.gclient.visitSwitchBatches(ctx, req, func(batch []*corev1.Switch) error {
		for _, sw := range batch {
			sid := sw.GetId().GetId()
			if sid == "" {
				continue
			}
			if state := sw.GetControllerState(); state != "" {
				result[sid] = state
			}
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindSwitchesByIds: %w", err)
	}
	return result, nil
}

// FindSwitchNvosIPs returns the resolved NVOS host IP for each given switch,
// keyed by Core SwitchId. Core resolves nvos_info from the expected switch's
// NVOS MAC and its assigned interface address, and only populates it once both
// are known, so switches without a resolved NVOS endpoint are omitted.
func (c *grpcClient) FindSwitchNvosIPs(ctx context.Context, switchIds []string) (map[string]string, error) {
	if len(switchIds) == 0 {
		return nil, nil
	}

	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.SwitchesByIdsRequest{
		SwitchIds: make([]*corev1.SwitchId, 0, len(switchIds)),
	}
	for _, id := range switchIds {
		req.SwitchIds = append(req.SwitchIds, &corev1.SwitchId{Id: id})
	}

	result := make(map[string]string, len(switchIds))
	if err := c.gclient.visitSwitchBatches(ctx, req, func(batch []*corev1.Switch) error {
		for _, sw := range batch {
			sid := sw.GetId().GetId()
			if sid == "" {
				continue
			}
			if ip := sw.GetNvosInfo().GetIp(); ip != "" {
				result[sid] = ip
			}
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindSwitchesByIds: %w", err)
	}
	return result, nil
}

// GetObservedNVLinkDomainMemberships returns valid rack/domain observations
// from a complete snapshot of Core's active switches. FindSwitchIds excludes
// deleted switches by default; the details response must include every
// requested switch so callers can safely reconcile omitted memberships.
func (c *grpcClient) GetObservedNVLinkDomainMemberships(ctx context.Context) ([]NVLinkDomainMembership, error) {
	idsCtx, idsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	idsResponse, err := c.gclient.FindSwitchIds(idsCtx, &corev1.SwitchSearchFilter{})
	idsCancel()
	if err != nil {
		return nil, fmt.Errorf("FindSwitchIds for NVLink domain topology: %w", err)
	}

	switchIDs := idsResponse.GetIds()
	if len(switchIDs) == 0 {
		return []NVLinkDomainMembership{}, nil
	}

	detailsCtx, detailsCancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer detailsCancel()
	request := &corev1.SwitchesByIdsRequest{
		SwitchIds: switchIDs,
	}
	memberships := make([]NVLinkDomainMembership, 0, len(switchIDs))
	if err := c.gclient.visitSwitchBatches(detailsCtx, request, func(batch []*corev1.Switch) error {
		projected, err := nvLinkDomainMembershipsFromSwitches(nil, batch)
		if err != nil {
			return err
		}
		memberships = append(memberships, projected...)
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindSwitchesByIds for NVLink domain topology: %w", err)
	}

	return memberships, nil
}

func nvLinkDomainMembershipsFromSwitches(
	switchIDs []*corev1.SwitchId,
	switches []*corev1.Switch,
) ([]NVLinkDomainMembership, error) {
	requested := make(map[string]struct{}, len(switchIDs))
	for _, id := range switchIDs {
		value := id.GetId()
		if value != "" {
			requested[value] = struct{}{}
		}
	}

	seen := make(map[string]struct{}, len(switches))
	memberships := make([]NVLinkDomainMembership, 0, len(switches))
	for _, sw := range switches {
		switchID := sw.GetId().GetId()
		if switchID == "" {
			continue
		}
		seen[switchID] = struct{}{}

		domainID := sw.GetNvlinkDomainUuid().GetValue()
		rackID := sw.GetRackId().GetId()
		if domainID == "" || rackID == "" {
			continue
		}
		memberships = append(memberships, NVLinkDomainMembership{
			DomainID: domainID,
			RackID:   rackID,
		})
	}

	for switchID := range requested {
		_, ok := seen[switchID]
		if !ok {
			return nil, fmt.Errorf("FindSwitchesByIds omitted active switch %s", switchID)
		}
	}

	return memberships, nil
}

// FindPowerShelfRackIDs returns the rack assignment of each given power shelf.
func (c *grpcClient) FindPowerShelfRackIDs(ctx context.Context, shelfIds []string) (map[string]string, error) {
	if len(shelfIds) == 0 {
		return nil, nil
	}

	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.PowerShelvesByIdsRequest{
		PowerShelfIds: make([]*corev1.PowerShelfId, 0, len(shelfIds)),
	}
	for _, id := range shelfIds {
		req.PowerShelfIds = append(req.PowerShelfIds, &corev1.PowerShelfId{Id: id})
	}

	result := make(map[string]string, len(shelfIds))
	if err := c.gclient.visitPowerShelfBatches(ctx, req, func(batch []*corev1.PowerShelf) error {
		for _, shelf := range batch {
			pid := shelf.GetId().GetId()
			if pid == "" {
				continue
			}
			if rid := shelf.GetRackId().GetId(); rid != "" {
				result[pid] = rid
			}
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindPowerShelvesByIds: %w", err)
	}
	return result, nil
}

// FindPowerShelfControllerStates returns the raw controller_state string Core
// reports for each power shelf. Shelves without a controller_state are
// omitted from the result map.
func (c *grpcClient) FindPowerShelfControllerStates(ctx context.Context, shelfIds []string) (map[string]string, error) {
	if len(shelfIds) == 0 {
		return nil, nil
	}

	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.PowerShelvesByIdsRequest{
		PowerShelfIds: make([]*corev1.PowerShelfId, 0, len(shelfIds)),
	}
	for _, id := range shelfIds {
		req.PowerShelfIds = append(req.PowerShelfIds, &corev1.PowerShelfId{Id: id})
	}

	result := make(map[string]string, len(shelfIds))
	if err := c.gclient.visitPowerShelfBatches(ctx, req, func(batch []*corev1.PowerShelf) error {
		for _, shelf := range batch {
			pid := shelf.GetId().GetId()
			if pid == "" {
				continue
			}
			if state := shelf.GetControllerState(); state != "" {
				result[pid] = state
			}
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("FindPowerShelvesByIds: %w", err)
	}
	return result, nil
}

// GetMachinePositionInfo returns position information for the given machine IDs
func (c *grpcClient) GetMachinePositionInfo(ctx context.Context, machineIds []string) ([]MachinePosition, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	if len(machineIds) == 0 {
		return nil, nil
	}

	req := &corev1.MachinePositionQuery{
		MachineIds: stringsToMachineIds(machineIds),
	}

	res, err := c.gclient.GetMachinePositionInfo(ctx, req)
	if err != nil {
		return nil, fmt.Errorf("failed to get machine position info: %w", err)
	}

	var result []MachinePosition
	for _, pos := range res.MachinePositionInfo {
		result = append(result, machinePositionFromPb(pos))
	}
	return result, nil
}

// FindMachineControllerStates returns the raw state string Core reports for
// each machine. Machines for which Core returns no state are omitted from
// the result map.
func (c *grpcClient) FindMachineControllerStates(ctx context.Context, machineIds []string) (map[string]string, error) {
	if len(machineIds) == 0 {
		return nil, nil
	}

	machines, err := c.FindMachinesByIds(ctx, machineIds)
	if err != nil {
		return nil, fmt.Errorf("FindMachinesByIds: %w", err)
	}

	result := make(map[string]string, len(machines))
	for _, m := range machines {
		if m.State != "" {
			result[m.MachineID] = m.State
		}
	}
	return result, nil
}

// DecommissionMachine initiates decommissioning of the given machine via Core.
func (c *grpcClient) DecommissionMachine(ctx context.Context, machineID string) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	_, err := c.gclient.DecommissionManagedHost(ctx, &corev1.DecommissionManagedHostRequest{
		MachineId: &corev1.MachineId{Id: machineID},
	})
	if err != nil {
		return fmt.Errorf("decommission machine %s: %w", machineID, err)
	}
	return nil
}

// DecommissionSwitch initiates decommissioning of the given switch via Core.
func (c *grpcClient) DecommissionSwitch(ctx context.Context, switchID string) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	_, err := c.gclient.DecommissionSwitch(ctx, &corev1.DecommissionSwitchRequest{
		SwitchId: &corev1.SwitchId{Id: switchID},
	})
	if err != nil {
		return fmt.Errorf("decommission switch %s: %w", switchID, err)
	}
	return nil
}

// DecommissionPowerShelf initiates decommissioning of the given power shelf via Core.
func (c *grpcClient) DecommissionPowerShelf(ctx context.Context, shelfID string) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	_, err := c.gclient.DecommissionPowerShelf(ctx, &corev1.DecommissionPowerShelfRequest{
		PowerShelfId: &corev1.PowerShelfId{Id: shelfID},
	})
	if err != nil {
		return fmt.Errorf("failed to decommission power shelf %s: %w", shelfID, err)
	}

	return nil
}

// AllowIngestionAndPowerOn opens NICo's power-on gate for a
// BMC endpoint.
func (c *grpcClient) AllowIngestionAndPowerOn(
	ctx context.Context,
	bmcIP string,
	bmcMAC string,
) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.BmcEndpointRequest{IpAddress: bmcIP}
	if bmcMAC != "" {
		req.MacAddress = &bmcMAC
	}

	_, err := c.gclient.AllowIngestionAndPowerOn(ctx, req)
	if err != nil {
		return fmt.Errorf(
			"failed to allow ingestion for BMC %s: %w",
			bmcIP, err,
		)
	}

	return nil
}

// DetermineMachineIngestionState queries the ingestion state of
// a machine relative to NICo's power-on gate.
func (c *grpcClient) DetermineMachineIngestionState(
	ctx context.Context,
	bmcIP string,
	bmcMAC string,
) (BringUpState, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.BmcEndpointRequest{IpAddress: bmcIP}
	if bmcMAC != "" {
		req.MacAddress = &bmcMAC
	}

	resp, err := c.gclient.DetermineMachineIngestionState(
		ctx, req,
	)
	if err != nil {
		return BringUpStateNotDiscovered, fmt.Errorf(
			"failed to get bring-up state for BMC %s: %w", //nolint
			bmcIP, err,
		)
	}

	return bringUpStateFromPb(
		resp.GetMachineIngestionState(),
	), nil
}

// AddExpectedMachine registers an expected machine with NICo.
func (c *grpcClient) AddExpectedMachine(ctx context.Context, req AddExpectedMachineRequest) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	pbReq := &corev1.ExpectedMachine{
		BmcMacAddress:       req.BMCMACAddress,
		BmcUsername:         req.BMCUsername,
		BmcPassword:         req.BMCPassword,
		ChassisSerialNumber: req.ChassisSerialNumber,
	}

	if len(req.FallbackDPUSerialNumbers) > 0 {
		pbReq.FallbackDpuSerialNumbers = req.FallbackDPUSerialNumbers
	}

	if req.RackID != "" {
		pbReq.RackId = &corev1.RackId{Id: req.RackID}
	}

	if req.PauseIngestionAndPowerOn != nil {
		pbReq.DefaultPauseIngestionAndPoweron = req.PauseIngestionAndPowerOn
	}

	_, err := c.gclient.AddExpectedMachine(ctx, pbReq)
	if err != nil {
		return fmt.Errorf("failed to add expected machine (bmc_mac=%s): %w", req.BMCMACAddress, err)
	}

	return nil
}

// GetAllExpectedSwitches returns all expected switches from NICo, keyed by BMC MAC address.
func (c *grpcClient) GetAllExpectedSwitches(ctx context.Context) (map[string]ExpectedSwitchInfo, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetAllExpectedSwitches(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("failed to get all expected switches: %w", err)
	}

	results := make(map[string]ExpectedSwitchInfo)
	for _, es := range resp.GetExpectedSwitches() {
		info := expectedSwitchInfoFromPb(es)
		if info.BMCMACAddress != "" {
			results[utils.NormalizeMAC(info.BMCMACAddress)] = info
		}
	}
	return results, nil
}

// AddExpectedSwitch registers an expected switch with NICo.
func (c *grpcClient) AddExpectedSwitch(ctx context.Context, req AddExpectedSwitchRequest) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	pbReq := &corev1.ExpectedSwitch{
		BmcMacAddress:      req.BMCMACAddress,
		BmcUsername:        req.BMCUsername,
		BmcPassword:        req.BMCPassword,
		SwitchSerialNumber: req.SwitchSerialNumber,
	}

	if req.RackID != "" {
		pbReq.RackId = &corev1.RackId{Id: req.RackID}
	}

	if req.NVOSUsername != "" {
		pbReq.NvosUsername = &req.NVOSUsername
	}

	if req.NVOSPassword != "" {
		pbReq.NvosPassword = &req.NVOSPassword
	}

	_, err := c.gclient.AddExpectedSwitch(ctx, pbReq)
	if err != nil {
		return fmt.Errorf("failed to add expected switch (bmc_mac=%s): %w", req.BMCMACAddress, err)
	}

	return nil
}

// AddExpectedPowerShelf registers an expected power shelf with NICo.
func (c *grpcClient) AddExpectedPowerShelf(ctx context.Context, req AddExpectedPowerShelfRequest) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	pbReq := &corev1.ExpectedPowerShelf{
		BmcMacAddress:     req.BMCMACAddress,
		BmcUsername:       req.BMCUsername,
		BmcPassword:       req.BMCPassword,
		ShelfSerialNumber: req.ShelfSerialNumber,
		BmcIpAddress:      req.IPAddress,
	}

	if req.RackID != "" {
		pbReq.RackId = &corev1.RackId{Id: req.RackID}
	}

	_, err := c.gclient.AddExpectedPowerShelf(ctx, pbReq)
	if err != nil {
		return fmt.Errorf("failed to add expected power shelf (bmc_mac=%s): %w", req.BMCMACAddress, err)
	}

	return nil
}

func (c *grpcClient) InsertHealthReportOverride(ctx context.Context, machineID string, source string) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.InsertMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		HealthReportEntry: &corev1.HealthReportEntry{
			Report: &corev1.HealthReport{
				Source: source,
				Alerts: []*corev1.HealthProbeAlert{{
					Id:              healthProbeIDMaintenance,
					Message:         "Machine under Flow-managed maintenance",
					Classifications: []string{classificationSuppressExternalAlerting},
				}},
			},
			Mode: corev1.HealthReportApplyMode_Replace,
		},
	}

	_, err := c.gclient.InsertHealthReportOverride(ctx, req)
	if err != nil {
		return fmt.Errorf("failed to insert health report override for machine %s: %w", machineID, err)
	}
	return nil
}

func (c *grpcClient) RemoveHealthReportOverride(ctx context.Context, machineID string, source string) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.RemoveMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		Source:    source,
	}

	_, err := c.gclient.RemoveHealthReportOverride(ctx, req)
	if err != nil {
		return fmt.Errorf("failed to remove health report override for machine %s: %w", machineID, err)
	}
	return nil
}

// InsertHostUpdateInProgressHealthOverride writes the (id,
// classifications, source) triple Core's `trigger_dpu_reprovisioning`
// validates against. The Replace mode means a stale override from an
// aborted earlier run is overwritten cleanly rather than accumulating
// duplicate alerts.
func (c *grpcClient) InsertHostUpdateInProgressHealthOverride(
	ctx context.Context,
	machineID string,
	message string,
) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.InsertMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		HealthReportEntry: &corev1.HealthReportEntry{
			Report: &corev1.HealthReport{
				Source: healthReportSourceHostUpdate,
				Alerts: []*corev1.HealthProbeAlert{{
					Id:      healthProbeIDHostUpdateInProgress,
					Message: message,
					Classifications: []string{
						classificationPreventAllocations,
						classificationSuppressExternalAlerting,
					},
				}},
			},
			Mode: corev1.HealthReportApplyMode_Replace,
		},
	}

	if _, err := c.gclient.InsertMachineHealthReport(ctx, req); err != nil {
		return fmt.Errorf(
			"failed to insert HostUpdateInProgress health override for machine %s: %w",
			machineID, err,
		)
	}
	return nil
}

// RemoveHostUpdateInProgressHealthOverride is the cleanup counterpart of
// InsertHostUpdateInProgressHealthOverride. The remove RPC tolerates
// removing an override that does not exist — that is the desired
// idempotent behavior for the deferred cleanup path.
func (c *grpcClient) RemoveHostUpdateInProgressHealthOverride(
	ctx context.Context,
	machineID string,
) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.RemoveMachineHealthReportRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		Source:    healthReportSourceHostUpdate,
	}

	if _, err := c.gclient.RemoveMachineHealthReport(ctx, req); err != nil {
		return fmt.Errorf(
			"failed to remove HostUpdateInProgress health override for machine %s: %w",
			machineID, err,
		)
	}
	return nil
}

// TriggerDpuReprovisioning forwards the call to Core's matching RPC with
// fixed Mode=Set / Initiator=AdminCli. AdminCli (rather than Automatic)
// is the right initiator because this method is on the Flow gRPC client
// path used by externally-driven tenant flows; the reconciler in Core
// uses Automatic for its own internal triggers.
func (c *grpcClient) TriggerDpuReprovisioning(
	ctx context.Context,
	machineID string,
	updateFirmware bool,
) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.DpuReprovisioningRequest{
		MachineId:      &corev1.MachineId{Id: machineID},
		Mode:           corev1.DpuReprovisioningRequest_Set,
		Initiator:      corev1.UpdateInitiator_AdminCli,
		UpdateFirmware: updateFirmware,
	}

	if _, err := c.gclient.TriggerDpuReprovisioning(ctx, req); err != nil {
		return fmt.Errorf(
			"failed to trigger DPU reprovisioning for machine %s: %w",
			machineID, err,
		)
	}
	return nil
}

// IsDpuReprovisioningPendingForHost calls Core's ListDpuWaitingForReprovisioning
// and matches by `id` (the DPU machine id). A pending entry's
// `started_at` may or may not be set depending on whether the host
// power-cycle has already begun, so callers should treat any presence
// in the list as "not done yet".
//
// This walks the full list because Core does not (yet) expose a
// per-host filter on the RPC; the list is short in practice (DPUs
// pending across the whole site), but if it ever grows we should
// extend the proto with a host filter rather than trim here.
func (c *grpcClient) IsDpuReprovisioningPendingForHost(
	ctx context.Context,
	hostMachineID string,
) (bool, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	dpuIDs, err := c.findAssociatedDpuMachineIdsLocked(ctx, hostMachineID)
	if err != nil {
		return false, err
	}
	if len(dpuIDs) == 0 {
		return false, nil
	}
	dpuSet := make(map[string]struct{}, len(dpuIDs))
	for _, id := range dpuIDs {
		dpuSet[id] = struct{}{}
	}

	resp, err := c.gclient.ListDpuWaitingForReprovisioning(ctx, &corev1.DpuReprovisioningListRequest{})
	if err != nil {
		return false, fmt.Errorf("failed to list DPUs waiting for reprovisioning: %w", err)
	}
	for _, item := range resp.GetDpus() {
		if _, ok := dpuSet[item.GetId().GetId()]; ok {
			return true, nil
		}
	}
	return false, nil
}

// FindAssociatedDpuMachineIds wraps findAssociatedDpuMachineIdsLocked
// with the per-call timeout. The unwrapped helper is reused by
// IsDpuReprovisioningPendingForHost so we don't double-apply the
// timeout when chaining the two RPCs.
func (c *grpcClient) FindAssociatedDpuMachineIds(
	ctx context.Context,
	hostMachineID string,
) ([]string, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	return c.findAssociatedDpuMachineIdsLocked(ctx, hostMachineID)
}

func (c *grpcClient) findAssociatedDpuMachineIdsLocked(
	ctx context.Context,
	hostMachineID string,
) ([]string, error) {
	if hostMachineID == "" {
		return nil, fmt.Errorf("host machine id is required")
	}

	request := &corev1.MachinesByIdsRequest{
		MachineIds: []*corev1.MachineId{{Id: hostMachineID}},
	}
	var machine *corev1.Machine
	if err := c.gclient.visitMachineBatches(ctx, request, func(batch []*corev1.Machine) error {
		if len(batch) > 0 {
			machine = batch[0]
		}
		return nil
	}); err != nil {
		return nil, fmt.Errorf("failed to find machine %s: %w", hostMachineID, err)
	}
	if machine == nil {
		return nil, fmt.Errorf("machine %s not found", hostMachineID)
	}

	dpus := machine.GetStatus().GetAssociatedDpuMachineIds()
	out := make([]string, 0, len(dpus))
	for _, id := range dpus {
		if v := id.GetId(); v != "" {
			out = append(out, v)
		}
	}
	return out, nil
}

// FindInstanceIdByMachineId returns the instance id currently attached
// to a host machine. An empty result is the canonical "no instance
// attached" signal — callers must check before passing the result to
// InvokeInstancePower because Core rejects an empty instance id.
func (c *grpcClient) FindInstanceIdByMachineId(
	ctx context.Context,
	machineID string,
) (string, error) {
	if machineID == "" {
		return "", fmt.Errorf("machine id is required")
	}
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.FindInstanceByMachineID(ctx, &corev1.MachineId{Id: machineID})
	if err != nil {
		return "", fmt.Errorf("failed to find instance for machine %s: %w", machineID, err)
	}
	for _, inst := range resp.GetInstances() {
		if id := inst.GetId().GetValue(); id != "" {
			return id, nil
		}
	}
	return "", nil
}

// InvokeInstancePower triggers a POWER_RESET on a tenant instance with
// optional `apply_updates_on_reboot`. POWER_RESET is the only operation
// the proto exposes today; if more are added (e.g. a graceful flavor)
// we will need to extend the wrapper.
func (c *grpcClient) InvokeInstancePower(
	ctx context.Context,
	instanceID string,
	applyUpdates bool,
) error {
	if instanceID == "" {
		return fmt.Errorf("instance id is required")
	}
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	req := &corev1.InstancePowerRequest{
		InstanceId:           &corev1.InstanceId{Value: instanceID},
		Operation:            corev1.InstancePowerRequest_POWER_RESET,
		ApplyUpdatesOnReboot: applyUpdates,
	}

	if _, err := c.gclient.InvokeInstancePower(ctx, req); err != nil {
		return fmt.Errorf(
			"failed to invoke instance power on %s (apply_updates=%t): %w",
			instanceID, applyUpdates, err,
		)
	}
	return nil
}

func (c *grpcClient) ComponentPowerControl(ctx context.Context, req *corev1.ComponentPowerControlRequest) (*corev1.ComponentPowerControlResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.ComponentPowerControl(ctx, req)
}

func (c *grpcClient) UpdateComponentFirmware(ctx context.Context, req *corev1.UpdateComponentFirmwareRequest) (*corev1.UpdateComponentFirmwareResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.UpdateComponentFirmware(ctx, req)
}

func (c *grpcClient) GetComponentFirmwareStatus(ctx context.Context, req *corev1.GetComponentFirmwareStatusRequest) (*corev1.GetComponentFirmwareStatusResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.GetComponentFirmwareStatus(ctx, req)
}

func (c *grpcClient) ListComponentFirmwareVersions(ctx context.Context, req *corev1.ListComponentFirmwareVersionsRequest) (*corev1.ListComponentFirmwareVersionsResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.ListComponentFirmwareVersions(ctx, req)
}

func (c *grpcClient) GetComponentInventory(ctx context.Context, req *corev1.GetComponentInventoryRequest) (*corev1.GetComponentInventoryResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()
	return c.gclient.GetComponentInventory(ctx, req)
}

func (c *grpcClient) GetAllExpectedSwitchesLinked(ctx context.Context) ([]LinkedExpectedSwitch, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetAllExpectedSwitchesLinked(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("failed to get all expected switches linked: %w", err)
	}

	var results []LinkedExpectedSwitch
	for _, les := range resp.GetExpectedSwitches() {
		results = append(results, linkedExpectedSwitchFromPb(les))
	}
	return results, nil
}

func (c *grpcClient) GetAllExpectedPowerShelvesLinked(ctx context.Context) ([]LinkedExpectedPowerShelf, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetAllExpectedPowerShelvesLinked(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("failed to get all expected power shelves linked: %w", err)
	}

	var results []LinkedExpectedPowerShelf
	for _, leps := range resp.GetExpectedPowerShelves() {
		results = append(results, linkedExpectedPowerShelfFromPb(leps))
	}
	return results, nil
}

func (c *grpcClient) GetAllExpectedRackDetails(ctx context.Context) ([]ExpectedRackDetail, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetAllExpectedRacks(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("failed to get all expected racks: %w", err)
	}
	rows := resp.GetExpectedRacks()
	if len(rows) == 0 {
		return nil, nil
	}
	results := make([]ExpectedRackDetail, 0, len(rows))
	for _, er := range rows {
		results = append(results, expectedRackDetailFromPb(er))
	}
	return results, nil
}

func (c *grpcClient) GetAllExpectedMachineDetails(ctx context.Context) ([]ExpectedMachineDetail, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetAllExpectedMachines(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("failed to get all expected machines: %w", err)
	}
	rows := resp.GetExpectedMachines()
	if len(rows) == 0 {
		return nil, nil
	}
	results := make([]ExpectedMachineDetail, 0, len(rows))
	for _, em := range rows {
		results = append(results, expectedMachineDetailFromPb(em))
	}
	return results, nil
}

func (c *grpcClient) GetAllExpectedSwitchDetails(ctx context.Context) ([]ExpectedSwitchDetail, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetAllExpectedSwitches(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("failed to get all expected switches: %w", err)
	}
	rows := resp.GetExpectedSwitches()
	if len(rows) == 0 {
		return nil, nil
	}
	results := make([]ExpectedSwitchDetail, 0, len(rows))
	for _, es := range rows {
		results = append(results, expectedSwitchDetailFromPb(es))
	}
	return results, nil
}

func (c *grpcClient) GetAllExpectedPowerShelfDetails(ctx context.Context) ([]ExpectedPowerShelfDetail, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetAllExpectedPowerShelves(ctx, &emptypb.Empty{})
	if err != nil {
		return nil, fmt.Errorf("failed to get all expected power shelves: %w", err)
	}
	rows := resp.GetExpectedPowerShelves()
	if len(rows) == 0 {
		return nil, nil
	}
	results := make([]ExpectedPowerShelfDetail, 0, len(rows))
	for _, eps := range rows {
		results = append(results, expectedPowerShelfDetailFromPb(eps))
	}
	return results, nil
}

func (c *grpcClient) GetDesiredFirmwareVersions(ctx context.Context) ([]*corev1.DesiredFirmwareVersionEntry, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	resp, err := c.gclient.GetDesiredFirmwareVersions(ctx, &corev1.GetDesiredFirmwareVersionsRequest{})
	if err != nil {
		return nil, fmt.Errorf("failed to get desired firmware versions: %w", err)
	}
	return resp.GetEntries(), nil
}

func (c *grpcClient) FindExploredEndpointsByIds(ctx context.Context, bmcIPs []string) ([]*corev1.ExploredEndpoint, error) {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	if len(bmcIPs) == 0 {
		return nil, nil
	}

	resp, err := c.gclient.FindExploredEndpointsByIds(ctx, &corev1.ExploredEndpointsByIdsRequest{
		EndpointIds: bmcIPs,
	})
	if err != nil {
		return nil, fmt.Errorf("failed to find explored endpoints by IDs: %w", err)
	}
	return resp.GetEndpoints(), nil
}

func (c *grpcClient) SetMachineAutoUpdate(ctx context.Context, machineID string, enable bool) error {
	ctx, cancel := context.WithTimeout(ctx, c.grpcTimeout)
	defer cancel()

	action := corev1.MachineSetAutoUpdateRequest_Enable
	if !enable {
		action = corev1.MachineSetAutoUpdateRequest_Disable
	}

	_, err := c.gclient.MachineSetAutoUpdate(ctx, &corev1.MachineSetAutoUpdateRequest{
		MachineId: &corev1.MachineId{Id: machineID},
		Action:    action,
	})
	if err != nil {
		return fmt.Errorf("failed to set auto-update for machine %s: %w", machineID, err)
	}
	return nil
}

func (c *grpcClient) AddMachine(machine MachineDetail) {
	panic("Not a unit test")
}

func (c *grpcClient) AddPowerState(machineID string, state PowerState) {
	panic("Not a unit test")
}

func (c *grpcClient) SetFirmwareUpdateTimeWindowError(err error) {
	panic("Not a unit test")
}

func (c *grpcClient) SetAdminPowerControlError(err error) {
	panic("Not a unit test")
}

func (c *grpcClient) AddMachineInterface(iface MachineInterface) {
	panic("Not a unit test")
}

func (c *grpcClient) AddExpectedSwitchInfo(info ExpectedSwitchInfo) {
	panic("Not a unit test")
}

func (c *grpcClient) SetLeakingMachineIds(ids []string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetLeakingSwitchIds(ids []string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetSwitchRackID(switchID, rackID string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetPowerShelfRackID(shelfID, rackID string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetSwitchControllerState(switchID, state string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetSwitchNvosIP(switchID, ip string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetPowerShelfControllerState(shelfID, state string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetObservedSwitches(devices []ObservedControllerDevice) {
	panic("Not a unit test")
}

func (c *grpcClient) SetObservedPowerShelves(devices []ObservedControllerDevice) {
	panic("Not a unit test")
}

func (c *grpcClient) SetRackHostMachineIDs(rackID string, machineIDs []string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetObservedNVLinkDomainMemberships(memberships []NVLinkDomainMembership) {
	panic("Not a unit test")
}

func (c *grpcClient) AddExpectedRackDetail(detail ExpectedRackDetail) {
	panic("Not a unit test")
}

func (c *grpcClient) AddExpectedMachineDetail(detail ExpectedMachineDetail) {
	panic("Not a unit test")
}

func (c *grpcClient) AddExpectedSwitchDetail(detail ExpectedSwitchDetail) {
	panic("Not a unit test")
}

func (c *grpcClient) AddExpectedPowerShelfDetail(detail ExpectedPowerShelfDetail) {
	panic("Not a unit test")
}

func (c *grpcClient) SetHostDpuMachineIds(hostMachineID string, dpuIDs []string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetHostInstanceID(hostMachineID string, instanceID string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetDpuReprovisioningPending(hostMachineID string, pending bool) {
	panic("Not a unit test")
}

func (c *grpcClient) SetInsertHostUpdateOverrideError(err error) {
	panic("Not a unit test")
}

func (c *grpcClient) SetTriggerDpuReprovisioningError(err error) {
	panic("Not a unit test")
}

func (c *grpcClient) SetInvokeInstancePowerError(err error) {
	panic("Not a unit test")
}

func (c *grpcClient) DpuReprovisioningTriggers() []DpuReprovisioningCall {
	panic("Not a unit test")
}

func (c *grpcClient) InstancePowerCalls() []InstancePowerCall {
	panic("Not a unit test")
}

func (c *grpcClient) HostUpdateOverridesActive() map[string]string {
	panic("Not a unit test")
}

func (c *grpcClient) SetMachineControllerState(machineID, state string) {
	panic("Not a unit test")
}

func (c *grpcClient) SetDecommissionMachineError(err error) {
	panic("Not a unit test")
}

func (c *grpcClient) SetDecommissionSwitchError(err error) {
	panic("Not a unit test")
}

func (c *grpcClient) SetDecommissionPowerShelfError(err error) {
	panic("Not a unit test")
}
