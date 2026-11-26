# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Shared Ubuntu Docker installation. Callers supply log, die, run_as_user,
# DEV_USER and VERSION_CODENAME. Sourcing does not change the host.

install_docker() {
  if command -v docker >/dev/null 2>&1 && \
    command -v containerd >/dev/null 2>&1 && \
    docker buildx version >/dev/null 2>&1; then
    return
  fi

  log "Installing Docker CE"
  install -m 0755 -d /etc/apt/keyrings
  curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
    -o /etc/apt/keyrings/docker.asc
  chmod a+r /etc/apt/keyrings/docker.asc

  cat >/etc/apt/sources.list.d/docker.sources <<EOF
Types: deb
URIs: https://download.docker.com/linux/ubuntu
Suites: ${VERSION_CODENAME}
Components: stable
Architectures: $(dpkg --print-architecture)
Signed-By: /etc/apt/keyrings/docker.asc
EOF

  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    containerd.io \
    docker-buildx-plugin \
    docker-ce \
    docker-ce-cli \
    docker-compose-plugin
}

configure_docker() {
  log "Configuring Docker access and registry TLS compatibility"

  local service config
  local config_changed=0
  local restart_services=()
  for service in containerd docker; do
    config="/etc/systemd/system/${service}.service.d/10-tls-compat.conf"
    if ! cmp -s "${config}" <(printf '[Service]\nEnvironment="GODEBUG=tlsmlkem=0"\n'); then
      install -m 0755 -d "$(dirname "${config}")"
      cat >"${config}" <<'EOF'
[Service]
Environment="GODEBUG=tlsmlkem=0"
EOF
      config_changed=1
      if systemctl is-active --quiet "${service}.service"; then
        restart_services+=("${service}.service")
      fi
    fi
  done

  usermod -aG docker "${DEV_USER}"
  if [[ "${config_changed}" == 1 ]]; then
    systemctl daemon-reload
  fi
  systemctl enable --now containerd.service docker.service
  # Repeated deploys must not restart kind or discard the development Vault.
  if ((${#restart_services[@]})); then
    systemctl restart "${restart_services[@]}"
  fi

  local _attempt
  for _attempt in {1..30}; do
    if docker info >/dev/null 2>&1; then
      break
    fi
    sleep 1
  done
  docker info >/dev/null 2>&1 || die "Docker did not become ready"
  run_as_user docker info >/dev/null 2>&1 || \
    die "${DEV_USER} cannot access Docker after being added to the docker group"
}
