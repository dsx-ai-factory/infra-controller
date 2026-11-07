// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cluster

import (
	"encoding/json"
	"net/http"
	"testing"

	corev1 "k8s.io/api/core/v1"
)

const cmPath = "/api/v1/namespaces/ns/configmaps"

func cmObject(name, version, uid, key, val string) string {
	return `{"kind":"ConfigMap","metadata":{"name":"` + name + `","resourceVersion":"` + version +
		`","uid":"` + uid + `"},"data":{"` + key + `":"` + val + `"}}`
}

func TestConfigMap(t *testing.T) {
	t.Run("get returns the object, a 404 is nil, server text never leaks", func(t *testing.T) {
		k := served(t, map[string]http.HandlerFunc{
			cmPath + "/r":    reply(200, cmObject("r", "7", "u1", "result.json", "{}")),
			cmPath + "/gone": reply(404, `{"kind":"Status","code":404,"reason":"NotFound","message":"`+secret+`"}`),
			cmPath + "/boom": reply(500, `{"kind":"Status","code":500,"reason":"InternalError","message":"`+secret+`"}`),
		})
		got, err := k.GetConfigMap(bg, "r")
		want := &ConfigMapObject{Name: "r", ResourceVersion: "7", UID: "u1", Data: map[string]string{"result.json": "{}"}}
		is(t, []any{got, err}, []any{want, nil}, "found")
		gone, err := k.GetConfigMap(bg, "gone")
		is(t, []any{gone, err}, []any{(*ConfigMapObject)(nil), nil}, "404 is nil")
		_, err = k.GetConfigMap(bg, "boom")
		is(t, err, api(500, "InternalError"), "no message text")
	})

	t.Run("create posts and reports AlreadyExists", func(t *testing.T) {
		k := served(t, map[string]http.HandlerFunc{
			"POST " + cmPath: func(w http.ResponseWriter, r *http.Request) {
				var o corev1.ConfigMap
				_ = json.NewDecoder(r.Body).Decode(&o)
				if o.Annotations["owner"] == "taken" {
					reply(409, `{"kind":"Status","code":409,"reason":"AlreadyExists"}`)(w, r)
					return
				}
				reply(201, cmObject("r", "1", "u9", "result.json", "{}"))(w, r)
			},
		})
		out, err := k.CreateConfigMap(bg, ConfigMapObject{Name: "r", Annotations: map[string]string{"owner": "me"}})
		is(t, []any{out.UID, err}, []any{"u9", nil}, "created")
		_, err = k.CreateConfigMap(bg, ConfigMapObject{Name: "r", Annotations: map[string]string{"owner": "taken"}})
		is(t, err, error(Error{Kind: AlreadyExists}), "already exists")
	})

	t.Run("replace sends the uid, refuses a bare version or no uid, and reports Conflict", func(t *testing.T) {
		var sent seen
		k := served(t, map[string]http.HandlerFunc{
			"PUT " + cmPath + "/r": func(w http.ResponseWriter, r *http.Request) {
				var o corev1.ConfigMap
				_ = json.NewDecoder(r.Body).Decode(&o)
				sent.add(string(o.UID))
				if o.ResourceVersion == "stale" {
					reply(409, `{"kind":"Status","code":409,"reason":"Conflict"}`)(w, r)
					return
				}
				reply(200, cmObject("r", "8", "u1", "result.json", "x"))(w, r)
			},
		})
		base := ConfigMapObject{Name: "r", UID: "u1"}
		with := func(rv string) ConfigMapObject { c := base; c.ResourceVersion = rv; return c }
		out, err := k.ReplaceConfigMap(bg, with("7"))
		is(t, []any{out.ResourceVersion, err}, []any{"8", nil}, "replaced")
		_, err = k.ReplaceConfigMap(bg, with("stale"))
		is(t, err, error(Error{Kind: Conflict}), "stale version is a conflict")
		// A bare "0" or "" version, and a missing UID, are refused before any call.
		_, errRV := k.ReplaceConfigMap(bg, with("0"))
		_, errUID := k.ReplaceConfigMap(bg, ConfigMapObject{Name: "r", ResourceVersion: "7"})
		is(t, []any{errRV, errUID}, []any{error(Error{Kind: Conflict}), error(Error{Kind: Invalid, Text: "a ConfigMap replace needs the ConfigMap UID"})}, "refused before the call")
		is(t, sent.all(), []string{"u1", "u1"}, "the UID is sent on the writes that went out")
	})
}
