ALTER TABLE sessions ADD COLUMN public_id TEXT;
ALTER TABLE sessions ADD COLUMN ip TEXT;
ALTER TABLE sessions ADD COLUMN user_agent TEXT;
UPDATE sessions SET public_id = lower(hex(randomblob(16)));
CREATE UNIQUE INDEX sessions_public_id ON sessions (public_id);
