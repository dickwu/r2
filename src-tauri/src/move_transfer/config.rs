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
