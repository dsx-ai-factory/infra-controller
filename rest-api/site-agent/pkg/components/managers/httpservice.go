// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package managers

import (
	"fmt"
	"net/http"
	"os"

	"github.com/rs/zerolog/log"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
)

func handleSiteStatusRequest(w http.ResponseWriter, r *http.Request) {
	// Get the status of Bootstrap n write to the HTTP response body.
	siteStatus := ManagerAccess.API.Bootstrap.GetState()
	for _, v := range siteStatus {
		fmt.Fprint(w, v)
	}
	siteStatus = ManagerAccess.API.Orchestrator.GetState()
	for _, v := range siteStatus {
		fmt.Fprint(w, v)
	}
	siteStatus = ManagerAccess.API.CoreGrpc.GetState()
	for _, v := range siteStatus {
		fmt.Fprint(w, v)
	}
	fmt.Fprint(w, fmt.Sprintln(" Site Agent Health: ",
		computils.CompStatus(ManagerAccess.Data.EB.HealthStatus.Load()).String()))
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
	port := ":" + os.Getenv("ESA_PORT")
	mux := newStatusServeMux()
	go func() {
		err := http.ListenAndServe(port, mux)
		log.Error().Err(err).Msg("Managers: status and probe server stopped")
	}()
}
