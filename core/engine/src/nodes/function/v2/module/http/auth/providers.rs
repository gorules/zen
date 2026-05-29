use crate::nodes::function::v2::module::http::auth::{AwsIamAuth, AzureIamAuth, GcpIamAuth};
use ::http::Request as HttpRequest;
use anyhow::Context;
use http::HeaderValue;
use reqsign::{aws, azure, google};
use reqwest::{Body, Request};
use sha2::{Digest, Sha256};
use std::ops::Deref;
use std::sync::{Arc, OnceLock};

impl AwsIamAuth {
    pub async fn build_request(&self, http_request: HttpRequest<Body>) -> anyhow::Result<Request> {
        static CACHED_PROVIDER: OnceLock<Arc<aws::DefaultCredentialProvider>> = OnceLock::new();
        let provider = CACHED_PROVIDER
            .get_or_init(|| Arc::new(aws::DefaultCredentialProvider::new()))
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
        static CACHED_PROVIDER: OnceLock<Arc<google::DefaultCredentialProvider>> = OnceLock::new();
        let provider = CACHED_PROVIDER
            .get_or_init(|| Arc::new(google::DefaultCredentialProvider::new()))
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
        static CACHED_PROVIDER: OnceLock<Arc<azure::DefaultCredentialProvider>> = OnceLock::new();
        let provider = CACHED_PROVIDER
            .get_or_init(|| Arc::new(azure::DefaultCredentialProvider::new()))
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

#[cfg(all(test, not(miri)))]
mod tests {
    use super::*;
    use crate::nodes::function::v2::module::http::auth::AwsRegion;
    use ::http::Request as HttpRequest;
    use reqwest::Body;

    #[tokio::test]
    async fn aws_iam_produces_sigv4_authorization_header() {
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE");
        std::env::set_var(
            "AWS_SECRET_ACCESS_KEY",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        );
        std::env::set_var("AWS_REGION", "us-east-1");

        let auth = AwsIamAuth {
            region: AwsRegion("us-east-1".into()),
            service: "s3".into(),
        };
        let req = HttpRequest::builder()
            .method("GET")
            .uri("https://example-bucket.s3.amazonaws.com/key")
            .body(Body::from("hello"))
            .expect("build request");

        let signed = auth
            .build_request(req)
            .await
            .expect("signing should succeed");

        assert!(
            signed.headers().contains_key("authorization"),
            "expected Authorization header"
        );
        assert!(signed.headers().contains_key("x-amz-content-sha256"));
        assert!(signed.headers().contains_key("x-amz-date"));
        let auth_hdr = signed
            .headers()
            .get("authorization")
            .expect("auth header present")
            .to_str()
            .expect("auth header is ascii");
        assert!(
            auth_hdr.starts_with("AWS4-HMAC-SHA256"),
            "unexpected auth scheme: {auth_hdr}"
        );
    }

    #[tokio::test]
    async fn gcp_iam_build_request_does_not_panic() {
        let auth = GcpIamAuth {
            service: "storage".into(),
        };
        let req = HttpRequest::builder()
            .method("GET")
            .uri("https://storage.googleapis.com/b/foo/o/bar")
            .body(Body::from(""))
            .expect("build request");

        // Without GOOGLE_APPLICATION_CREDENTIALS the provider returns no creds;
        // we only assert the signer plumbing runs without panicking.
        let _ = auth.build_request(req).await;
    }

    // Ignored by default: Azure's DefaultCredentialProvider probes IMDS/MSI
    // endpoints when no creds are present, adding ~60s wall time. Run with
    // `cargo test -- --ignored` to exercise it on demand.
    #[tokio::test]
    #[ignore]
    async fn azure_iam_build_request_does_not_panic() {
        let auth = AzureIamAuth;
        let req = HttpRequest::builder()
            .method("GET")
            .uri("https://account.blob.core.windows.net/container/blob")
            .body(Body::from(""))
            .expect("build request");

        let _ = auth.build_request(req).await;
    }
}
