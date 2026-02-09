//! R2 list operations (buckets, objects)

use super::types::{create_r2_client, ListObjectsResult, R2Bucket, R2Config, R2Object, R2Result};

/// List all buckets in the R2 account
pub async fn list_buckets(config: &R2Config) -> R2Result<Vec<R2Bucket>> {
    let client = create_r2_client(config).await?;
    let response = client.list_buckets().send().await?;

    let buckets = response
        .buckets()
        .iter()
        .filter_map(|bucket| {
            let name = bucket.name()?.to_string();
            let creation_date = bucket
                .creation_date()
                .map(|dt| dt.to_string())
                .unwrap_or_default();
            Some(R2Bucket {
                name,
                creation_date,
            })
        })
        .collect();

    Ok(buckets)
}

/// List objects in a bucket with optional prefix and delimiter
pub async fn list_objects(
    config: &R2Config,
    prefix: Option<&str>,
    delimiter: Option<&str>,
    continuation_token: Option<&str>,
    max_keys: Option<i32>,
) -> R2Result<ListObjectsResult> {
    let client = create_r2_client(config).await?;

    let mut request = client
        .list_objects_v2()
        .bucket(&config.bucket)
        .max_keys(max_keys.unwrap_or(1000));

    if let Some(p) = prefix {
        request = request.prefix(p);
    }
    if let Some(d) = delimiter {
        request = request.delimiter(d);
    }
    if let Some(token) = continuation_token {
        request = request.continuation_token(token);
    }

    let response = request.send().await?;

    let objects = response
        .contents()
        .iter()
        .filter_map(|obj| {
            let key = obj.key()?.to_string();
            // Skip directory markers
            if key.ends_with('/') {
                return None;
            }
            Some(R2Object {
                key,
                size: obj.size().unwrap_or(0),
                last_modified: obj
                    .last_modified()
                    .map(|dt| dt.to_string())
                    .unwrap_or_default(),
                etag: obj.e_tag().unwrap_or_default().to_string(),
            })
        })
        .collect();

    let folders = response
        .common_prefixes()
        .iter()
        .filter_map(|prefix| prefix.prefix().map(|s| s.to_string()))
        .collect();

    Ok(ListObjectsResult {
        objects,
        folders,
        truncated: response.is_truncated().unwrap_or(false),
        continuation_token: response.next_continuation_token().map(|s| s.to_string()),
    })
}

/// List all objects recursively (for caching)
pub async fn list_all_objects_recursive(
    config: &R2Config,
    progress_callback: Option<Box<dyn Fn(usize) + Send + Sync>>,
) -> R2Result<Vec<R2Object>> {
    let client = create_r2_client(config).await?;
    let mut all_objects: Vec<R2Object> = Vec::new();
    let mut continuation_token: Option<String> = None;

    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(&config.bucket)
            .max_keys(1000);

        if let Some(token) = &continuation_token {
            request = request.continuation_token(token);
        }

        let response = request.send().await?;
        let is_truncated = response.is_truncated().unwrap_or(false);
        let next_token = response.next_continuation_token().map(|s| s.to_string());

        // Process current page objects and accumulate.
        let objects: Vec<R2Object> = response
            .contents()
            .iter()
            .filter_map(|obj| {
                let key = obj.key()?.to_string();
                // Skip directory markers
                if key.ends_with('/') {
                    return None;
                }
                Some(R2Object {
                    key,
                    size: obj.size().unwrap_or(0),
                    last_modified: obj
                        .last_modified()
                        .map(|dt| dt.to_string())
                        .unwrap_or_default(),
                    etag: obj.e_tag().unwrap_or_default().to_string(),
                })
            })
            .collect();

        all_objects.extend(objects);
        if let Some(ref cb) = progress_callback {
            cb(all_objects.len());
        }

        if !is_truncated {
            break;
        }

        continuation_token = next_token;
    }

    Ok(all_objects)
}

/// List all objects under a prefix with delimiter (for folder listing)
/// Returns both files and folders at the current level
pub async fn list_folder_objects(
    config: &R2Config,
    prefix: Option<&str>,
    progress_callback: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
) -> R2Result<ListObjectsResult> {
    let client = create_r2_client(config).await?;
    let mut all_objects = Vec::new();
    let mut all_folders = Vec::new();
    let mut continuation_token: Option<String> = None;
    let mut page_count = 0;

    loop {
        let mut request = client
            .list_objects_v2()
            .bucket(&config.bucket)
            .delimiter("/")
            .max_keys(1000);

        if let Some(p) = prefix {
            request = request.prefix(p);
        }
        if let Some(token) = &continuation_token {
            request = request.continuation_token(token);
        }

        let response = request.send().await?;
        page_count += 1;

        // Process objects
        let objects: Vec<R2Object> = response
            .contents()
            .iter()
            .filter_map(|obj| {
                let key = obj.key()?.to_string();
                // Skip directory markers
                if key.ends_with('/') {
                    return None;
                }
                Some(R2Object {
                    key,
                    size: obj.size().unwrap_or(0),
                    last_modified: obj
                        .last_modified()
                        .map(|dt| dt.to_string())
                        .unwrap_or_default(),
                    etag: obj.e_tag().unwrap_or_default().to_string(),
                })
            })
            .collect();

        all_objects.extend(objects);

        // Process folders
        let folders: Vec<String> = response
            .common_prefixes()
            .iter()
            .filter_map(|prefix| prefix.prefix().map(|s| s.to_string()))
            .collect();

        for folder in folders {
            if !all_folders.contains(&folder) {
                all_folders.push(folder);
            }
        }

        // Report progress (page_count, total items so far)
        if let Some(ref cb) = progress_callback {
            cb(page_count, all_objects.len() + all_folders.len());
        }

        if !response.is_truncated().unwrap_or(false) {
            break;
        }

        continuation_token = response.next_continuation_token().map(|s| s.to_string());
    }

    Ok(ListObjectsResult {
        objects: all_objects,
        folders: all_folders,
        truncated: false,
        continuation_token: None,
    })
}
