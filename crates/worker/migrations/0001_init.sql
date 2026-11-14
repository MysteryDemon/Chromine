-- One row per completed browser download. Upload, notification and cleanup
-- progress are tracked separately so a failure in a later stage never
-- repeats an earlier one.
CREATE TABLE jobs (
    id                      TEXT PRIMARY KEY NOT NULL,      -- UUIDv4
    download_guid           TEXT NOT NULL UNIQUE,           -- Chrome download GUID ("manual-…" for imports)
    source                  TEXT NOT NULL,                  -- cdp | history | manual
    original_filename       TEXT NOT NULL,
    chrome_filename         TEXT NOT NULL,                  -- name Chrome saved in incoming/
    size_bytes              INTEGER NOT NULL,
    file_dev                INTEGER NOT NULL,
    file_ino                INTEGER NOT NULL,
    incoming_path           TEXT NOT NULL,
    staged_path             TEXT NOT NULL,                  -- jobs/<id>/data
    download_completed_at   INTEGER NOT NULL,               -- unix ms
    created_at              INTEGER NOT NULL,
    updated_at              INTEGER NOT NULL,

    file_state              TEXT NOT NULL,                  -- claiming | staged | missing | deleted

    upload_state            TEXT NOT NULL,                  -- waiting | pending | uploading | finalizing | retry_wait | uploaded | failed | cancelled
    upload_attempts         INTEGER NOT NULL DEFAULT 0,
    upload_next_at          INTEGER,
    upload_error            TEXT,
    remote_upload_id        TEXT,
    remote_upload_token     TEXT,                           -- cleared once the upload is finished
    remote_file_id          TEXT,
    remote_chunk_size       INTEGER,
    remote_chunk_count      INTEGER,
    remote_expires_at       INTEGER,
    remote_chunks_done      INTEGER NOT NULL DEFAULT 0,
    uploaded_bytes          INTEGER NOT NULL DEFAULT 0,
    finalize_attempted      INTEGER NOT NULL DEFAULT 0,
    cloud_link              TEXT,
    uploaded_at             INTEGER,

    notify_state            TEXT NOT NULL,                  -- waiting | pending | sending | retry_wait | sent | failed | disabled
    notify_attempts         INTEGER NOT NULL DEFAULT 0,
    notify_next_at          INTEGER,
    notify_error            TEXT,
    telegram_random_id      INTEGER,                        -- reused on every retry of this notification
    telegram_message_id     INTEGER,
    notified_at             INTEGER,

    cleanup_state           TEXT NOT NULL,                  -- waiting | pending | retry_wait | deleted | retained | failed
    cleanup_attempts        INTEGER NOT NULL DEFAULT 0,
    cleanup_next_at         INTEGER,
    cleanup_error           TEXT,
    cleaned_at              INTEGER
);

CREATE INDEX jobs_upload_queue  ON jobs (upload_state, upload_next_at, created_at);
CREATE INDEX jobs_notify_queue  ON jobs (notify_state, notify_next_at, created_at);
CREATE INDEX jobs_cleanup_queue ON jobs (cleanup_state, cleanup_next_at, created_at);
CREATE INDEX jobs_created       ON jobs (created_at DESC);

-- Dashboard login sessions. Only a SHA-256 of the cookie value is stored.
CREATE TABLE sessions (
    id_hash        TEXT PRIMARY KEY NOT NULL,
    username       TEXT NOT NULL,
    csrf_token     TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL,
    last_seen_at   INTEGER NOT NULL
);
CREATE INDEX sessions_expiry ON sessions (expires_at);
