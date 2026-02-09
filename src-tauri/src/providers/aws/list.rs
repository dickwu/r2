use super::types::{
    create_aws_client, AwsBucket, AwsConfig, AwsObject, AwsResult, ListObjectsResult,
};

pub async fn list_buckets(config: &AwsConfig) -> AwsResult<Vec<AwsBucket>> {
    let client = create_aws_client(config).await?;
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
            Some(AwsBucket {
                name,
                creation_date,
            })
        })
        .collect();

    Ok(buckets)
}

pub async fn list_objects(
    config: &AwsConfig,
    prefix: Option<&str>,
    delimiter: Option<&str>,
    continuation_token: Option<&str>,
    max_keys: Option<i32>,
) -> AwsResult<ListObjectsResult> {
    let client = create_aws_client(config).await?;

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
            if key.ends_with('/') {
                return None;
            }
            Some(AwsObject {
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

pub async fn list_all_objects_recursive(
    config: &AwsConfig,
    progress_callback: Option<Box<dyn Fn(usize) + Send + Sync>>,
) -> AwsResult<Vec<AwsObject>> {
    let client = create_aws_client(config).await?;
    let mut all_objects: Vec<AwsObject> = Vec::new();

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

        let objects: Vec<AwsObject> = response
            .contents()
            .iter()
            .filter_map(|obj| {
                let key = obj.key()?.to_string();
                if key.ends_with('/') {
                    return None;
                }
                Some(AwsObject {
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

pub async fn list_folder_objects(
    config: &AwsConfig,
    prefix: Option<&str>,
    progress_callback: Option<Box<dyn Fn(usize, usize) + Send + Sync>>,
) -> AwsResult<ListObjectsResult> {
    let client = create_aws_client(config).await?;
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

        let objects: Vec<AwsObject> = response
            .contents()
            .iter()
            .filter_map(|obj| {
                let key = obj.key()?.to_string();
                if key.ends_with('/') {
                    return None;
                }
                Some(AwsObject {
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
