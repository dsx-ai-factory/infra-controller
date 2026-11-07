/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! The dpu-agent's local API: an `AgentLocal` gRPC service on a unix socket,
//! by default in a dedicated `run/` subdirectory of the shared `/opt/forge`
//! credentials directory (issue #355) — so containerized consumers mount the
//! socket read-write while the credentials stay on a read-only mount.
//!
//! This is the consolidation point for agent <-> co-located-service
//! communication on the DPU. First RPC: `GetNodeToken`, which hands
//! co-located NICo services (fmds, …) a short-lived node-auth bearer JWT so
//! they can authenticate to nico-api without mounting the machine's private
//! key — only the agent ever touches the key. Future local needs should add
//! RPCs here rather than new sockets, ports, or file drops.

use std::sync::Arc;

use ::rpc::agent_local::agent_local_server::{AgentLocal, AgentLocalServer};
use ::rpc::agent_local::{GetNodeTokenRequest, GetNodeTokenResponse};
use ::rpc::node_jwt::NodeJwtMinter;
use eyre::WrapErr;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::{Request, Response, Status};

struct AgentLocalService {
    minter: Arc<NodeJwtMinter>,
}

#[tonic::async_trait]
impl AgentLocal for AgentLocalService {
    async fn get_node_token(
        &self,
        _request: Request<GetNodeTokenRequest>,
    ) -> Result<Response<GetNodeTokenResponse>, Status> {
        // Minting reads the cert/key from disk on cache miss — cheap enough to
        // do inline, and it means the endpoint starts working the moment the
        // machine certificate lands without any coordination.
        match self.minter.current_with_expiry() {
            Some((token, expires_at)) => {
                Ok(Response::new(GetNodeTokenResponse { token, expires_at }))
            }
            None => Err(Status::unavailable(
                "no node token available yet; machine certificate not present or unreadable",
            )),
        }
    }
}

/// Prepares the socket's parent directory, which must be dedicated to this
/// socket and nothing else.
///
/// The directory has to be unreachable to other users *before* `bind`: `bind`
/// creates the socket with umask-derived permissions — typically
/// world-connectable — and the 0600 chmod only lands afterward. The listener
/// is already bound in that window, so a connection from any local user would
/// queue in the backlog and be served a full machine identity the moment
/// accepting starts. An unreachable parent closes the window; the socket's own
/// 0600 is then defence in depth.
///
/// But `local-api-socket` is operator-configurable, so applying 0700 to
/// whatever the parent happens to be is its own hazard: `/run/agent.sock`
/// would take `/run` to 0700 and lock every non-root service on the box out of
/// its runtime files. Only a directory this function created, or one holding
/// nothing but this socket, is ours to restrict — anything else is rejected
/// with an actionable message rather than silently modified.
///
/// The directory is also required to be a real directory owned by this
/// process. A symlink would redirect the 0700 onto a target chosen by whoever
/// planted it, and a directory owned by another user is one we have no
/// business restricting — in both cases the configured path is wrong in a way
/// worth reporting rather than working around. This does not close every race:
/// the path is resolved afresh by `read_dir`, `set_permissions` and `bind`, so
/// an attacker who can already swap directories underneath us between those
/// calls is not stopped by it. On the DPU every process that can do that is
/// already root, which is why this stays a validity check rather than growing
/// into `openat`/`fchmod` plumbing.
fn prepare_socket_dir(dir: &std::path::Path, socket_path: &str) -> eyre::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};

    let Some(metadata) = optional_symlink_metadata(dir)
        .wrap_err(format!("inspecting socket directory {}", dir.display()))?
    else {
        // Ancestors keep their normal modes; only the socket directory itself
        // is created restricted, and created that way from the first instant
        // rather than chmod-ed after the fact.
        if let Some(ancestor) = dir.parent() {
            std::fs::create_dir_all(ancestor)
                .wrap_err(format!("creating socket directory {}", ancestor.display()))?;
        }
        return std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir)
            .wrap_err(format!("creating socket directory {}", dir.display()));
    };

    // `symlink_metadata` does not follow the final component, so this catches a
    // symlink instead of reporting whatever it points at.
    if metadata.file_type().is_symlink() {
        eyre::bail!(
            "socket directory {} is a symlink; point local-api-socket at a real \
             directory, so restricting it to 0700 cannot be redirected elsewhere",
            dir.display()
        );
    }
    if !metadata.is_dir() {
        eyre::bail!(
            "socket path parent {} exists but is not a directory",
            dir.display()
        );
    }
    let euid = nix::unistd::geteuid().as_raw();
    if metadata.uid() != euid {
        eyre::bail!(
            "socket directory {} is owned by uid {}, not this process (uid {}); \
             point local-api-socket at a directory this agent owns",
            dir.display(),
            metadata.uid(),
            euid
        );
    }

    let socket_name = std::path::Path::new(socket_path).file_name();
    for entry in
        std::fs::read_dir(dir).wrap_err(format!("reading socket directory {}", dir.display()))?
    {
        let entry = entry.wrap_err(format!("reading socket directory {}", dir.display()))?;
        let name = entry.file_name();
        if socket_name == Some(name.as_os_str()) {
            if !entry
                .file_type()
                .wrap_err(format!("inspecting socket path {}", entry.path().display()))?
                .is_socket()
            {
                eyre::bail!(
                    "socket path {} exists and is not a socket; refusing to restrict its parent \
                     directory — point local-api-socket at a path the agent owns",
                    entry.path().display()
                );
            }
        } else {
            eyre::bail!(
                "socket directory {} is shared with other files (found {}); \
                 point local-api-socket at a directory used for nothing else, \
                 so restricting it to 0700 cannot lock other services out",
                dir.display(),
                name.to_string_lossy()
            );
        }
    }

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).wrap_err(format!(
        "restricting socket directory permissions on {}",
        dir.display()
    ))
}

/// `symlink_metadata`, with "does not exist" as `Ok(None)` rather than an error.
fn optional_symlink_metadata(path: &std::path::Path) -> std::io::Result<Option<std::fs::Metadata>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Clears the socket left behind by a previous run, since `bind` fails on an
/// existing path even when nothing is listening.
///
/// Only an actual socket is removed. The path is operator-configurable and the
/// dedicated-directory rule above matches on filename alone, so a regular file,
/// directory or symlink sitting at that name would otherwise be silently
/// deleted by a privileged process — a fine primitive to hand an attacker who
/// can write the directory, and a data-loss footgun for an operator who
/// mistypes the path. Anything that is not a socket is reported instead.
fn remove_stale_socket(socket_path: &str) -> eyre::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let path = std::path::Path::new(socket_path);
    let Some(metadata) = optional_symlink_metadata(path)
        .wrap_err(format!("inspecting socket path {socket_path}"))?
    else {
        return Ok(());
    };

    if !metadata.file_type().is_socket() {
        eyre::bail!(
            "{socket_path} exists and is not a socket; refusing to remove it — \
             point local-api-socket at a path the agent owns"
        );
    }

    std::fs::remove_file(path).wrap_err(format!("removing stale socket {socket_path}"))
}

/// Binds the local API socket and serves until the process exits. The socket
/// is created mode 0600 (root-only): every legitimate consumer on the DPU
/// runs as root, and nothing else on the shared directory should be able to
/// obtain machine credentials.
///
/// Deployment-agnostic: containerized (DPF) the socket lands on the mounted
/// `/opt/forge` volume; as a plain service on DPU OS (non-DPF) the parent
/// directory is created if the agent starts before anything else touched it.
pub(crate) async fn serve(minter: Arc<NodeJwtMinter>, socket_path: &str) -> eyre::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = std::path::Path::new(socket_path).parent() {
        prepare_socket_dir(parent, socket_path)?;
    }
    remove_stale_socket(socket_path)?;

    let listener = tokio::net::UnixListener::bind(socket_path)
        .wrap_err(format!("binding agent local API socket {socket_path}"))?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))
        .wrap_err(format!("restricting socket permissions on {socket_path}"))?;
    tracing::info!(target: "node_auth", socket = %socket_path, "node-auth: serving agent local API (node tokens)");

    tonic::transport::Server::builder()
        .add_service(AgentLocalServer::new(AgentLocalService { minter }))
        .serve_with_incoming(UnixListenerStream::new(listener))
        .await
        .wrap_err("agent local API server exited")
}

#[cfg(test)]
mod tests {
    use ::rpc::node_jwt::NodeTokenProvider;
    use ::rpc::node_token_socket::SocketTokenSource;

    use super::*;

    const SPIFFE_URI: &str = "spiffe://forge.local/forge-system/machine/fm100xtest";

    fn write_cert_and_key(dir: &tempfile::TempDir) -> (String, String) {
        let mut params = rcgen::CertificateParams::default();
        params.subject_alt_names = vec![rcgen::SanType::URI(
            rcgen::string::Ia5String::try_from(SPIFFE_URI.to_string()).expect("uri"),
        )];
        let key = rcgen::KeyPair::generate().expect("key pair");
        let cert = params.self_signed(&key).expect("certificate");
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("cert.key");
        std::fs::write(&cert_path, cert.pem()).expect("write cert");
        std::fs::write(&key_path, key.serialize_pem()).expect("write key");
        (
            cert_path.to_string_lossy().into_owned(),
            key_path.to_string_lossy().into_owned(),
        )
    }

    /// End-to-end broker flow: agent serves tokens minted from the machine
    /// cert; a key-less consumer obtains one through `SocketTokenSource`.
    #[tokio::test]
    async fn keyless_consumer_gets_token_via_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert_path, key_path) = write_cert_and_key(&dir);
        // Its own subdirectory, as in production (`<certsDir>/run/agent.sock`):
        // the socket never shares a directory with the credentials.
        let socket = dir.path().join("run").join("agent.sock");
        let socket_str = socket.to_string_lossy().into_owned();

        let minter = NodeJwtMinter::new(cert_path, key_path);
        let expected = minter.current().expect("agent side can mint");
        tokio::spawn({
            let socket_str = socket_str.clone();
            async move { serve(minter, &socket_str).await }
        });

        let source = SocketTokenSource::spawn(socket_str);
        let mut got = None;
        for _ in 0..100 {
            if let Some(token) = source.current() {
                got = Some(token);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(got.as_deref(), Some(expected.as_str()));
    }

    #[tokio::test]
    async fn socket_is_root_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert_path, key_path) = write_cert_and_key(&dir);
        let socket = dir.path().join("run").join("agent.sock");
        let socket_str = socket.to_string_lossy().into_owned();

        tokio::spawn({
            let socket_str = socket_str.clone();
            async move { serve(NodeJwtMinter::new(cert_path, key_path), &socket_str).await }
        });
        // Wait for the mode, not merely for the socket to appear: `bind`
        // creates it at umask permissions and the chmod lands afterward, so
        // sampling on existence alone would read the pre-chmod mode whenever
        // the poll happened to fall inside that window — a flake that looks
        // like a real permissions regression.
        let mut mode = None;
        for _ in 0..100 {
            if let Ok(metadata) = std::fs::metadata(&socket) {
                let observed = metadata.permissions().mode() & 0o777;
                if observed == 0o600 {
                    mode = Some(observed);
                    break;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            mode,
            Some(0o600),
            "the socket must end up root-only; last seen: {:?}",
            std::fs::metadata(&socket).map(|m| m.permissions().mode() & 0o777)
        );
    }

    /// The socket's own mode is applied only after `bind`, so the directory is
    /// what actually keeps other users out during that window. Anyone who got
    /// in would be handed a full machine identity.
    #[tokio::test]
    async fn socket_directory_is_root_only_before_the_socket_exists() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let (cert_path, key_path) = write_cert_and_key(&dir);
        // A pre-existing, world-traversable directory: the agent must tighten
        // it rather than assume a fresh one.
        let run_dir = dir.path().join("run");
        std::fs::create_dir(&run_dir).expect("create run dir");
        std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o755))
            .expect("loosen run dir");
        let socket = run_dir.join("agent.sock");
        let socket_str = socket.to_string_lossy().into_owned();

        tokio::spawn({
            let socket_str = socket_str.clone();
            async move { serve(NodeJwtMinter::new(cert_path, key_path), &socket_str).await }
        });
        for _ in 0..100 {
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let mode = std::fs::metadata(&run_dir)
            .expect("run dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "socket directory must exclude other users");
    }

    /// Tightening the parent is only safe when the parent belongs to us.
    /// `local-api-socket = /run/agent.sock` would otherwise take `/run` to
    /// 0700 and lock every non-root service on the box out of its runtime
    /// files, so a directory holding anything else is refused outright.
    #[test]
    fn shared_socket_directory_is_refused_rather_than_restricted() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let shared = dir.path().join("run");
        std::fs::create_dir(&shared).expect("create shared dir");
        std::fs::write(shared.join("someone-elses.pid"), b"1234").expect("write neighbour");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755))
            .expect("loosen shared dir");
        let socket = shared.join("agent.sock");

        let err = prepare_socket_dir(&shared, &socket.to_string_lossy())
            .expect_err("a shared directory must not be accepted");
        assert!(
            err.to_string().contains("someone-elses.pid"),
            "the error should name the file that made it shared, got: {err}"
        );

        let mode = std::fs::metadata(&shared)
            .expect("shared dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o755,
            "a refused directory must be left exactly as it was found"
        );
    }

    /// A directory we create ourselves is restricted from its first instant,
    /// with no window at a looser mode for anyone to slip through.
    #[test]
    fn fresh_socket_directory_is_created_root_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let run_dir = dir.path().join("nested").join("run");
        let socket = run_dir.join("agent.sock");

        prepare_socket_dir(&run_dir, &socket.to_string_lossy()).expect("prepare fresh dir");

        let mode = std::fs::metadata(&run_dir)
            .expect("run dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "a freshly created socket directory is 0700");
    }

    /// The stale socket from a previous run is ours, so it must not be
    /// mistaken for a neighbour and block startup.
    #[test]
    fn a_stale_socket_does_not_make_the_directory_look_shared() {
        let dir = tempfile::tempdir().expect("tempdir");
        let run_dir = dir.path().join("run");
        std::fs::create_dir(&run_dir).expect("create run dir");
        let socket = run_dir.join("agent.sock");
        // A real socket, as a previous run would have left: the type is what
        // `remove_stale_socket` keys on, so a placeholder file would not
        // exercise the same path.
        let listener = std::os::unix::net::UnixListener::bind(&socket).expect("stale socket");
        drop(listener);

        prepare_socket_dir(&run_dir, &socket.to_string_lossy())
            .expect("a directory holding only our own socket is still dedicated");
        remove_stale_socket(&socket.to_string_lossy()).expect("our own stale socket is removable");
        assert!(!socket.exists(), "the stale socket should be gone");
    }

    /// Removing whatever happens to sit at the configured path would hand an
    /// attacker who can write the directory a privileged unlink, and would
    /// quietly destroy an operator's file after a mistyped path.
    #[test]
    fn a_non_socket_at_the_socket_path_is_refused_not_deleted() {
        let dir = tempfile::tempdir().expect("tempdir");
        let occupied = dir.path().join("agent.sock");
        std::fs::write(&occupied, b"not a socket").expect("write regular file");

        let err = remove_stale_socket(&occupied.to_string_lossy())
            .expect_err("a regular file must not be removed");
        assert!(
            err.to_string().contains("not a socket"),
            "the error should say why, got: {err}"
        );
        assert!(occupied.exists(), "the file must be left untouched");
    }

    /// An existing entry with the configured socket name must be a socket
    /// before it makes the parent look dedicated. Otherwise we would tighten
    /// the directory and only then reject the regular file or symlink.
    #[test]
    fn a_non_socket_in_the_socket_directory_is_refused_before_restricting() {
        use std::os::unix::fs::PermissionsExt;

        for occupant in ["regular file", "symlink"] {
            let dir = tempfile::tempdir().expect("tempdir");
            let run_dir = dir.path().join("run");
            std::fs::create_dir(&run_dir).expect("create run dir");
            std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o755))
                .expect("loosen run dir");
            let socket = run_dir.join("agent.sock");
            match occupant {
                "regular file" => {
                    std::fs::write(&socket, b"not a socket").expect("write regular file");
                }
                "symlink" => {
                    std::os::unix::fs::symlink("someone-elses.sock", &socket)
                        .expect("create symlink");
                }
                _ => unreachable!("test cases are exhaustive"),
            }

            let err = prepare_socket_dir(&run_dir, &socket.to_string_lossy())
                .expect_err("a non-socket must not make the directory look dedicated");
            assert!(
                err.to_string().contains("not a socket"),
                "{occupant}: the error should say why, got: {err}"
            );
            let mode = std::fs::metadata(&run_dir)
                .expect("run dir metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(
                mode, 0o755,
                "{occupant}: a refused directory must be left exactly as it was found"
            );
        }
    }

    /// A missing socket is the normal first-boot case, not an error.
    #[test]
    fn a_missing_socket_path_is_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agent.sock");
        remove_stale_socket(&socket.to_string_lossy()).expect("nothing to remove is fine");
    }

    /// A symlinked directory would redirect the 0700 onto a target chosen by
    /// whoever planted it.
    #[test]
    fn a_symlinked_socket_directory_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("elsewhere");
        std::fs::create_dir(&real).expect("create target dir");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755))
            .expect("loosen target");
        let link = dir.path().join("run");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let err = prepare_socket_dir(&link, &link.join("agent.sock").to_string_lossy())
            .expect_err("a symlinked directory must be refused");
        assert!(
            err.to_string().contains("symlink"),
            "the error should say why, got: {err}"
        );

        let mode = std::fs::metadata(&real)
            .expect("target metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o755,
            "the symlink target must not be re-permissioned"
        );
    }
}
