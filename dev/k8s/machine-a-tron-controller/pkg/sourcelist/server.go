// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sourcelist

import (
	"encoding/json"
	"net/http"
)

const (
	// DefaultAddr is the default listen address. The endpoint is
	// unauthenticated and must only be reachable from within the pod.
	DefaultAddr = "127.0.0.1:8090"

	// PathSources is the source list path.
	PathSources = "/v1/sources"
	// PathHealthz answers 200 while the controller serves the source list.
	// It is loopback-only like the rest of this listener; the kubelet probes
	// the pod-network endpoint of package healthz instead.
	PathHealthz = "/v1/healthz"
)

// Handler returns an http.Handler serving the v1 endpoints from the registry.
func Handler(registry *Registry) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc(PathSources, func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			w.Header().Set("Allow", "GET, HEAD")
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		body, err := json.Marshal(registry.Snapshot())
		if err != nil {
			http.Error(w, "encoding response", http.StatusInternalServerError)
			return
		}
		w.Header().Set("Content-Type", "application/json")
		w.Header().Set("Cache-Control", "no-store")
		w.WriteHeader(http.StatusOK)
		if r.Method == http.MethodGet {
			_, _ = w.Write(body)
		}
	})
	mux.HandleFunc(PathHealthz, func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			w.Header().Set("Allow", "GET, HEAD")
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.WriteHeader(http.StatusOK)
		if r.Method == http.MethodGet {
			_, _ = w.Write([]byte("ok\n"))
		}
	})
	return mux
}
