-- Schema v1. All culling decisions live here; originals are never modified.

CREATE TABLE IF NOT EXISTS sources (
  id              INTEGER PRIMARY KEY,
  root_path       TEXT NOT NULL UNIQUE,
  label           TEXT,
  output_path     TEXT,
  cache_path      TEXT,
  created_at      INTEGER NOT NULL,
  last_indexed_at INTEGER
);

-- One logical photo: a RAW+JPEG/HEIF pair or a lone file.
CREATE TABLE IF NOT EXISTS photos (
  id              INTEGER PRIMARY KEY,
  source_id       INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  dir             TEXT NOT NULL,
  stem            TEXT NOT NULL,
  capture_time_ms INTEGER NOT NULL,
  low_precision   INTEGER NOT NULL DEFAULT 0,
  width           INTEGER,
  height          INTEGER,
  orientation     INTEGER,
  burst_id        INTEGER REFERENCES bursts(id) ON DELETE SET NULL,
  frame_index     INTEGER,
  burst_locked    INTEGER NOT NULL DEFAULT 0,
  keep            INTEGER NOT NULL DEFAULT 0,
  sharpness       REAL,
  UNIQUE(source_id, dir, stem)
);
CREATE INDEX IF NOT EXISTS idx_photos_time  ON photos(source_id, capture_time_ms);
CREATE INDEX IF NOT EXISTS idx_photos_burst ON photos(burst_id, frame_index);

CREATE TABLE IF NOT EXISTS files (
  id           INTEGER PRIMARY KEY,
  photo_id     INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
  rel_path     TEXT NOT NULL,
  kind         TEXT NOT NULL CHECK(kind IN ('raw','jpeg','heif')),
  size         INTEGER NOT NULL,
  mtime        INTEGER NOT NULL,
  content_hash TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_files_photo ON files(photo_id);
CREATE INDEX IF NOT EXISTS idx_files_hash  ON files(content_hash);

CREATE TABLE IF NOT EXISTS bursts (
  id                INTEGER PRIMARY KEY,
  source_id         INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
  start_ms          INTEGER NOT NULL,
  end_ms            INTEGER NOT NULL,
  frame_count       INTEGER NOT NULL,
  fps_estimate      REAL,
  status            TEXT NOT NULL DEFAULT 'undecided'
                    CHECK(status IN ('undecided','done','rejected')),
  keep_video        INTEGER NOT NULL DEFAULT 0,
  video_cache_path  TEXT,
  preprocessed_at   INTEGER,
  exported_at       INTEGER,
  video_exported_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_bursts_source ON bursts(source_id, start_ms);

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS export_log (
  id           INTEGER PRIMARY KEY,
  job_id       TEXT NOT NULL,
  photo_id     INTEGER,
  file_id      INTEGER,
  dest_path    TEXT NOT NULL,
  action       TEXT NOT NULL CHECK(action IN ('copied','skipped_identical','renamed')),
  completed_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_export_log_job ON export_log(job_id);
