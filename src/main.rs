use anyhow::Result;
use once_cell::sync::Lazy;
use std::sync::{atomic::AtomicU64, Mutex};

use hyper::{
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server,
};

use prometheus_client::{
    encoding::text::{encode, Encode},
    metrics::family::Family,
    metrics::gauge::Gauge,
    registry::Registry,
};

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use std::{
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
};
use tokio::signal::unix::{signal, SignalKind};
use tower::ServiceBuilder;
use tracing::*;

use kube::{
    api::ListParams,
    client::ConfigExt,
    core::{DynamicObject, GroupVersionKind},
    discovery::ApiResource,
    Api, Client, Config,
};
use tokio_cron_scheduler::{Job, JobScheduler};

#[derive(Clone, Hash, PartialEq, Eq, Encode)]
pub struct Labels {
    pub kind: String,
    pub version: String,
    pub group: String,
}

#[derive(Default)]
pub struct Metrics {
    crds: Family<Labels, Gauge<f64, AtomicU64>>,
    numcrds: Gauge,
}

impl Metrics {
    pub fn set_crd(&self, kind: String, version: String, group: String, count: f64) {
        self.crds
            .get_or_create(&Labels {
                kind,
                version,
                group,
            })
            .set(count);
    }

    pub fn set_numcrds(&self, count: u64) {
        self.numcrds.set(count);
    }
}

static METRICS: Lazy<Mutex<Metrics>> = Lazy::new(|| Mutex::new(Metrics::default()));

fn set_crd(kind: String, version: String, group: String, count: f64) {
    METRICS.lock().unwrap().set_crd(kind, version, group, count);
}

fn set_numcrds(count: u64) {
    METRICS.lock().unwrap().set_numcrds(count);
}

fn get_crd_clone() -> Family<Labels, Gauge<f64, AtomicU64>> {
    METRICS.lock().unwrap().crds.clone()
}

fn get_numcrd_clone() -> Gauge {
    METRICS.lock().unwrap().numcrds.clone()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let mut registry = <Registry>::with_prefix("crdmon");

    registry.register(
        "crd",
        "Count of active CRD objects",
        Box::new(get_crd_clone()),
    );
    registry.register("numcrds", "Count of CRDs", Box::new(get_numcrd_clone()));

    info!("Startup");
    get_cr_definitions().await?;
    info!("Startup: fetched CustomResourceDefinitions");

    let sched = JobScheduler::new().await?;

    sched
        .add(Job::new_async("*/10 * * * * *", |_, _| {
            Box::pin(async move {
                info!("I run async every 10 seconds");
                match get_cr_definitions().await {
                    Ok(_) => info!("Cron: fetched CustomResourceDefinitions"),
                    Err(e) => warn!("Cron: cannot fetch CustomResourceDefinitions: {}", e),
                }
            })
        })?)
        .await
        .unwrap();

    // Spawn a server to serve the OpenMetrics endpoint.
    let metrics_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
    sched.start().await?;

    start_metrics_server(metrics_addr, registry).await;
    Ok(())
}

async fn get_cr_definitions() -> Result<()> {
    let config = Config::infer().await?;

    let https = config.rustls_https_connector()?;

    let service = ServiceBuilder::new()
        .layer(config.base_uri_layer())
        .option_layer(config.auth_layer()?)
        .service(hyper::Client::builder().build(https));
    let client: Client = Client::new(service, config.default_namespace);

    let crdapi: Api<CustomResourceDefinition> = Api::all(client.clone());
    let crdlist = crdapi.list(&Default::default()).await?;
    let params = ListParams::default().limit(1);

    set_numcrds(crdlist.items.len().clone() as u64);
    for c in crdlist {
        for v in &c.spec.versions {
            if !v.served {
                continue;
            }
            match v.deprecated {
                Some(deprecated) => {
                    if deprecated {
                        continue;
                    }
                }
                None => (),
            }

            let gvk = GroupVersionKind::gvk(
                c.spec.group.as_str(),
                v.name.as_str(),
                (c.spec).names.kind.as_str(),
            );
            let api_resource =
                ApiResource::from_gvk_with_plural(&gvk, (c.spec).names.plural.as_str());
            let api: Api<DynamicObject> = Api::all_with(client.clone(), &api_resource);
            match api.list(&params).await {
                Ok(crd_items) => {
                    let mut num_items = crd_items.items.len();
                    match crd_items.metadata.remaining_item_count {
                        Some(remaining_item_count) => {
                            num_items = num_items + remaining_item_count as usize;
                        }
                        None => (),
                    }
                    set_crd(
                        format!("{}", (c.spec).names.kind),
                        format!("{}", v.name),
                        format!("{}", c.spec.group),
                        num_items as f64,
                    );
                }
                Err(e) => {
                    warn!(
                        "Got error listing {} {} {} : {}\n{:?}",
                        (c.spec).names.singular.as_ref().unwrap().clone(),
                        v.name.clone(),
                        c.spec.group.clone(),
                        e,
                        api
                    );
                    set_crd(
                        (c.spec).names.kind.clone(),
                        v.name.clone(),
                        c.spec.group.clone(),
                        -1.0,
                    );
                }
            }
        }
    }

    Ok(())
}

/// Start a HTTP server to report metrics.
pub async fn start_metrics_server(metrics_addr: SocketAddr, registry: Registry) {
    let mut shutdown_stream = signal(SignalKind::terminate()).unwrap();

    eprintln!("Starting metrics server on {metrics_addr}");

    let registry = Arc::new(registry);
    Server::bind(&metrics_addr)
        .serve(make_service_fn(move |_conn| {
            let registry = registry.clone();
            async move {
                let handler = make_handler(registry);
                Ok::<_, io::Error>(service_fn(handler))
            }
        }))
        .with_graceful_shutdown(async move {
            shutdown_stream.recv().await;
        })
        .await
        .unwrap();
}

/// This function returns a HTTP handler (i.e. another function)
pub fn make_handler(
    registry: Arc<Registry>,
) -> impl Fn(Request<Body>) -> Pin<Box<dyn Future<Output = io::Result<Response<Body>>> + Send>> {
    // This closure accepts a request and responds with the OpenMetrics encoding of our metrics.
    move |_req: Request<Body>| {
        let reg = registry.clone();
        Box::pin(async move {
            let mut buf = Vec::new();
            encode(&mut buf, &reg.clone()).map(|_| {
                let body = Body::from(buf);
                Response::builder()
                    .header(
                        hyper::header::CONTENT_TYPE,
                        "application/openmetrics-text; version=1.0.0; charset=utf-8",
                    )
                    .body(body)
                    .unwrap()
            })
        })
    }
}
