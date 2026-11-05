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
		kind     TaskKind
		wantErr  bool
	}{
		{name: "location header", location: "/redfish/v1/TaskService/Tasks/3", body: `{}`, want: "/redfish/v1/TaskService/Tasks/3", kind: TaskMonitor},
		{name: "absolute location", location: "https://bmc/redfish/v1/TaskService/Tasks/4", body: `{}`, want: "/redfish/v1/TaskService/Tasks/4", kind: TaskMonitor},
		{name: "body id only", body: `{"Id":"5"}`, want: "/redfish/v1/TaskService/Tasks/5"},
		{name: "escaped body id", body: `{"Id":"a/b?c"}`, want: "/redfish/v1/TaskService/Tasks/a%2Fb%3Fc"},
		{name: "body resource before id and monitor", location: "/monitor", body: `{"@odata.id":"/tasks/actual","Id":"other"}`, want: "/tasks/actual"},
		{name: "body resource only", body: `{"@odata.id":"/redfish/v1/TaskService/Tasks/6"}`, want: "/redfish/v1/TaskService/Tasks/6"},
		{name: "body id before monitor", location: "/monitor", body: `{"Id":"5"}`, want: "/redfish/v1/TaskService/Tasks/5"},
		{name: "escaped absolute resource", body: `{"@odata.id":"https://bmc/tasks/a%2Fb?view=1"}`, want: "/tasks/a%2Fb?view=1"},
		{name: "encoded leading slash", location: "%2F@evil.invalid/task", body: `{}`, wantErr: true},
		{name: "invalid relative reference", location: "not-an-absolute-path", body: `{}`, wantErr: true},
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
			if tc.kind == "" {
				tc.kind = TaskResource
			}
			if (err != nil) != tc.wantErr || got.Path != tc.want || (!tc.wantErr && got.Kind != tc.kind) {
				t.Fatalf("SimpleUpdate = %+v, %v; want %q kind=%q (err=%v)", got, err, tc.want, tc.kind, tc.wantErr)
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
	for _, tc := range []struct {
		name, body, state, message string
		kind                       TaskKind
		code, percent              int
		wantErr                    bool
	}{
		{name: "resource running", kind: TaskResource, code: 200, body: `{"TaskState":"Running"}`, state: TaskStateRunning},
		{name: "resource completed", kind: TaskResource, code: 200, body: `{"TaskState":"Completed","PercentComplete":100,"Messages":[{"Message":"ok"}]}`, state: TaskStateCompleted, percent: 100, message: "ok"},
		{name: "resource exception", kind: TaskResource, code: 200, body: `{"TaskState":"Exception","Messages":[{"Message":"bad image"}]}`, state: TaskStateException, message: "bad image"},
		{name: "resource missing state", kind: TaskResource, code: 200, body: `{"Id":"3"}`, wantErr: true},
		{name: "resource invalid JSON", kind: TaskResource, code: 200, body: `not JSON`, wantErr: true},
		{name: "resource accepted", kind: TaskResource, code: 202, state: TaskStateRunning},
		{name: "resource no content", kind: TaskResource, code: 204, state: TaskStateCompleted, percent: 100},
		{name: "monitor action response", kind: TaskMonitor, code: 200, body: `{"Message":"installed"}`, state: TaskStateCompleted, percent: 100},
		{name: "monitor empty response", kind: TaskMonitor, code: 200, state: TaskStateCompleted, percent: 100},
		{name: "monitor non-JSON response", kind: TaskMonitor, code: 200, body: `installed`, state: TaskStateCompleted, percent: 100},
		{name: "monitor success envelope", kind: TaskMonitor, code: 200, body: `{"error":{"code":"Base.Success"}}`, state: TaskStateCompleted, percent: 100},
		{name: "monitor running task", kind: TaskMonitor, code: 200, body: `{"TaskState":"Running","PercentComplete":25}`, state: TaskStateRunning, percent: 25},
		{name: "monitor exception task", kind: TaskMonitor, code: 200, body: `{"TaskState":"Exception","Messages":[{"Message":"bad image"}]}`, state: TaskStateException, message: "bad image"},
		{name: "monitor completed task", kind: TaskMonitor, code: 200, body: `{"TaskState":"Completed","PercentComplete":100,"Messages":[{"Message":"ok"}]}`, state: TaskStateCompleted, percent: 100, message: "ok"},
		{name: "monitor malformed task", kind: TaskMonitor, code: 200, body: `{"TaskState":"Running","PercentComplete":"invalid"}`, wantErr: true},
		{name: "monitor empty task state", kind: TaskMonitor, code: 200, body: `{"TaskState":""}`, wantErr: true},
		{name: "monitor null task state", kind: TaskMonitor, code: 200, body: `{"TaskState":null}`, wantErr: true},
		{name: "monitor non-string task state", kind: TaskMonitor, code: 200, body: `{"TaskState":5}`, wantErr: true},
		{name: "monitor non-object response", kind: TaskMonitor, code: 200, body: `["installed"]`, state: TaskStateCompleted, percent: 100},
		{name: "monitor accepted", kind: TaskMonitor, code: 202, state: TaskStateRunning},
		{name: "monitor no content", kind: TaskMonitor, code: 204, state: TaskStateCompleted, percent: 100},
		{name: "resource unauthorized", kind: TaskResource, code: 401, wantErr: true},
		{name: "resource server error", kind: TaskResource, code: 500, wantErr: true},
		{name: "monitor unauthorized", kind: TaskMonitor, code: 401, wantErr: true},
		{name: "monitor server error", kind: TaskMonitor, code: 500, wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			c := newTestClient(t, func(w http.ResponseWriter, _ *http.Request) {
				w.WriteHeader(tc.code)
				_, _ = w.Write([]byte(tc.body))
			})
			task, err := c.GetTask(context.Background(), TaskReference{Path: "/task", Kind: tc.kind})
			if (err != nil) != tc.wantErr || errors.Is(err, ErrTaskNotFound) || task.State != tc.state || task.PercentComplete != tc.percent {
				t.Fatalf("GetTask = %+v, %v, want state=%q percent=%d err=%v", task, err, tc.state, tc.percent, tc.wantErr)
			}
			if tc.message != "" && (len(task.Messages) != 1 || task.Messages[0] != tc.message) {
				t.Fatalf("messages=%v, want %q", task.Messages, tc.message)
			}
		})
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
			for _, kind := range []TaskKind{TaskResource, TaskMonitor} {
				_, err := c.GetTask(context.Background(), TaskReference{Path: "/redfish/v1/TaskService/Tasks/1", Kind: kind})
				if err == nil || errors.Is(err, ErrTaskNotFound) != tc.lost {
					t.Fatalf("%s GetTask error = %v, want TaskLost=%v", kind, err, tc.lost)
				}
			}
		})
	}
}

func TestSimpleUpdatePrefersTaskResourceOverMonitor(t *testing.T) {
	monitorGETs := 0
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case simpleUpdatePath:
			w.Header().Set("Location", "/redfish/v1/TaskService/TaskMonitors/3")
			w.WriteHeader(http.StatusAccepted)
			_, _ = w.Write([]byte(`{"@odata.id":"/redfish/v1/TaskService/Tasks/3","Id":"3","TaskState":"Running"}`))
		case "/redfish/v1/TaskService/Tasks/3":
			_, _ = w.Write([]byte(`{"TaskState":"Completed","PercentComplete":100}`))
		case "/redfish/v1/TaskService/TaskMonitors/3":
			monitorGETs++
			w.WriteHeader(http.StatusAccepted)
		default:
			http.NotFound(w, r)
		}
	})
	path, err := c.SimpleUpdate(context.Background(), "http://x/y.bfb")
	if err != nil {
		t.Fatal(err)
	}
	task, err := c.GetTask(context.Background(), path)
	if err != nil || task.State != TaskStateCompleted || monitorGETs != 0 {
		t.Fatalf("task=%+v err=%v monitor GETs=%d, want Completed without polling the monitor", task, err, monitorGETs)
	}
}

func TestLocationOnlyTaskMonitor(t *testing.T) {
	polls := 0
	c := newTestClient(t, func(w http.ResponseWriter, r *http.Request) {
		switch r.Method {
		case http.MethodPost:
			w.Header().Set("Location", "/opaque-monitor?task=3")
			w.WriteHeader(http.StatusAccepted)
		case http.MethodGet:
			if r.URL.RequestURI() != "/opaque-monitor?task=3" {
				t.Errorf("monitor URI=%q, want query preserved", r.URL.RequestURI())
			}
			polls++
			if polls == 1 {
				w.WriteHeader(http.StatusAccepted)
			} else {
				w.WriteHeader(http.StatusNoContent)
			}
		}
	})
	path, err := c.SimpleUpdate(context.Background(), "http://x/y.bfb")
	if err != nil {
		t.Fatal(err)
	}
	for _, expected := range []string{"Running", TaskStateCompleted} {
		task, err := c.GetTask(context.Background(), path)
		if err != nil || task.State != expected {
			t.Fatalf("task=%+v err=%v, want %s", task, err, expected)
		}
	}
}
