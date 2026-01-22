use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CacheUpdatedEvent {
    pub action: String,
    pub affected_paths: Vec<String>,
}

/// Event emitted when empty paths are removed after file deletion
#[derive(Debug, Clone, Serialize)]
pub(crate) struct PathsRemovedEvent {
    pub removed_paths: Vec<String>,
}

/// Get all unique parent paths (including ancestors) for a list of keys.
/// This ensures that when a file is uploaded to "a/b/file.txt", the affected paths
/// include "", "a/", and "a/b/" so that all ancestor folder views are refreshed.
pub(crate) fn get_unique_parent_paths(keys: &[String]) -> Vec<String> {
    let mut paths: HashSet<String> = HashSet::new();

    for key in keys {
        // Always include root
        paths.insert(String::new());

        // Add all ancestor paths
        let mut current_path = String::new();
        for segment in key.split('/') {
            if segment.is_empty() {
                continue;
            }
            // Check if this is the last segment (the filename)
            let remaining = &key[current_path.len()..];
            if !remaining.contains('/') {
                // This is the filename, stop here
                break;
            }
            current_path.push_str(segment);
            current_path.push('/');
            paths.insert(current_path.clone());
        }
    }

    paths.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::get_unique_parent_paths;

    #[test]
    fn unique_parent_paths_includes_root_and_nested_folders() {
        let keys = vec![
            "file.txt".to_string(),
            "dir/other.txt".to_string(),
            "dir/sub/file.txt".to_string(),
            "dir/sub/file.txt".to_string(),
        ];

        let mut paths = get_unique_parent_paths(&keys);
        paths.sort();

        assert_eq!(
            paths,
            vec!["".to_string(), "dir/".to_string(), "dir/sub/".to_string()]
        );
    }
}
