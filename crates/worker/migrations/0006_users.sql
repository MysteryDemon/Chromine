CREATE TABLE users (
    id            TEXT PRIMARY KEY NOT NULL,
    username      TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL
);
ALTER TABLE sessions ADD COLUMN owner TEXT NOT NULL DEFAULT '';
CREATE INDEX sessions_owner ON sessions (owner);
ALTER TABLE jobs ADD COLUMN owner TEXT NOT NULL DEFAULT '';
CREATE INDEX jobs_owner ON jobs (owner, created_at DESC);
