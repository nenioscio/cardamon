use anyhow::{bail, Result};
use futures::lock::Mutex;
use once_cell::sync::Lazy;
use std::{collections::HashMap, sync::atomic::AtomicU64};

use hyper::{
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server,
};

use prometheus_client::{
    encoding::{text::encode, EncodeLabelSet},
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
use tracing::*;

use kube::{
    api::ListParams,
    core::{DynamicObject, GroupVersionKind},
    discovery::ApiResource,
    Api, Client,
};
use tokio_cron_scheduler::{Job, JobScheduler};

// #[cfg(not(target_env = "msvc"))]
// use jemallocator::Jemalloc;

// #[cfg(not(target_env = "msvc"))]
// #[global_allocator]
// static GLOBAL: Jemalloc = Jemalloc;

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug)]
pub struct Labels {
    pub kind: String,
    pub version: String,
    pub group: String,
}

#[derive(Default)]
pub struct CrdMetrics {
    crds: Family<Labels, Gauge<f64, AtomicU64>>,
    numcrds: Gauge,
    kube_client: Option<Client>,
    rediscovered: HashMap<Labels, bool>,
}

impl CrdMetrics {
    pub fn set_crd(&mut self, kind: String, version: String, group: String, count: f64) {
        let label = Labels {
            kind,
            version,
            group,
        };
        self.crds.get_or_create(&label).set(count);
        self.rediscovered
            .entry(label)
            .and_modify(|v| *v = true)
            .or_insert(true);
    }

    pub fn reset_discovered(&mut self) {
        for v in self.rediscovered.values_mut() {
            *v = false;
        }
    }

    pub fn remove_absent(&self) {
        for (k, v) in &self.rediscovered {
            if !v {
                self.crds.remove(&(k.clone()));
            }
        }
    }

    pub async fn init_client(&mut self) -> Result<()> {
        self.kube_client = Some(Client::try_default().await?);

        Ok(())
    }

    pub fn set_numcrds(&self, count: i64) {
        self.numcrds.set(count);
    }

    async fn get_cr_definitions(&mut self) -> Result<i64> {
        let crdapi: Api<CustomResourceDefinition> =
            Api::all(self.kube_client.as_ref().unwrap().clone());
        let crdlist = crdapi.list(&Default::default()).await?;
        let num_crds = crdlist.items.len() as i64;
        self.set_numcrds(num_crds);

        self.reset_discovered();
        let params = ListParams::default().limit(1);
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
                    c.spec.group.clone().as_str(),
                    v.name.clone().as_str(),
                    c.spec.names.kind.clone().as_str(),
                );
                let api_resource = ApiResource::from_gvk_with_plural(
                    &gvk,
                    c.spec.names.plural.clone().as_str(),
                );
                let api: Api<DynamicObject> =
                    Api::all_with(self.kube_client.as_ref().unwrap().clone(), &api_resource);
                match api.list(&params).await {
                    Ok(crd_items) => {
                        let mut num_items = crd_items.items.len();
                        match crd_items.metadata.remaining_item_count {
                            Some(remaining_item_count) => {
                                num_items = num_items + remaining_item_count as usize;
                            }
                            None => (),
                        }
                        self.set_crd(
                            c.spec.names.kind.clone().to_string(),
                            v.name.clone().to_string(),
                            c.spec.group.clone().to_string(),
                            num_items as f64,
                        );
                    }
                    Err(e) => match e {
                        kube::Error::Api(api_error) => {
                            warn!(
                                "Got ApiError({}) for listing {} {} {} : {}",
                                api_error.code,
                                c.spec.names.singular.as_ref().unwrap().clone(),
                                v.name.clone(),
                                c.spec.group.clone(),
                                api_error
                            );
                            if api_error.code == 500 {
                                warn!(
                                        "Detected API error(500) for CRD::list({} {} {}) reporting with broken metric", 
                                        c.spec.names.singular.as_ref().unwrap().clone(),
                                        v.name.clone(),
                                        c.spec.group.clone(),
                                    );
                                self.set_crd(
                                    c.spec.names.kind.clone().as_str().to_string(),
                                    v.name.clone().as_str().to_string(),
                                    c.spec.group.clone().as_str().to_string(),
                                    -1.0,
                                );
                            }
                        }
                        _ => bail!(e),
                    },
                }
            }
        }
        self.remove_absent();

        Ok(num_crds)
    }
}

static METRICS: Lazy<Mutex<CrdMetrics>> = Lazy::new(|| Mutex::new(CrdMetrics::default()));

async fn get_crd_clone() -> Family<Labels, Gauge<f64, AtomicU64>> {
    METRICS.lock().await.crds.clone()
}

async fn get_numcrd_clone() -> Gauge {
    METRICS.lock().await.numcrds.clone()
}

async fn init_client() -> Result<()> {
    METRICS.lock().await.init_client().await?;

    Ok(())
}

async fn get_cr_definitions() -> Result<i64> {
    METRICS.lock().await.get_cr_definitions().await
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let mut registry = <Registry>::with_prefix("cardamon");

    registry.register(
        "crd",
        "Count of active CRD objects",
        get_crd_clone().await,
    );
    registry.register(
        "numcrds",
        "Count of CRDs",
        get_numcrd_clone().await,
    );

    info!("Startup");
    init_client().await?;
    get_cr_definitions().await?;
    info!("Startup: fetched CustomResourceDefinitions");

    let sched = JobScheduler::new().await?;

    sched
        .add(Job::new_async("0 */1 * * * *", |_, _| {
            Box::pin(async move {
                info!("Cron: fetching CustomResourceDefinitions and number of objects");
                match get_cr_definitions().await {
                    Ok(num_crds) => info!("Cron: fetched {} CustomResourceDefinitions", num_crds),
                    Err(e) => warn!("Cron: cannot fetch CustomResourceDefinitions: {}", e),
                }
            })
        })?)
        .await
        .unwrap();

    // Spawn a server to serve the OpenMetrics endpoint.
    let metrics_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 8000);
    sched.start().await?;

    start_metrics_server(metrics_addr, registry).await;
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
            let mut buf = String::new();
            encode(&mut buf, &reg.clone())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
                .map(|_| {
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
