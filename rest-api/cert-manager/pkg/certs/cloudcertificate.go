// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package certs

import (
	"context"
	"encoding/json"
	"net/http"

	"github.com/NVIDIA/infra-controller/rest-api/cert-manager/pkg/core"
)

type pkiCloudCertificateHandler struct {
	certificateIssuer CertificateIssuer
}

func (h *pkiCloudCertificateHandler) reply(ctx context.Context, cert, key string, err Error, w http.ResponseWriter) {
	log := core.GetLogger(ctx)

	resp := &CertificateResponse{}
	if err == ErrorNone {
		resp.Key = key
		resp.Certificate = cert
	}

	respBytes, marshalErr := json.Marshal(resp)
	if marshalErr != nil {
		log.WithField("err", ErrorMarshalJSON.String()).Errorf("Failed to json.Marshal ClientCertificateResponse %+v, err: %s", resp, marshalErr.Error())
		http.Error(w, ErrorMarshalJSON.Error(), ErrorMarshalJSON.Code())
		return
	}
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(err.Code())
	_, errWrite := w.Write(respBytes)
	if errWrite != nil {
		log.Error(errWrite)
		http.Error(w, errWrite.Error(), http.StatusInternalServerError)
		return
	}
}

// ServeHTTP implements /v1/pki/cloud-cert
func (h *pkiCloudCertificateHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	log := core.GetLogger(ctx)

	req := &CertificateRequest{}
	if err := json.NewDecoder(r.Body).Decode(req); err != nil {
		log.WithField("err", ErrorParseRequest.String()).Errorf("failed to parse request body as CertificateRequest: %s", err.Error())
		h.reply(ctx, "", "", ErrorParseRequest, w)
		return
	}

	cert, privKey, err := h.certificateIssuer.NewCertificate(ctx, req)

	if err != nil {
		log.WithField("err", ErrorGetCertificate.String()).Errorf("failed certificateIssuer.NewCertificate, err: %s", err.Error())
		h.reply(ctx, "", "", ErrorGetCertificate, w)
		return
	}

	h.reply(ctx, cert, privKey, ErrorNone, w)
}
