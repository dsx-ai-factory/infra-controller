// Covers `NvRedfishClientPool::service_root_with_cache_predicate`: a fetched
// ServiceRoot is cached only when the caller's predicate accepts it.
//
// Some BMCs transiently serve a service root with
// navigation properties missing while resetting or booting. Even with a
// finite TTL, rejecting an incomplete root avoids poisoning explorations until
// that TTL elapses. The mock BMC here serves a `Chassis`-less root first
// (rejected by the predicate, not cached), then a full root (re-fetched,
// accepted, and served from cache afterwards).

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum_server::tls_rustls::RustlsConfig;
use carbide_redfish::nv_redfish::NvRedfishClientPool;
use carbide_secrets::credentials::Credentials;

// A full, healthy AMI service root including the `Chassis` navigation property.
const FULL_ROOT: &str = r##"{
  "@odata.id": "/redfish/v1/",
  "@odata.type": "#ServiceRoot.v1_13_0.ServiceRoot",
  "Id": "RootService",
  "Name": "Root Service",
  "RedfishVersion": "1.15.1",
  "Vendor": "AMI",
  "Product": "AMI Redfish Server",
  "UUID": "6acb216c-bfde-11d3-02c0-146dd4e0ff10",
  "Chassis": { "@odata.id": "/redfish/v1/Chassis" },
  "Systems": { "@odata.id": "/redfish/v1/Systems" },
  "Managers": { "@odata.id": "/redfish/v1/Managers" },
  "UpdateService": { "@odata.id": "/redfish/v1/UpdateService" },
  "SessionService": { "@odata.id": "/redfish/v1/SessionService" },
  "Links": { "Sessions": { "@odata.id": "/redfish/v1/SessionService/Sessions" } },
  "ProtocolFeaturesSupported": {
    "ExpandQuery": { "ExpandAll": true, "Levels": true, "Links": true, "MaxLevels": 5, "NoLinks": true },
    "FilterQuery": true,
    "SelectQuery": true
  }
}"##;

// The same root as served transiently during a BMC reset/boot: every nav
// property is the same EXCEPT `Chassis`, which the BMC omits.
const STRIPPED_ROOT: &str = r##"{
  "@odata.id": "/redfish/v1/",
  "@odata.type": "#ServiceRoot.v1_13_0.ServiceRoot",
  "Id": "RootService",
  "Name": "Root Service",
  "RedfishVersion": "1.15.1",
  "Vendor": "AMI",
  "Product": "AMI Redfish Server",
  "UUID": "6acb216c-bfde-11d3-02c0-146dd4e0ff10",
  "Systems": { "@odata.id": "/redfish/v1/Systems" },
  "Managers": { "@odata.id": "/redfish/v1/Managers" },
  "UpdateService": { "@odata.id": "/redfish/v1/UpdateService" },
  "SessionService": { "@odata.id": "/redfish/v1/SessionService" },
  "Links": { "Sessions": { "@odata.id": "/redfish/v1/SessionService/Sessions" } },
  "ProtocolFeaturesSupported": {
    "ExpandQuery": { "ExpandAll": true, "Levels": true, "Links": true, "MaxLevels": 5, "NoLinks": true },
    "FilterQuery": true,
    "SelectQuery": true
  }
}"##;

const CHASSIS_COLLECTION: &str = r##"{
  "@odata.id": "/redfish/v1/Chassis",
  "@odata.type": "#ChassisCollection.ChassisCollection",
  "Name": "Chassis Collection",
  "Members@odata.count": 0,
  "Members": []
}"##;

#[derive(Clone)]
struct AppState {
    root_hits: Arc<AtomicUsize>,
}

async fn service_root(State(state): State<AppState>) -> impl IntoResponse {
    // First request -> stripped (BMC mid-reset); subsequent -> full (recovered).
    let n = state.root_hits.fetch_add(1, Ordering::SeqCst);
    let body = if n == 0 { STRIPPED_ROOT } else { FULL_ROOT };
    ([(header::CONTENT_TYPE, "application/json")], body)
}

async fn chassis_collection() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        CHASSIS_COLLECTION,
    )
}

fn spawn_mock_bmc(root_hits: Arc<AtomicUsize>) -> SocketAddr {
    let app = Router::new()
        .route("/redfish/v1", get(service_root))
        .route("/redfish/v1/", get(service_root))
        .route("/redfish/v1/Chassis", get(chassis_collection))
        .with_state(AppState { root_hits });

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();

    let tls = bmc_mock::tls::server_config(None::<&str>).unwrap();
    let config = RustlsConfig::from_config(Arc::new(tls));
    tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, config)
            .unwrap()
            .serve(app.into_make_service())
            .await
            .unwrap();
    });

    addr
}

#[tokio::test]
async fn poisoned_service_root_cache_recovers_after_bmc_heals() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    let root_hits = Arc::new(AtomicUsize::new(0));
    let addr = spawn_mock_bmc(root_hits.clone());

    let pool = NvRedfishClientPool::new(Arc::new(ArcSwap::new(Arc::new(None))));
    let creds = || Credentials::UsernamePassword {
        username: "root".to_string(),
        password: "placeholder".to_string(),
    };

    // Cache predicate as used by site-explorer's exploration path: only roots
    // exposing `Chassis` and `Managers` are worth caching.
    let root_has_chassis = |root: &carbide_redfish::nv_redfish::ServiceRoot| {
        root.root.chassis.is_some() && root.root.managers.is_some()
    };

    // 1st exploration: the BMC transiently serves a Chassis-less root, which
    // must NOT be cached.
    let first = pool
        .service_root_with_cache_predicate(addr, Some(creds()), root_has_chassis)
        .await
        .unwrap();
    assert!(
        first.chassis_links().await.unwrap().is_none(),
        "precondition: the first root must be the stripped (Chassis-less) one",
    );
    assert_eq!(root_hits.load(Ordering::SeqCst), 1);

    // The BMC has recovered and now serves a complete root. Since the stripped
    // root was rejected by the cache predicate, the pool must re-fetch and
    // observe the recovered root.
    let second = pool
        .service_root_with_cache_predicate(addr, Some(creds()), root_has_chassis)
        .await
        .unwrap();

    assert_eq!(
        root_hits.load(Ordering::SeqCst),
        2,
        "BUG: pool never re-fetched the service root; it served the cached \
         Chassis-less root from the BMC's reset window",
    );
    assert!(
        second.chassis_links().await.unwrap().is_some(),
        "BUG: pool returned a Chassis-less ServiceRoot even though the BMC has \
         recovered -- this is what surfaces as 'BMC has not provided chassis collection'",
    );

    // The recovered root passed the predicate, so it IS cached now: a third
    // call must be served from cache with no extra fetch.
    let third = pool
        .service_root_with_cache_predicate(addr, Some(creds()), root_has_chassis)
        .await
        .unwrap();
    assert_eq!(root_hits.load(Ordering::SeqCst), 2);
    assert!(third.chassis_links().await.unwrap().is_some());
}

#[tokio::test]
async fn expired_service_root_is_refetched_without_invalidating_existing_holders() {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();

    // Start at one so the mock serves a complete root on every request.
    let root_hits = Arc::new(AtomicUsize::new(1));
    let addr = spawn_mock_bmc(root_hits.clone());
    let pool =
        NvRedfishClientPool::with_cache_ttl(Arc::new(ArcSwap::new(Arc::new(None))), Duration::ZERO);
    let creds = || Credentials::UsernamePassword {
        username: "root".to_string(),
        password: "placeholder".to_string(),
    };

    let first = pool.service_root(addr, Some(creds())).await.unwrap();
    let second = pool.service_root(addr, Some(creds())).await.unwrap();

    assert_eq!(
        root_hits.load(Ordering::SeqCst),
        3,
        "an expired service root should be fetched again",
    );
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(
        first.chassis_links().await.unwrap().is_some(),
        "eviction must not invalidate callers that still hold the old root",
    );
}
