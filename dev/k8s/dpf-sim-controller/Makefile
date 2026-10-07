# dpf-sim-controller — dev tooling. See README.md.
#
# One-command deploy onto a machine-a-tron simulation cluster:
#
#   make image  IMG=<registry>/dpf-sim-controller:<tag>          # build + push
#   make deploy IMG=<registry>/dpf-sim-controller:<tag> \
#               PULL_SECRET=<existing-pull-secret-name>          # CRDs + RBAC + Deployment
#
# `deploy` installs ONLY the CRDs the simulator drives — it never installs the
# real DPF operator, so it is independent of the setup.sh DPF-install work.
# Do NOT run this alongside a real DPF operator: both would drive DPU status.
IMG           ?= dpf-sim-controller:dev
# Default to the namespace recorded by the last `make deploy`, so a plain
# `make undeploy` always removes the deployment it created — deploying to a
# custom namespace and undeploying the default one would leave a stray
# simulator running (violating the never-two-writers rule).
DPF_NAMESPACE ?= $(shell cat .deploy-namespace 2>/dev/null || echo dpf-operator-system)
PHASE_DWELL   ?= 3s
# Image platforms for docker-build. Simulation clusters in this repo are
# amd64; override (e.g. PLATFORMS=linux/amd64,linux/arm64) for mixed fleets.
PLATFORMS     ?= linux/amd64
# Name of a pre-existing image pull secret in $(DPF_NAMESPACE). Leave empty if
# the nodes can already pull $(IMG) (public image / node-level creds).
PULL_SECRET   ?=
# DPF CRDs the simulator drives (DPUDevice/DPUNode/DPU + the CRs NICo references).
# Shipped in the NICo repo, so no DPF operator install is required.
CRD_DIR       ?= ../../../crates/dpf/crds

# Render config/ with namespace, image and phase-dwell substituted. `|` is the
# sed delimiter on purpose — `#` would be read as a comment inside a Makefile.
RENDER_RBAC = sed -e 's|dpf-operator-system|$(DPF_NAMESPACE)|g' config/rbac/role.yaml
RENDER_MGR  = sed -e 's|dpf-operator-system|$(DPF_NAMESPACE)|g' \
                  -e 's|image: dpf-sim-controller:dev|image: $(IMG)|' \
                  -e 's|--phase-dwell=3s|--phase-dwell=$(PHASE_DWELL)|' \
                  config/manager/manager.yaml

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

.PHONY: tidy
tidy: ## go mod tidy (run after pinning the doca-platform version)
	go mod tidy

.PHONY: build
build: ## Build the binary
	go build -o bin/dpf-sim-controller ./cmd

.PHONY: run
run: ## Run out-of-cluster against $KUBECONFIG
	go run ./cmd --dpf-namespace=$(DPF_NAMESPACE) --phase-dwell=$(PHASE_DWELL)

.PHONY: test
test: ## Run unit tests (phase walker, name derivation)
	go test ./...

.PHONY: docker-build
docker-build: ## Build the image for $(PLATFORMS) (see README for the Apple-Silicon note)
	docker build --platform $(PLATFORMS) -t $(IMG) .

.PHONY: docker-push
docker-push: ## Push the image at IMG
	docker push $(IMG)

.PHONY: image
image: docker-build docker-push ## Build + push the image at IMG

.PHONY: install-crds
install-crds: ## Install ONLY the DPF CRDs the simulator drives (never the operator)
	kubectl apply -f $(CRD_DIR)
	# apply returns before discovery serves the new types; starting the
	# manager that early fails its cache/watch init on a fresh cluster.
	# 300s: right after a full site reinstall the apiserver can be slow
	# enough that already-established CRDs miss a 60s wait window.
	kubectl wait --for condition=established --timeout=300s -f $(CRD_DIR)

.PHONY: deploy
deploy: install-crds ## One-command deploy: CRDs + RBAC + Deployment (override IMG=, DPF_NAMESPACE=, PULL_SECRET=)
	kubectl get namespace $(DPF_NAMESPACE) >/dev/null 2>&1 || kubectl create namespace $(DPF_NAMESPACE)
	$(RENDER_RBAC) | kubectl apply -f -
	$(RENDER_MGR) | kubectl apply -f -
	@if [ -n "$(PULL_SECRET)" ]; then \
		echo "patching imagePullSecrets=$(PULL_SECRET)"; \
		kubectl -n $(DPF_NAMESPACE) patch deployment dpf-sim-controller --type merge \
			-p '{"spec":{"template":{"spec":{"imagePullSecrets":[{"name":"$(PULL_SECRET)"}]}}}}'; \
	fi
	# Mutable tags (the :dev default) leave a same-tag repush running old bytes;
	# with imagePullPolicy: Always, a restart forces the fresh pull either way.
	kubectl -n $(DPF_NAMESPACE) rollout restart deployment/dpf-sim-controller
	kubectl -n $(DPF_NAMESPACE) rollout status deployment/dpf-sim-controller --timeout=120s
	# Record the namespace only after the deploy fully succeeded — a failed
	# attempt must not repoint `make undeploy` away from the live deployment.
	@echo "$(DPF_NAMESPACE)" > .deploy-namespace

.PHONY: undeploy
undeploy: ## Remove the simulator (leaves the DPF CRDs in place)
	$(RENDER_MGR) | kubectl delete --ignore-not-found -f -
	$(RENDER_RBAC) | kubectl delete --ignore-not-found -f -
