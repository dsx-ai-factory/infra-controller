// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package handler

// This opt-in test is launched only by the Rust SQLx fixture. It uses the
// production Site workflow/activity and a real Core loopback gRPC service;
// the SDK in-process test environment replaces the Temporal frontend only.
import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	tclient "go.temporal.io/sdk/client"
	tmocks "go.temporal.io/sdk/mocks"
	"go.temporal.io/sdk/testsuite"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/status"
	"google.golang.org/protobuf/proto"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model"
	"github.com/NVIDIA/infra-controller/rest-api/common/pkg/grpcproxy"
	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cdbp "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	siteactivity "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	siteclient "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/grpc/client"
	siteworkflow "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
	recovery "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/domain"
	recoverysite "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/client/site"
)

type joinedPacket []byte

// A binary passthrough codec is used ONLY by the forwarding server. The
// production Site client continues using protobuf encoding and descriptor
// lookup; the shim cannot forge a Core success response.
type joinedCodec struct{}

func (joinedCodec) Name() string { return "proto" }
func (joinedCodec) Marshal(v any) ([]byte, error) {
	if packet, ok := v.(*joinedPacket); ok {
		return *packet, nil
	}
	return proto.Marshal(v.(proto.Message))
}
func (joinedCodec) Unmarshal(b []byte, v any) error {
	if packet, ok := v.(*joinedPacket); ok {
		*packet = append((*packet)[:0], b...)
		return nil
	}
	return proto.Unmarshal(b, v.(proto.Message))
}

type joinedGate struct {
	entered    chan uuid.UUID
	release    chan struct{}
	canceled   chan uuid.UUID
	dropCancel bool
	mu         sync.Mutex
	methods    []string
}

func (g *joinedGate) serve(server grpc.ServerStream, core *grpc.ClientConn) error {
	method, ok := grpc.MethodFromServerStream(server)
	if !ok {
		return errors.New("no proxied method")
	}
	var in joinedPacket
	if err := server.RecvMsg(&in); err != nil {
		return err
	}
	var id uuid.UUID
	if method == corev1.Forge_CreateDomain_FullMethodName {
		var req corev1.CreateDomainRequest
		if err := proto.Unmarshal(in, &req); err != nil {
			return err
		}
		var err error
		id, err = uuid.Parse(req.GetReservedId().GetValue())
		if err != nil {
			return fmt.Errorf("create missing reserved ID: %w", err)
		}
		if req.Name == "joined-delayed.example.com" {
			select {
			case g.entered <- id:
			case <-server.Context().Done():
				return server.Context().Err()
			}
			select {
			case <-g.release:
			case <-server.Context().Done():
				return server.Context().Err()
			}
		}
	}
	if method == corev1.Forge_DeleteDomain_FullMethodName {
		var req corev1.DomainDeletionRequest
		if err := proto.Unmarshal(in, &req); err != nil {
			return err
		}
		if !req.GetCancelReservedId() {
			return errors.New("joined cancellation missing reserved intent")
		}
		var err error
		id, err = uuid.Parse(req.GetId().GetValue())
		if err != nil {
			return err
		}
	}
	g.mu.Lock()
	g.methods = append(g.methods, method)
	g.mu.Unlock()
	var out joinedPacket
	if err := core.Invoke(server.Context(), method, &in, &out, grpc.ForceCodec(joinedCodec{})); err != nil {
		return err
	}
	if method == corev1.Forge_DeleteDomain_FullMethodName {
		select {
		case g.canceled <- id:
		case <-server.Context().Done():
			return server.Context().Err()
		}
		g.mu.Lock()
		drop := g.dropCancel
		g.dropCancel = false
		g.mu.Unlock()
		if drop {
			return status.Error(codes.Unavailable, "fixture lost reply after Core committed cancellation")
		}
	}
	return server.SendMsg(&out)
}

type joinedWorkflowResult struct {
	response grpcproxy.Response
	err      error
}
type joinedRun struct {
	tmocks.WorkflowRun
	done chan joinedWorkflowResult
}

func (r *joinedRun) Get(ctx context.Context, result any) error {
	select {
	case v := <-r.done:
		if v.err == nil {
			*(result.(*grpcproxy.Response)) = v.response
		}
		return v.err
	case <-ctx.Done():
		return ctx.Err()
	}
}

type joinedTemporalClient struct {
	tmocks.Client
	activity siteactivity.ManageCoreProxy
}

func (c *joinedTemporalClient) ExecuteWorkflow(_ context.Context, _ tclient.StartWorkflowOptions, name any, args ...any) (tclient.WorkflowRun, error) {
	if name != grpcproxy.Core.WorkflowName || len(args) != 1 {
		return nil, fmt.Errorf("unexpected Site workflow %v", name)
	}
	req, ok := args[0].(grpcproxy.Request)
	if !ok || req.FullMethod == "" {
		return nil, errors.New("missing typed Core proxy request")
	}
	run := &joinedRun{done: make(chan joinedWorkflowResult, 1)}
	go func() {
		var suite testsuite.WorkflowTestSuite
		env := suite.NewTestWorkflowEnvironment()
		env.RegisterActivity(c.activity.InvokeCoreGRPCOnSite)
		env.ExecuteWorkflow(siteworkflow.InvokeCoreGRPC, req)
		var response grpcproxy.Response
		err := env.GetWorkflowError()
		if err == nil {
			err = env.GetWorkflowResult(&response)
		}
		run.done <- joinedWorkflowResult{response, err}
	}()
	return run, nil
}

func TestDomainJoinedRealSiteCore(t *testing.T) {
	target := os.Getenv("CORE_JOINED_ADDR")
	resultPath := os.Getenv("CORE_JOINED_RESULT")
	if target == "" || resultPath == "" {
		t.Skip("run via disposable Rust Core SQLx fixture")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 120*time.Second)
	defer cancel()
	core, err := grpc.NewClient(target, grpc.WithTransportCredentials(insecure.NewCredentials()))
	require.NoError(t, err)
	t.Cleanup(func() { require.NoError(t, core.Close()) })
	gate := &joinedGate{entered: make(chan uuid.UUID, 1), release: make(chan struct{}), canceled: make(chan uuid.UUID, 2)}
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	require.NoError(t, err)
	shim := grpc.NewServer(grpc.ForceServerCodec(joinedCodec{}), grpc.UnknownServiceHandler(func(_ any, stream grpc.ServerStream) error { return gate.serve(stream, core) }))
	go func() { _ = shim.Serve(listener) }()
	t.Cleanup(func() { shim.Stop(); _ = listener.Close() })
	site, err := siteclient.NewCoreGrpcClient(&siteclient.CoreGrpcClientConfig{Address: listener.Addr().String(), Secure: siteclient.InsecureGrpc})
	require.NoError(t, err, "Core Version handshake through real forwarding shim")
	t.Cleanup(func() { require.NoError(t, site.Close()) })
	atom := siteclient.NewCoreGrpcAtomicClient(&siteclient.CoreGrpcClientConfig{Address: listener.Addr().String(), Secure: siteclient.InsecureGrpc})
	atom.SwapClient(site)
	bridge := &joinedTemporalClient{activity: siteactivity.NewManageCoreProxy(atom, "")}
	ids := make([]uuid.UUID, 0, 2)

	t.Run("delayed create after real REST delete", func(t *testing.T) {
		f := newDomainHandlerFixture(t, nil)
		f.scp.IDClientMap[f.site.ID.String()] = bridge
		done := make(chan *struct {
			code int
			body string
		}, 1)
		go func() {
			// The handler's helper uses t.Fatal only if fixture setup fails;
			// HTTP/body are checked after join on the main test goroutine.
			r := f.requestWithContext(t, ctx, NewCreateDomainHandler(f.dbSession, f.scp).Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{Name: "joined-delayed.example.com", SiteID: f.site.ID.String()})
			done <- &struct {
				code int
				body string
			}{r.Code, r.Body.String()}
		}()
		var coreID uuid.UUID
		select {
		case coreID = <-gate.entered:
		case <-ctx.Done():
			t.Fatal("delayed real Create never entered forwarding shim")
		}
		ids = append(ids, coreID)
		f.requireDomainWithStatus(t, "joined-delayed.example.com", cdbm.DomainStatusPending)
		domains, _, err := cdbm.NewDomainDAO(f.dbSession).GetAll(ctx, nil, cdbm.DomainFilterInput{TenantIDs: []uuid.UUID{f.tenant.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		require.Len(t, domains, 1)
		require.Equal(t, coreID, *domains[0].ControllerDomainID)
		deleted := f.requestWithContext(t, ctx, NewDeleteDomainHandler(f.dbSession, f.scp).Handle, http.MethodDelete, "/", domains[0].ID.String(), nil)
		require.Equal(t, http.StatusNoContent, deleted.Code, deleted.Body.String())
		select {
		case got := <-gate.canceled:
			require.Equal(t, coreID, got)
		case <-ctx.Done():
			t.Fatal("Core delete not confirmed")
		}
		close(gate.release)
		select {
		case created := <-done:
			require.NotEqual(t, http.StatusCreated, created.code, created.body)
		case <-ctx.Done():
			t.Fatal("delayed create failed to finish")
		}
		f.requireNoDomains(t)
	})

	t.Run("lost cancel reply after real Core commit", func(t *testing.T) {
		f := newDomainHandlerFixture(t, nil)
		f.scp.IDClientMap[f.site.ID.String()] = bridge
		gate.mu.Lock()
		gate.dropCancel = true
		gate.mu.Unlock()
		response := f.requestWithContext(t, ctx, NewCreateDomainHandler(f.dbSession, f.scp).Handle, http.MethodPost, "/", "", model.APIDomainCreateRequest{Name: "joined-conflict.example.com", SiteID: f.site.ID.String()})
		require.Equal(t, http.StatusAccepted, response.Code, response.Body.String())
		var public model.APIDomain
		require.NoError(t, json.Unmarshal(response.Body.Bytes(), &public))
		require.Equal(t, "Pending", public.Status)
		f.requireDomainWithStatus(t, "joined-conflict.example.com", cdbm.DomainStatusRejecting)
		domains, _, err := cdbm.NewDomainDAO(f.dbSession).GetAll(ctx, nil, cdbm.DomainFilterInput{TenantIDs: []uuid.UUID{f.tenant.ID}}, cdbp.PageInput{Limit: cutil.GetPtr(cdbp.TotalLimit)}, nil)
		require.NoError(t, err)
		require.Len(t, domains, 1)
		coreID := *domains[0].ControllerDomainID
		ids = append(ids, coreID)
		select {
		case got := <-gate.canceled:
			require.Equal(t, coreID, got)
		case <-ctx.Done():
			t.Fatal("lost reply had no confirmed real Core cancellation")
		}
		workflowPool := recoverysite.NewClientPool(nil)
		workflowPool.IDClientMap[f.site.ID.String()] = bridge
		manager := recovery.ManageDomain{DB: f.dbSession, Sites: workflowPool}
		require.NoError(t, manager.ReconcileReservedDomains(ctx))
		f.requireDomainWithStatus(t, "joined-conflict.example.com", cdbm.DomainStatusError)
		changed, err := cdb.WithTxResult(ctx, f.dbSession, func(tx *cdb.Tx) (bool, error) {
			return cdbm.NewDomainDAO(f.dbSession).TransitionOwned(ctx, tx, domains[0].ID, coreID, cdbm.DomainStatusPending, cdbm.DomainStatusReady)
		})
		require.NoError(t, err)
		require.False(t, changed, "late Create success must not mark cancelled intent Ready")
	})
	gate.mu.Lock()
	defer gate.mu.Unlock()
	require.Contains(t, gate.methods, corev1.Forge_CreateDomain_FullMethodName, "real Create dispatch must occur")
	require.Contains(t, gate.methods, corev1.Forge_DeleteDomain_FullMethodName, "real Delete dispatch must occur")
	require.Len(t, ids, 2)
	file, err := os.Create(resultPath)
	require.NoError(t, err)
	for _, id := range ids {
		_, err = fmt.Fprintln(file, id)
		require.NoError(t, err)
	}
	require.NoError(t, file.Close())
}
