use crate::nodes::function::v2::error::ResultExt;
use crate::nodes::function::v2::module::http::HttpConfig;
use ::http::Request as HttpRequest;
use anyhow::Context;
use async_trait::async_trait;
use http::HeaderValue;
use reqsign::{aws, azure, google};
use reqwest::{Body, Request};
use rquickjs::{Ctx, FromJs, Value};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

#[derive(Deserialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum HttpConfigAuth {
    #[serde(rename = "iam")]
    Iam(IamAuth),
}

#[derive(Deserialize, Clone)]
#[serde(tag = "provider", rename_all = "camelCase")]
pub(crate) enum IamAuth {
    Aws(AwsIamAuth),
    Azure(AzureIamAuth),
    Gcp(GcpIamAuth),
}

impl<'js> FromJs<'js> for HttpConfig {
    fn from_js(ctx: &Ctx<'js>, value: Value<'js>) -> rquickjs::Result<Self> {
        rquickjs_serde::from_value(value).or_throw(&ctx)
    }
}

#[derive(Debug)]
struct CachedProvider<Provider>(Arc<Provider>)
where
    Provider: reqsign::ProvideCredential + Debug;

#[async_trait]
impl<Provider> reqsign::ProvideCredential for CachedProvider<Provider>
where
    Provider: reqsign::ProvideCredential + Debug,
{
    type Credential = Provider::Credential;

    async fn provide_credential(
        &self,
        ctx: &reqsign::Context,
    ) -> reqsign::Result<Option<Self::Credential>> {
        self.0.provide_credential(ctx).await
    }
}

impl<Provider> Clone for CachedProvider<Provider>
where
    Provider: reqsign::ProvideCredential + Debug,
{
    fn clone(&self) -> Self {
        CachedProvider(self.0.clone())
    }
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AwsIamAuth {
    region: Arc<str>,
    service: Arc<str>,
}

impl AwsIamAuth {
    pub async fn build_request(&self, http_request: HttpRequest<Body>) -> anyhow::Result<Request> {
        static CACHED_PROVIDER: OnceLock<CachedProvider<aws::DefaultCredentialProvider>> =
            OnceLock::new();
        let provider = CACHED_PROVIDER
            .get_or_init(|| CachedProvider(Arc::new(aws::DefaultCredentialProvider::new())))
            .clone();

        let signer = aws::default_signer(self.service.deref(), self.region.deref())
            .with_credential_provider(provider);

        let (mut parts, body) = http_request.into_parts();
        let payload_hash_opt = body
            .as_bytes()
            .map(|body_bytes| format!("{:x}", Sha256::digest(&body_bytes)));

        if let Some(payload_hash) = payload_hash_opt {
            parts.headers.insert(
                "x-amz-content-sha256",
                HeaderValue::from_str(payload_hash.as_str())?,
            );
        }

        signer
            .sign(&mut parts, None)
            .await
            .context("Failed to sign request body")?;

        let new_http_request = HttpRequest::from_parts(parts, body);
        Request::try_from(new_http_request).context("Failed to create request")
    }
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GcpIamAuth {
    service: Arc<str>,
}

impl GcpIamAuth {
    pub async fn build_request(&self, http_request: HttpRequest<Body>) -> anyhow::Result<Request> {
        static CACHED_PROVIDER: OnceLock<CachedProvider<google::DefaultCredentialProvider>> =
            OnceLock::new();
        let provider = CACHED_PROVIDER
            .get_or_init(|| CachedProvider(Arc::new(google::DefaultCredentialProvider::new())))
            .clone();

        let signer =
            google::default_signer(self.service.deref()).with_credential_provider(provider);
        let (mut parts, body) = http_request.into_parts();

        signer
            .sign(&mut parts, None)
            .await
            .context("Failed to sign request body")?;
        let new_http_request = HttpRequest::from_parts(parts, body);
        Request::try_from(new_http_request).context("Failed to create request")
    }
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AzureIamAuth;

impl AzureIamAuth {
    pub async fn build_request(&self, http_request: HttpRequest<Body>) -> anyhow::Result<Request> {
        static CACHED_PROVIDER: OnceLock<CachedProvider<azure::DefaultCredentialProvider>> =
            OnceLock::new();
        let provider = CACHED_PROVIDER
            .get_or_init(|| CachedProvider(Arc::new(azure::DefaultCredentialProvider::new())))
            .clone();

        let signer = azure::default_signer().with_credential_provider(provider);
        let (mut parts, body) = http_request.into_parts();

        signer
            .sign(&mut parts, None)
            .await
            .context("Failed to sign request body")?;
        let new_http_request = HttpRequest::from_parts(parts, body);
        Request::try_from(new_http_request).context("Failed to create request")
    }
}
