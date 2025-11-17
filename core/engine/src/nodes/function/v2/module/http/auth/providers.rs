use crate::nodes::function::v2::module::http::auth::{AwsIamAuth, AzureIamAuth, GcpIamAuth};
use ::http::Request as HttpRequest;
use anyhow::Context;
use async_trait::async_trait;
use http::HeaderValue;
use reqsign::{aws, azure, google};
use reqwest::{Body, Request};
use sha2::{Digest, Sha256};
use std::fmt::Debug;
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

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

impl AwsIamAuth {
    pub async fn build_request(&self, http_request: HttpRequest<Body>) -> anyhow::Result<Request> {
        static CACHED_PROVIDER: OnceLock<CachedProvider<aws::DefaultCredentialProvider>> =
            OnceLock::new();
        let provider = CACHED_PROVIDER
            .get_or_init(|| CachedProvider(Arc::new(aws::DefaultCredentialProvider::new())))
            .clone();

        let signer = aws::default_signer(self.service.deref(), self.region.0.deref())
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
