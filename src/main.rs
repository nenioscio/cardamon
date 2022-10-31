
use anyhow::{bail, Context, Result};

use hyper::{
    service::{make_service_fn, service_fn},
    Body, Request, Response, Server,
};
use prometheus_client::{encoding::text::{encode, Encode}, metrics::family::Family, metrics::gauge::Gauge, registry::Registry};

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use tower::ServiceBuilder;
use std::{
    future::Future,
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::Arc,
};
use tokio::signal::unix::{signal, SignalKind};
use tracing::*;

use kube::{client::ConfigExt, Api, Client, Config, ResourceExt};

#[derive(Clone, Hash, PartialEq, Eq, Encode)]
pub struct Labels {
    pub name: String,
    pub version: String,
    pub group: String,
}

#[derive(Default)]
pub struct Metrics {
    crds: Family<Labels, Gauge>,
    numcrds: Gauge,
}

impl Metrics {
    pub fn set_crd(&self, name: String, version: String, group: String, count: u64) {
        self.crds.get_or_create(&Labels { name, version, group }).set(count);
    }
    
    pub fn set_numcrds(&self, count: u64) {
        self.numcrds.set(count);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let config = Config::infer().await?;

    let mut registry = <Registry>::with_prefix("crdmon");
    let metrics: Metrics = Default::default();

    let mut sched = JobScheduler::new();


    // Pick TLS at runtime
    let https = config.rustls_https_connector()?;
    let service = ServiceBuilder::new()
        .layer(config.base_uri_layer())
        .option_layer(config.auth_layer()?)
        .service(hyper::Client::builder().build(https));
    let client:Client = Client::new(service, config.default_namespace);


    sched.add(Job::new_async("0 */10 * * * *", |uuid, _l| Box::pin( async {
        println!("I run async every 10 minutes");
        let crds: Api<CustomResourceDefinition> = Api::all(client);
        for c in crds.list(&Default::default()).await? {
            for v in &c.spec.versions
            {
                metrics.set_crd((c.spec).names.singular.as_ref().unwrap().clone(), v.name.clone(), c.spec.group.clone(), 1);
            }
        }    
    })).await.unwrap());

    registry.register(
        "crd",
        "Count of active CRD objects",
        Box::new(metrics.crds.clone()),
    );
    registry.register(
        "numcrds",
        "Count of CRDs",
        Box::new(metrics.numcrds.clone())
    );

    //Ok(())
    // Spawn a server to serve the OpenMetrics endpoint.
    let metrics_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
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
            let mut buf = Vec::new();
            encode(&mut buf, &reg.clone()).map(|_| {
                info!("{}", String::from_utf8(buf.clone()).unwrap());
            
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
