// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package redfish

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"
)

func newTestClient(t *testing.T, h http.HandlerFunc) *Client {
	t.Helper()
	// A TLS server with a self-signed certificate, like a DPU BMC.
	srv := httptest.NewTLSServer(h)
	t.Cleanup(srv.Close)
	return New(srv.URL, "root", "pw", 5*time.Second)
}

func TestSimpleUpdateTaskPath(t *testing.T) {
	for _, tc := range []struct {
		name     string
		location string
		body     string
		want     string
		wantErr  bool
	}{
		{name: "location header", location: "/redfish/v1/TaskService/Tasks/3", body: `{}`, want: "/redfish/v1/TaskService/Tasks/3"},
		{name: "absolute location", location: "https://bmc/redfish/v1/TaskService/Tasks/4", body: `{}`, want: "/redfish/v1/TaskService/Tasks/4"},
		{name: "body id only", body: `{"Id":"5"}`, want: "/redfish/v1/TaskService/Tasks/5"},
		{name: "no task named", body: `{}`, wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := newTestClient(t, func(w http.ResponseWriter, _ *http.Request) {
				if tc.location != "" {
					w.Header().Set("Location", tc.location)
				}
				w.WriteHeader(http.StatusAccepted)
				_, _ = w.Write([]byte(tc.body))
			})
			got, err := c.SimpleUpdate(context.Background(), "http://x/y.bfb")
			if (err != nil) != tc.wantErr || got != tc.want {
				t.Fatalf("SimpleUpdate = %q, %v; want %q (err=%v)", got, err, tc.want, tc.wantErr)
			}
		})
	}
}

func TestSimpleUpdateRequiresAccepted(t *testing.T) {
	c := newTestClient(t, func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Location", "/redfish/v1/TaskService/Tasks/1")
		w.WriteHeader(http.StatusOK)
	})
	if _, err := c.SimpleUpdate(context.Background(), "http://x/y.bfb"); !errors.Is(err, ErrUpdateRejected) {
		t.Fatalf("err = %v, want ErrUpdateRejected", err)
	}
}

func TestGetTask(t *testing.T) {
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/t/done":
			_, _ = w.Write([]byte(`{"TaskState":"Completed","PercentComplete":100,"Messages":[{"Message":"ok"}]}`))
		case "/t/bad":
			w.WriteHeader(http.StatusInternalServerError)
		case "/t/missing":
			w.WriteHeader(http.StatusNotFound)
			_, _ = w.Write([]byte(`{"error":{"code":"Base.ResourceMissingAtURI","message":"task not found"}}`))
		default:
			http.NotFound(w, r)
		}
	})
	task, err := c.GetTask(context.Background(), "/t/done")
	if err != nil || task.State != TaskStateCompleted || task.PercentComplete != 100 || len(task.Messages) != 1 {
		t.Fatalf("GetTask = %+v, %v", task, err)
	}
	if _, err := c.GetTask(context.Background(), "/t/missing"); !errors.Is(err, ErrTaskNotFound) {
		t.Fatalf("missing task err = %v, want ErrTaskNotFound", err)
	}
	if _, err := c.GetTask(context.Background(), "/t/bad"); err == nil || errors.Is(err, ErrTaskNotFound) {
		t.Fatalf("500 err = %v, want a plain error", err)
	}
}

func TestTaskNotFoundRequiresRedfishError(t *testing.T) {
	for _, tc := range []struct {
		name string
		body string
		lost bool
	}{
		{name: "lost task", body: `{"error":{"code":"Base.ResourceMissingAtURI"}}`, lost: true},
		{name: "route miss", body: `no router configured for forwarded_host/host/authority`},
		{name: "empty response"},
		{name: "other JSON", body: `{"message":"proxy route missing"}`},
		{name: "null error", body: `{"error":null}`},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := newTestClient(t, func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(http.StatusNotFound)
				_, _ = w.Write([]byte(tc.body))
			})
			_, err := c.GetTask(context.Background(), "/redfish/v1/TaskService/Tasks/1")
			if err == nil || errors.Is(err, ErrTaskNotFound) != tc.lost {
				t.Fatalf("GetTask error = %v, want TaskLost=%v", err, tc.lost)
			}
		})
	}
}
