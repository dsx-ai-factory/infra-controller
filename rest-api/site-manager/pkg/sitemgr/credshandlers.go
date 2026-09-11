// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package sitemgr

import (
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	cutils "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	crdsv1 "github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/crds/v1"
	"github.com/NVIDIA/infra-controller/rest-api/site-manager/pkg/types"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

const (
	clientTTL = 90 * 24
)

type credsHandler struct {
	manager *SiteMgr
}

// ServeHTTP implements the site creds method
func (h *credsHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	log := h.manager.log
	req := &types.SiteCredsRequest{}
	if err := json.NewDecoder(r.Body).Decode(req); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	objName := nameFromUUID(req.SiteUUID)

	siteObj, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).Get(r.Context(), objName, metav1.GetOptions{})
	if err != nil {
		// This line and the rejections below report the OTP that arrived rather
		// than the request, which carries it in full and stays usable until a
		// handshake consumes it.
		log.Errorf("Site Creds Req: site %s, OTP %s  %v", req.SiteUUID,
			cutils.RedactSecret(req.OTP, cutils.SecretLogPrefixLen), err)
		var errStr string
		if k8serr.IsNotFound(err) {
			errStr = fmt.Sprintf("site %s not found", req.SiteUUID)
		} else {
			errStr = "Unable to retrieve site - check logs"
		}
		http.Error(w, errStr, http.StatusInternalServerError)
		return
	}

	if siteObj.Status.BootstrapState != crdsv1.SiteAwaitHandshake {
		log.Infof("Creds request with used OTP: site %s, OTP %s", req.SiteUUID,
			cutils.RedactSecret(req.OTP, cutils.SecretLogPrefixLen))
		http.Error(w, "OTP already used", http.StatusInternalServerError)
		return
	}

	if req.OTP != siteObj.Status.OTP.Passcode {
		log.Infof("Bad OTP received: site %s, OTP %s", req.SiteUUID,
			cutils.RedactSecret(req.OTP, cutils.SecretLogPrefixLen))
		http.Error(w, "Bad OTP", http.StatusInternalServerError)
		return
	}

	expiry, err := parseExpiry(&siteObj.Status.OTP)
	if err != nil {
		log.Errorf("Error parsing expiry: %v +%v", err, siteObj.Status.OTP.Timestamp)
		http.Error(w, "check logs", http.StatusInternalServerError)
		return
	}

	if time.Now().After(*expiry) {
		log.Infof("Expired OTP received: site %s, OTP %s", req.SiteUUID,
			cutils.RedactSecret(req.OTP, cutils.SecretLogPrefixLen))
		http.Error(w, "OTP expired", http.StatusInternalServerError)
		return
	}

	log.Infof("Getting creds for site: %s", req.SiteUUID)
	cr, err := h.manager.getCertificate(r.Context(), "client", req.SiteUUID, clientTTL)
	if err != nil {
		log.Errorf("getCertificate: %v", err)
		http.Error(w, "Error getting creds, check logs", http.StatusInternalServerError)
		return
	}

	ca, err := h.manager.getCA(r.Context())
	if err != nil {
		log.Errorf("getCA: %v", err)
		http.Error(w, "Error getting CA, check logs", http.StatusInternalServerError)
		return
	}

	resp := &types.SiteCredsResponse{
		Key:           cr.Key,
		Certificate:   cr.Certificate,
		CACertificate: ca,
	}

	siteObj.Status.BootstrapState = crdsv1.SiteHandshakeComplete
	if _, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).UpdateStatus(r.Context(), siteObj, metav1.UpdateOptions{}); err != nil {
		log.Errorf("Get credentials site %s %v", objName, err)
		http.Error(w, "check logs", http.StatusInternalServerError)
		return
	}

	h.manager.writeJSONResp(w, resp)
}
