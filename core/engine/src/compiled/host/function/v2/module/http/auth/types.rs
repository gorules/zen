use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::{Arc, OnceLock};

#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "camelCase")]
pub(crate) enum HttpConfigAuth {
    #[serde(rename = "iam")]
    Iam(IamAuth),
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(tag = "provider", rename_all = "camelCase")]
pub(crate) enum IamAuth {
    Aws(AwsIamAuth),
    Azure(AzureIamAuth),
    Gcp(GcpIamAuth),
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AwsIamAuth {
    pub region: AwsRegion,
    pub service: Arc<str>,
}

#[derive(Clone)]
pub(crate) struct AwsRegion(pub Arc<str>);

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GcpIamAuth {
    pub service: Arc<str>,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AzureIamAuth;

impl<'de> Deserialize<'de> for AwsRegion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let region_str = Option::<Arc<str>>::deserialize(deserializer)?;
        match region_str {
            Some(region) => Ok(AwsRegion(region)),
            None => {
                static AWS_REGION_ENV: OnceLock<Option<Arc<str>>> = OnceLock::new();
                let aws_region_opt = AWS_REGION_ENV.get_or_init(|| {
                    std::env::var("AWS_REGION")
                        .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
                        .map(Arc::from)
                        .ok()
                });
                let Some(aws_region) = aws_region_opt else {
                    return Err(serde::de::Error::custom(
                        "AWS_REGION environment variable is missing - region parameter is required",
                    ));
                };

                Ok(AwsRegion(aws_region.clone()))
            }
        }
    }
}

impl Serialize for AwsRegion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}
