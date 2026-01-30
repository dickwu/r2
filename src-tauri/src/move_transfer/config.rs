use crate::providers::{aws, minio};
use crate::r2::R2Config;

#[derive(Debug, Clone)]
pub(crate) enum MoveConfig {
    R2(R2Config),
    Aws(aws::AwsConfig),
    Minio(minio::MinioConfig),
    Rustfs(minio::MinioConfig),
}
