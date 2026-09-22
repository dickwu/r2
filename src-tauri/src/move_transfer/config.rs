use crate::providers::{aws, minio};
use crate::r2::R2Config;

#[derive(Debug, Clone)]
pub(crate) enum MoveConfig {
    R2(R2Config),
    Aws(aws::AwsConfig),
    Minio(minio::MinioConfig),
    Rustfs(minio::MinioConfig),
}

impl MoveConfig {
    pub(crate) fn operation_endpoint(&self) -> String {
        match self {
            Self::R2(cfg) => format!("r2:{}", cfg.account_id.trim().to_ascii_lowercase()),
            Self::Aws(cfg)
                if cfg
                    .endpoint_host
                    .as_deref()
                    .is_none_or(|host| host.trim().is_empty()) =>
            {
                format!("aws:{}", cfg.region.trim().to_ascii_lowercase())
            }
            Self::Aws(cfg) => physical_operation_endpoint(
                cfg.endpoint_scheme.as_deref().unwrap_or("https"),
                cfg.endpoint_host.as_deref().unwrap_or_default(),
                &cfg.bucket,
            ),
            Self::Minio(cfg) | Self::Rustfs(cfg) => {
                physical_operation_endpoint(&cfg.endpoint_scheme, &cfg.endpoint_host, &cfg.bucket)
            }
        }
    }

    pub(crate) fn capability_scope(&self) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        let (key, secret) = match self {
            Self::R2(cfg) => (&cfg.access_key_id, &cfg.secret_access_key),
            Self::Aws(cfg) => (&cfg.access_key_id, &cfg.secret_access_key),
            Self::Minio(cfg) | Self::Rustfs(cfg) => (&cfg.access_key_id, &cfg.secret_access_key),
        };
        let mut digest = Sha256::new();
        for value in [super::planner::scope(self)?.as_str(), key, secret] {
            digest.update((value.len() as u64).to_le_bytes());
            digest.update(value.as_bytes());
        }
        Ok(hex::encode(digest.finalize()))
    }

    pub(crate) async fn supports_condition(
        &self,
        condition: crate::providers::conditional::Condition,
    ) -> Result<bool, String> {
        if super::planner::native_aws(self) {
            return Ok(true);
        }
        let client = self.client().await?;
        crate::providers::conditional::supported(
            &client,
            self.bucket(),
            &self.capability_scope()?,
            condition,
        )
        .await
    }
    pub(crate) fn bucket(&self) -> &str {
        match self {
            Self::R2(cfg) => &cfg.bucket,
            Self::Aws(cfg) => &cfg.bucket,
            Self::Minio(cfg) | Self::Rustfs(cfg) => &cfg.bucket,
        }
    }

    pub(crate) async fn client(&self) -> Result<aws_sdk_s3::Client, String> {
        match self {
            Self::R2(cfg) => crate::r2::create_r2_client(cfg).await,
            Self::Aws(cfg) => aws::create_aws_client(cfg).await,
            Self::Minio(cfg) | Self::Rustfs(cfg) => minio::create_minio_client(cfg).await,
        }
        .map_err(|e| e.to_string())
    }
}

pub(crate) fn normalized_operation_endpoint(scheme: &str, host: &str) -> String {
    let scheme = scheme.trim().to_ascii_lowercase();
    let host = host.trim();
    let raw = if host.contains("://") {
        host.to_string()
    } else {
        format!("{scheme}://{host}")
    };
    match reqwest::Url::parse(&raw) {
        Ok(url) => {
            let mut endpoint = format!(
                "{}://{}",
                url.scheme().to_ascii_lowercase(),
                url.host_str().unwrap_or_default().to_ascii_lowercase()
            );
            if let Some(port) = url.port() {
                endpoint.push(':');
                endpoint.push_str(&port.to_string());
            }
            let path = url.path().trim_end_matches('/');
            if !path.is_empty() {
                endpoint.push_str(path);
            }
            endpoint
        }
        Err(_) => format!("{scheme}://{host}")
            .trim_end_matches('/')
            .to_ascii_lowercase(),
    }
}

/// The server a bucket's requests share. An endpoint entered with the bucket
/// as its last path segment (`host/tenant/bucket`) names the same server as
/// `host/tenant`, so endpoint budgets must not split per bucket.
pub(crate) fn physical_operation_endpoint(scheme: &str, host: &str, bucket: &str) -> String {
    let endpoint = normalized_operation_endpoint(scheme, host);
    let bucket = bucket.trim();
    if bucket.is_empty() {
        return endpoint;
    }
    match endpoint.strip_suffix(bucket) {
        Some(server) if server.ends_with('/') && !server.ends_with("://") => {
            server.trim_end_matches('/').to_string()
        }
        _ => endpoint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minio(endpoint_host: &str, bucket: &str) -> MoveConfig {
        MoveConfig::Minio(minio::MinioConfig {
            bucket: bucket.into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            endpoint_scheme: "https".into(),
            endpoint_host: endpoint_host.into(),
            force_path_style: true,
        })
    }

    fn aws_custom(endpoint_host: &str, bucket: &str) -> MoveConfig {
        MoveConfig::Aws(aws::AwsConfig {
            bucket: bucket.into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            region: "us-east-1".into(),
            endpoint_scheme: Some("https".into()),
            endpoint_host: Some(endpoint_host.into()),
            force_path_style: true,
        })
    }

    #[test]
    fn operation_endpoint_is_shared_across_buckets_and_normalizes_equivalent_urls() {
        assert_eq!(
            minio("EXAMPLE.test:443/Path/", "source").operation_endpoint(),
            minio("example.test/Path", "dest").operation_endpoint()
        );
        assert_eq!(
            minio("EXAMPLE.test:443/Path/", "source").operation_endpoint(),
            "https://example.test/Path"
        );
    }

    #[test]
    fn operation_endpoint_drops_only_a_whole_trailing_bucket_segment() {
        assert_eq!(
            minio("example.test/tenant/photos", "photos").operation_endpoint(),
            minio("example.test/tenant", "videos").operation_endpoint()
        );
        assert_eq!(
            minio("example.test/tenant/my-photos", "photos").operation_endpoint(),
            "https://example.test/tenant/my-photos"
        );
        assert_eq!(
            minio("photos", "photos").operation_endpoint(),
            "https://photos"
        );
    }

    #[test]
    fn operation_endpoint_distinguishes_endpoints_for_same_bucket() {
        assert_ne!(
            aws_custom("example.test", "bucket").operation_endpoint(),
            aws_custom("example.test:9443", "bucket").operation_endpoint()
        );
        assert_ne!(
            aws_custom("example.test/Path", "bucket").operation_endpoint(),
            aws_custom("example.test/path", "bucket").operation_endpoint()
        );
    }

    #[test]
    fn operation_endpoint_normalizes_native_regions_and_r2_accounts() {
        let aws = MoveConfig::Aws(aws::AwsConfig {
            bucket: "one".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
            region: " US-EAST-1 ".into(),
            endpoint_scheme: None,
            endpoint_host: None,
            force_path_style: false,
        });
        let r2 = MoveConfig::R2(R2Config {
            account_id: " AccountABC ".into(),
            bucket: "two".into(),
            access_key_id: "key".into(),
            secret_access_key: "secret".into(),
        });
        assert_eq!(aws.operation_endpoint(), "aws:us-east-1");
        assert_eq!(r2.operation_endpoint(), "r2:accountabc");
    }
}
