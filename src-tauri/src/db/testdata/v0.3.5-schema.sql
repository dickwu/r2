-- Schema created by init_db in v0.3.5 (tag v0.3.5, commit 6c232c4), in its
-- creation order, copied verbatim from that release's src-tauri/src/db/*.rs.
-- A frozen fixture for migration tests: never edit it to match newer code.

-- sessions
    -- Upload sessions tables
    CREATE TABLE IF NOT EXISTS upload_sessions (
        id TEXT PRIMARY KEY,
        file_path TEXT NOT NULL,
        file_size INTEGER NOT NULL,
        file_mtime INTEGER NOT NULL,
        object_key TEXT NOT NULL,
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        upload_id TEXT,
        content_type TEXT NOT NULL,
        total_parts INTEGER NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        status TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS completed_parts (
        session_id TEXT NOT NULL,
        part_number INTEGER NOT NULL,
        etag TEXT NOT NULL,
        PRIMARY KEY (session_id, part_number),
        FOREIGN KEY (session_id) REFERENCES upload_sessions(id)
    );

    CREATE INDEX IF NOT EXISTS idx_sessions_status ON upload_sessions(status);
    CREATE INDEX IF NOT EXISTS idx_sessions_file ON upload_sessions(file_path, file_size, file_mtime);


-- accounts
        -- Multi-account tables
        CREATE TABLE IF NOT EXISTS accounts (
            id TEXT PRIMARY KEY,
            name TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );


-- tokens
    CREATE TABLE IF NOT EXISTS tokens (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL REFERENCES accounts(id),
        name TEXT,
        api_token TEXT NOT NULL,
        access_key_id TEXT NOT NULL,
        secret_access_key TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_tokens_account ON tokens(account_id);


-- buckets
        CREATE TABLE IF NOT EXISTS buckets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            token_id INTEGER NOT NULL REFERENCES tokens(id),
            name TEXT NOT NULL,
            public_domain TEXT,
            public_domain_scheme TEXT,
            is_public INTEGER NOT NULL DEFAULT 0,
            public_path_prefix TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(token_id, name)
        );

        CREATE INDEX IF NOT EXISTS idx_buckets_token ON buckets(token_id);
        CREATE INDEX IF NOT EXISTS idx_buckets_unique ON buckets(token_id, name);


-- aws_accounts
    CREATE TABLE IF NOT EXISTS aws_accounts (
        id TEXT PRIMARY KEY,
        name TEXT,
        access_key_id TEXT NOT NULL,
        secret_access_key TEXT NOT NULL,
        region TEXT NOT NULL,
        endpoint_scheme TEXT NOT NULL,
        endpoint_host TEXT,
        force_path_style INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_aws_accounts_created ON aws_accounts(created_at);


-- aws_buckets
    CREATE TABLE IF NOT EXISTS aws_buckets (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL REFERENCES aws_accounts(id),
        name TEXT NOT NULL,
        public_domain_scheme TEXT,
        public_domain_host TEXT,
        is_public INTEGER NOT NULL DEFAULT 0,
        public_path_prefix TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        UNIQUE(account_id, name)
    );

    CREATE INDEX IF NOT EXISTS idx_aws_buckets_account ON aws_buckets(account_id);
    CREATE INDEX IF NOT EXISTS idx_aws_buckets_unique ON aws_buckets(account_id, name);


-- minio_accounts
    CREATE TABLE IF NOT EXISTS minio_accounts (
        id TEXT PRIMARY KEY,
        name TEXT,
        access_key_id TEXT NOT NULL,
        secret_access_key TEXT NOT NULL,
        endpoint_scheme TEXT NOT NULL,
        endpoint_host TEXT NOT NULL,
        force_path_style INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_minio_accounts_created ON minio_accounts(created_at);


-- minio_buckets
    CREATE TABLE IF NOT EXISTS minio_buckets (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL REFERENCES minio_accounts(id),
        name TEXT NOT NULL,
        public_domain_scheme TEXT,
        public_domain_host TEXT,
        is_public INTEGER NOT NULL DEFAULT 0,
        public_path_prefix TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        UNIQUE(account_id, name)
    );

    CREATE INDEX IF NOT EXISTS idx_minio_buckets_account ON minio_buckets(account_id);
    CREATE INDEX IF NOT EXISTS idx_minio_buckets_unique ON minio_buckets(account_id, name);


-- rustfs_accounts
    CREATE TABLE IF NOT EXISTS rustfs_accounts (
        id TEXT PRIMARY KEY,
        name TEXT,
        access_key_id TEXT NOT NULL,
        secret_access_key TEXT NOT NULL,
        endpoint_scheme TEXT NOT NULL,
        endpoint_host TEXT NOT NULL,
        force_path_style INTEGER NOT NULL DEFAULT 1,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_rustfs_accounts_created ON rustfs_accounts(created_at);


-- rustfs_buckets
    CREATE TABLE IF NOT EXISTS rustfs_buckets (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        account_id TEXT NOT NULL REFERENCES rustfs_accounts(id),
        name TEXT NOT NULL,
        public_domain_scheme TEXT,
        public_domain_host TEXT,
        is_public INTEGER NOT NULL DEFAULT 0,
        public_path_prefix TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        UNIQUE(account_id, name)
    );

    CREATE INDEX IF NOT EXISTS idx_rustfs_buckets_account ON rustfs_buckets(account_id);
    CREATE INDEX IF NOT EXISTS idx_rustfs_buckets_unique ON rustfs_buckets(account_id, name);


-- app_state
    CREATE TABLE IF NOT EXISTS app_state (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );


-- file_cache
    -- File cache tables (replaces IndexedDB)

    CREATE TABLE IF NOT EXISTS cached_files (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        key TEXT NOT NULL,
        parent_path TEXT NOT NULL,  -- Parent folder path (empty string for root)
        name TEXT NOT NULL,          -- File name without path
        size INTEGER NOT NULL,
        last_modified TEXT NOT NULL,
        synced_at INTEGER NOT NULL,
        PRIMARY KEY (bucket, account_id, key)
    );


    CREATE TABLE IF NOT EXISTS directory_tree (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        path TEXT NOT NULL,
        parent_path TEXT NOT NULL,  -- Parent folder path for fast child lookup
        file_count INTEGER NOT NULL,
        total_file_count INTEGER NOT NULL,
        size INTEGER NOT NULL,
        total_size INTEGER NOT NULL,
        last_modified TEXT,
        last_updated INTEGER NOT NULL,
        PRIMARY KEY (bucket, account_id, path)
    );

    CREATE TABLE IF NOT EXISTS cached_files_staging (
        bucket TEXT NOT NULL, account_id TEXT NOT NULL, key TEXT NOT NULL,
        parent_path TEXT NOT NULL, name TEXT NOT NULL, size INTEGER NOT NULL,
        last_modified TEXT NOT NULL, synced_at INTEGER NOT NULL,
        PRIMARY KEY(bucket, account_id, key)
    );

    CREATE TABLE IF NOT EXISTS sync_meta (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        last_sync INTEGER NOT NULL,
        file_count INTEGER NOT NULL,
        PRIMARY KEY (bucket, account_id)
    );

    -- Index for fast folder listing (exact match on parent_path)
    CREATE INDEX IF NOT EXISTS idx_cached_files_parent ON cached_files(bucket, account_id, parent_path);
    CREATE INDEX IF NOT EXISTS idx_directory_tree_parent ON directory_tree(bucket, account_id, parent_path);


-- downloads
    -- Download sessions table
    CREATE TABLE IF NOT EXISTS download_sessions (
        id TEXT PRIMARY KEY,
        object_key TEXT NOT NULL,
        file_name TEXT NOT NULL,
        file_size INTEGER NOT NULL,
        downloaded_bytes INTEGER NOT NULL DEFAULT 0,
        local_path TEXT NOT NULL,
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'pending',
        error TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_download_sessions_status ON download_sessions(status);
    CREATE INDEX IF NOT EXISTS idx_download_sessions_bucket ON download_sessions(bucket, account_id);


-- move_sessions
    CREATE TABLE IF NOT EXISTS move_sessions (
        id TEXT PRIMARY KEY,
        source_key TEXT NOT NULL,
        dest_key TEXT NOT NULL,
        source_bucket TEXT NOT NULL,
        source_account_id TEXT NOT NULL,
        source_provider TEXT NOT NULL,
        dest_bucket TEXT NOT NULL,
        dest_account_id TEXT NOT NULL,
        dest_provider TEXT NOT NULL,
        delete_original INTEGER NOT NULL DEFAULT 1,
        file_size INTEGER,
        progress INTEGER NOT NULL DEFAULT 0,
        status TEXT NOT NULL DEFAULT 'pending',
        error TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX IF NOT EXISTS idx_move_sessions_status ON move_sessions(status);
    CREATE INDEX IF NOT EXISTS idx_move_sessions_source ON move_sessions(source_bucket, source_account_id);

    CREATE TABLE IF NOT EXISTS move_upload_sessions (
        task_id TEXT PRIMARY KEY,
        upload_id TEXT NOT NULL,
        part_size INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS move_upload_parts (
        task_id TEXT NOT NULL,
        part_number INTEGER NOT NULL,
        etag TEXT NOT NULL,
        size INTEGER NOT NULL,
        PRIMARY KEY (task_id, part_number)
    );

    CREATE INDEX IF NOT EXISTS idx_move_upload_parts_task ON move_upload_parts(task_id);

    CREATE TABLE IF NOT EXISTS move_journal (
        task_id TEXT PRIMARY KEY,
        data TEXT NOT NULL
    );


-- prefix_sync
    CREATE TABLE IF NOT EXISTS prefix_sync_times (
        bucket TEXT NOT NULL,
        account_id TEXT NOT NULL,
        prefix TEXT NOT NULL,
        last_synced_at INTEGER NOT NULL,
        file_count INTEGER NOT NULL DEFAULT 0,
        folder_count INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (bucket, account_id, prefix)
    );
    CREATE INDEX IF NOT EXISTS idx_prefix_sync ON prefix_sync_times(bucket, account_id, prefix);


-- cache_scope
CREATE TABLE IF NOT EXISTS cache_scopes (
        account_id TEXT PRIMARY KEY, provider TEXT NOT NULL,
        fingerprint TEXT NOT NULL, revision INTEGER NOT NULL
    );
