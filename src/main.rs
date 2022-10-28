use prometheus_client::encoding::text::{encode, Encode};
use prometheus_client::metrics::gauge::Gauge;
// use prometheus_client::metrics::family::Family;
use prometheus_client::registry::Registry;

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use tower::ServiceBuilder;
use tracing::*;

use kube::{client::ConfigExt, Api, Client, Config, ResourceExt};

#[derive(Clone, Hash, PartialEq, Eq, Encode)]
pub struct Labels {
    pub group: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::infer().await?;

    let mut registry = Registry::default();

    // Pick TLS at runtime
    let https = config.rustls_https_connector()?;
    let service = ServiceBuilder::new()
        .layer(config.base_uri_layer())
        .option_layer(config.auth_layer()?)
        .service(hyper::Client::builder().build(https));
    let client:Client = Client::new(service, config.default_namespace);

    let crds: Api<CustomResourceDefinition> = Api::all(client);
    for c in crds.list(&Default::default()).await? {
        for v in &c.spec.versions
        {

            let metric_data: Gauge<u64> = Default::default();
            let metric_name = format!("{}_{}", c.name_any().replace('.', "_"), v.name);
            let metric_description = format!("Count of active CRD objects for {}.{}.{} (-1 on error)", &(c.spec).names.singular.as_ref().unwrap(), v.name, c.spec.group);

            // info!("{} \"{}\" {}", &metric_name, &metric_description, c.creation_timestamp().unwrap().0);

            registry.register(
                metric_name,
                metric_description,
                metric_data.clone(),
            );

            metric_data.set(1);
        }
    }

    let mut buffer = vec![];
    encode(&mut buffer, &registry).unwrap();

    info!("{}", String::from_utf8(buffer).unwrap());
    Ok(())
}
