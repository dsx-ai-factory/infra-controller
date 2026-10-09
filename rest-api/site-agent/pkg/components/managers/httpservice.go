// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"encoding/json"
	"net/http"
	"time"

	"github.com/rs/zerolog/log"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
)

const (
	podRoleMaster   = "Master"
	podRoleFollower = "Follower"
)

// siteAgentStatus is the /status response. The Service spreads requests across replicas,
// so it names the pod that answered. Any client that reaches the pod can read it, so it
// leaves out errors, addresses, and the Site's registration, which stay in the logs.
type siteAgentStatus struct {
	Pod       podStatus       `json:"pod"`
	Health    string          `json:"health"`
	Bootstrap bootstrapStatus `json:"bootstrap"`
	Temporal  temporalStatus  `json:"temporal"`
	CoreGrpc  grpcStatus      `json:"coreGrpc"`
	// FlowGrpc is null unless Flow gRPC is enabled.
	FlowGrpc *grpcStatus `json:"flowGrpc"`
}

type podStatus struct {
	Name string `json:"name"`
	// Role is Master on the pod that runs the bootstrap and Follower on every other pod.
	Role string `json:"role"`
}

// bootstrapStatus reports the bootstrap on this pod. When it does not run here, Message
// says why, and the download counts are null.
type bootstrapStatus struct {
	Enabled                      bool    `json:"enabled"`
	Message                      *string `json:"message"`
	CredentialDownloadsAttempted *uint64 `json:"credentialDownloadsAttempted"`
	CredentialDownloadsSucceeded *uint64 `json:"credentialDownloadsSucceeded"`
}

type temporalStatus struct {
	Health                string     `json:"health"`
	ConnectionsAttempted  uint64     `json:"connectionsAttempted"`
	ConnectionsSucceeded  uint64     `json:"connectionsSucceeded"`
	LastConnectionAttempt *time.Time `json:"lastConnectionAttempt"`
}

// grpcStatus reports a Core or Flow gRPC client.
type grpcStatus struct {
	Health            string `json:"health"`
	RequestsSucceeded uint64 `json:"requestsSucceeded"`
	RequestsFailed    uint64 `json:"requestsFailed"`
}

func handleSiteStatusRequest(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Content-Type", "application/json")
	if err := json.NewEncoder(w).Encode(newSiteAgentStatus()); err != nil {
		log.Error().Err(err).Msg("Managers: failed to write the status response")
	}
}

func newSiteAgentStatus() siteAgentStatus {
	conf := ManagerAccess.Conf.EB
	temporal := ManagerAccess.Data.EB.Managers.Workflow.State
	coreGrpc := ManagerAccess.Data.EB.Managers.CoreGrpc.State

	status := siteAgentStatus{
		Pod:       podStatus{Name: conf.PodName, Role: podRoleFollower},
		Health:    computils.SiteHealth(ManagerAccess.Data.EB).String(),
		Bootstrap: newBootstrapStatus(),
		Temporal: temporalStatus{
			Health:               computils.CompStatus(temporal.HealthStatus.Load()).String(),
			ConnectionsAttempted: temporal.ConnectionAttempted.Load(),
			ConnectionsSucceeded: temporal.ConnectionSucc.Load(),
		},
		CoreGrpc: grpcStatus{
			Health:            computils.CompStatus(coreGrpc.HealthStatus.Load()).String(),
			RequestsSucceeded: coreGrpc.GrpcSucc.Load(),
			RequestsFailed:    coreGrpc.GrpcFail.Load(),
		},
	}
	if conf.IsMasterPod {
		status.Pod.Role = podRoleMaster
	}
	if conf.FlowGrpc.Enabled {
		flowGrpc := ManagerAccess.Data.EB.Managers.FlowGrpc.State
		status.FlowGrpc = &grpcStatus{
			Health:            computils.CompStatus(flowGrpc.HealthStatus.Load()).String(),
			RequestsSucceeded: flowGrpc.GrpcSucc.Load(),
			RequestsFailed:    flowGrpc.GrpcFail.Load(),
		}
	}
	if attempted := temporal.ConnectionTime(); !attempted.IsZero() {
		status.Temporal.LastConnectionAttempt = new(attempted.UTC())
	}
	return status
}

// newBootstrapStatus reports the download counts only where the bootstrap runs. Elsewhere
// nothing is ever downloaded, so they would read as zero.
func newBootstrapStatus() bootstrapStatus {
	conf := ManagerAccess.Conf.EB
	if !conf.IsMasterPod {
		return bootstrapStatus{Message: new("Bootstrap only runs on the master pod")}
	}
	if conf.DisableBootstrap {
		return bootstrapStatus{Message: new("Bootstrap is disabled by DISABLE_BOOTSTRAP")}
	}
	bootstrap := ManagerAccess.Data.EB.Managers.Bootstrap
	return bootstrapStatus{
		Enabled:                      true,
		CredentialDownloadsAttempted: new(bootstrap.State.DownloadAttempted.Load()),
		CredentialDownloadsSucceeded: new(bootstrap.State.DownloadSucceeded.Load()),
	}
}

func newStatusServeMux() *http.ServeMux {
	mux := http.NewServeMux()
	mux.HandleFunc(computils.SiteStatus, handleSiteStatusRequest)
	mux.HandleFunc(computils.LivenessStatus, handleLivenessRequest)
	mux.HandleFunc(computils.ReadinessStatus, handleReadinessRequest)
	return mux
}

// StartHTTPServer - start a web server on the specified port.
func StartHTTPServer() {
	port := ":" + computils.StatusPort()
	mux := newStatusServeMux()
	go func() {
		err := http.ListenAndServe(port, mux)
		log.Error().Err(err).Msg("Managers: status and probe server stopped")
	}()
}
