// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package sitemgr implements the site manager
package sitemgr

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	crdsv1 "github.com/dsx-ai-factory/infra-controller/rest-api/site-manager/pkg/crds/v1"
	"github.com/dsx-ai-factory/infra-controller/rest-api/site-manager/pkg/types"
	"github.com/gorilla/mux"
	k8serr "k8s.io/apimachinery/pkg/api/errors"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
)

const (
	otpLength = 20
)

type createHandler struct {
	manager *SiteMgr
}

// ServeHTTP implements the site create method
func (h *createHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	log := h.manager.log
	req := &types.SiteCreateRequest{}
	if err := json.NewDecoder(r.Body).Decode(req); err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	otp, err := generateOTP()
	if err != nil {
		log.Errorf("generateOTP: %v", err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	exp, err := parseExpiry(otp)
	if err != nil {
		log.Errorf("parseExpiry: %v", err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	objName := nameFromUUID(req.SiteUUID)
	siteCRD := &crdsv1.Site{
		ObjectMeta: metav1.ObjectMeta{Name: objName, Namespace: h.manager.namespace},
		Spec: crdsv1.SiteSpec{
			UUID:     req.SiteUUID,
			SiteName: req.Name,
			Provider: req.Provider,
			FCOrg:    req.FCOrg,
		},
	}

	if _, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).Create(r.Context(), siteCRD, metav1.CreateOptions{}); err != nil {
		log.Errorf("Create Site Req: %+v  %v", req, err)
		if k8serr.IsAlreadyExists(err) {
			http.Error(w, err.Error(), http.StatusBadRequest)
			return
		}
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	siteObj, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).Get(r.Context(), objName, metav1.GetOptions{})
	if err != nil {
		log.Errorf("Create site %s %v", objName, err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	siteObj.Status = crdsv1.SiteStatus{
		OTP:            *otp,
		BootstrapState: crdsv1.SiteAwaitHandshake,
	}
	if _, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).UpdateStatus(r.Context(), siteObj, metav1.UpdateOptions{}); err != nil {
		log.Errorf("Create site %s %v", objName, err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	log.Infof("Site created - %+v", req)
	log.Infof("expire at: %s", exp.String())
}

type deleteHandler struct {
	manager *SiteMgr
}

// ServeHTTP implements the site delete method
func (h *deleteHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	log := h.manager.log
	vars := mux.Vars(r)
	uuid := vars["uuid"]
	if uuid == "" {
		http.Error(w, "UUID is required", http.StatusBadRequest)
		return
	}
	objName := nameFromUUID(uuid)
	if err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).Delete(r.Context(), objName, metav1.DeleteOptions{}); err != nil {
		log.Errorf("Delete site %s %v", objName, err)
		if k8serr.IsNotFound(err) {
			http.Error(w, err.Error(), http.StatusNotFound)
			return
		}
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	log.Infof("Deleted site %s", objName)
}

type getHandler struct {
	manager *SiteMgr
}

// ServeHTTP implements the site get method
func (h *getHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	log := h.manager.log
	vars := mux.Vars(r)
	uuid := vars["uuid"]
	if uuid == "" {
		log.Errorf("Get site uuid missing")
		http.Error(w, "UUID is required", http.StatusBadRequest)
		return
	}
	objName := nameFromUUID(uuid)
	siteObj, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).Get(r.Context(), objName, metav1.GetOptions{})
	if err != nil {
		log.Errorf("Get site %s %v", objName, err)
		if k8serr.IsNotFound(err) {
			http.Error(w, err.Error(), http.StatusNotFound)
			return
		}
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	resp := &types.SiteGetResponse{}
	resp.SiteUUID = uuid
	resp.Name = siteObj.Spec.SiteName
	resp.Provider = siteObj.Spec.Provider
	resp.FCOrg = siteObj.Spec.FCOrg
	resp.BootstrapState = siteObj.Status.BootstrapState
	resp.ControlPlaneStatus = siteObj.Status.ControlPlaneStatus
	resp.OTP = siteObj.Status.OTP.Passcode
	exp, err := parseExpiry(&siteObj.Status.OTP)
	if err == nil {
		resp.OTPExpiry = exp.String()
	}
	h.manager.writeJSONResp(w, resp)
}

type registerHandler struct {
	manager *SiteMgr
}

// ServeHTTP implements the site register method
func (h *registerHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	log := h.manager.log
	vars := mux.Vars(r)
	uuid := vars["uuid"]
	if uuid == "" {
		http.Error(w, "UUID is required", http.StatusBadRequest)
		return
	}
	objName := nameFromUUID(uuid)
	siteObj, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).Get(r.Context(), objName, metav1.GetOptions{})
	if err != nil {
		log.Errorf("Register site %s %v", objName, err)
		if k8serr.IsNotFound(err) {
			http.Error(w, err.Error(), http.StatusNotFound)
			return
		}
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	siteObj.Status.BootstrapState = crdsv1.SiteRegistrationComplete
	if _, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).UpdateStatus(r.Context(), siteObj, metav1.UpdateOptions{}); err != nil {
		log.Errorf("Register site %s %v", objName, err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	log.Infof("%s registration complete", objName)
}

type rollHandler struct {
	manager *SiteMgr
}

// ServeHTTP implements the site roll method
func (h *rollHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	log := h.manager.log
	vars := mux.Vars(r)
	uuid := vars["uuid"]
	if uuid == "" {
		http.Error(w, "UUID is required", http.StatusBadRequest)
		return
	}
	objName := nameFromUUID(uuid)
	siteObj, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).Get(r.Context(), objName, metav1.GetOptions{})
	if err != nil {
		log.Errorf("Roll site %s %v", objName, err)
		if k8serr.IsNotFound(err) {
			http.Error(w, err.Error(), http.StatusNotFound)
			return
		}
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	otp, err := generateOTP()
	if err != nil {
		log.Errorf("generateOTP: %v", err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	exp, err := parseExpiry(otp)
	if err != nil {
		log.Errorf("parseExpiry: %v", err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	siteObj.Status.OTP = *otp
	siteObj.Status.BootstrapState = crdsv1.SiteAwaitHandshake
	if _, err := h.manager.crdClient.ForgeV1().Sites(h.manager.namespace).UpdateStatus(r.Context(), siteObj, metav1.UpdateOptions{}); err != nil {
		log.Errorf("Roll site %s %v", objName, err)
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	log.Infof("%s rolled, expire at: %s", objName, exp.String())
}

func generateOTP() (*crdsv1.OTPInfo, error) {
	p, err := generateRandomString(otpLength)
	if err != nil {
		return nil, err
	}

	expiry := time.Now().Add(otpDuration)
	expiryTS, err := expiry.MarshalText()
	if err != nil {
		return nil, err
	}
	return &crdsv1.OTPInfo{
		Passcode:  p,
		Timestamp: string(expiryTS),
	}, nil
}

// GenerateRandomBytes generates a random byte array of length n, using the crypto rand package.
func generateRandomBytes(n int) ([]byte, error) {
	b := make([]byte, n)
	_, err := rand.Read(b)
	if err != nil {
		return b, err
	}

	return b, nil
}

// GenerateRandomString generates a random string of length n, using the crypto rand package.
func generateRandomString(n int) (string, error) {
	b, err := generateRandomBytes(n)
	return base64.URLEncoding.EncodeToString(b), err
}

func parseExpiry(o *crdsv1.OTPInfo) (*time.Time, error) {
	t := &time.Time{}
	if o.Timestamp == "" {
		return nil, fmt.Errorf("empty timestamp")
	}
	err := t.UnmarshalText([]byte(o.Timestamp))
	if err != nil {
		return nil, err
	}
	return t, nil
}

func nameFromUUID(uuid string) string {
	return fmt.Sprintf("site-%s", uuid)
}
