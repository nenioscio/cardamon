use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use tower::ServiceBuilder;
use tracing::*;

use kube::{client::ConfigExt, Api, Client, Config, ResourceExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let config = Config::infer().await?;

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
            info!("{}_{} {}.{}.{} {}", c.name_any().replace('.', "_"), v.name, &(c.spec).names.singular.as_ref().unwrap(), v.name, c.spec.group, c.creation_timestamp().unwrap().0);
        }
    }

    Ok(())
}
