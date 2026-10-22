-- Haloid initial schema.
--
-- Our own design: we never share a database with another host, and importing
-- from one is out of scope beyond the `legacy_id_encoded` columns.
--
-- Conventions:
--  * `ih_` prefix throughout, so co-tenancy in a shared database cannot collide.
--  * Unique-indexed string columns are VARCHAR(190), never 255: utf8mb4 x 255
--    exceeds InnoDB's 767-byte index prefix on older row formats. The server
--    preflights `innodb_default_row_format=dynamic` at startup.
--  * Public identifiers (`id_encoded`) are random base62, not an encoding of
--    the row id. Decoding is a lookup on the unique index.
--  * Soft delete via `deleted_at`; a job does the hard delete so that storage
--    failures stay retryable.

-- Settings are stored as opaque strings. There is deliberately no value_type
-- column: the type of each setting is compile-time knowledge in
-- haloid-core::settings, and a column restating it would be a second source of
-- truth free to drift from the parser. The admin API reports types and
-- defaults from the code instead.
CREATE TABLE ih_settings (
  name       VARCHAR(190) NOT NULL PRIMARY KEY,
  value      TEXT         NULL,
  updated_at DATETIME     NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_users (
  id                BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  id_encoded        VARCHAR(32)     NOT NULL,
  username          VARCHAR(64)     NOT NULL,
  -- Written by the application, not generated: the lookup key for both
  -- uniqueness and login.
  username_lower    VARCHAR(64)     NOT NULL,
  email             VARCHAR(190)    NULL,
  email_lower       VARCHAR(190)    NULL,
  -- argon2id PHC string. NULL means this account has no password login.
  password_hash     VARCHAR(255)    NULL,
  role              ENUM('user','manager','admin') NOT NULL DEFAULT 'user',
  status            ENUM('valid','awaiting_confirmation','banned') NOT NULL DEFAULT 'valid',
  display_name      VARCHAR(190)    NULL,
  avatar_file_id    BIGINT UNSIGNED NULL,
  timezone          VARCHAR(64)     NULL,
  is_private        TINYINT(1)      NOT NULL DEFAULT 0,
  -- Denormalised counters, maintained with `SET x = x + ?`.
  file_count        INT UNSIGNED    NOT NULL DEFAULT 0,
  album_count       INT UNSIGNED    NOT NULL DEFAULT 0,
  disk_used         BIGINT UNSIGNED NOT NULL DEFAULT 0,
  failed_logins     INT UNSIGNED    NOT NULL DEFAULT 0,
  locked_until      DATETIME        NULL,
  legacy_id_encoded VARCHAR(32)     NULL,
  created_at        DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at        DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  last_login_at     DATETIME        NULL,
  UNIQUE KEY uq_users_id_encoded (id_encoded),
  UNIQUE KEY uq_users_username (username_lower),
  UNIQUE KEY uq_users_email (email_lower),
  KEY ix_users_role_status (role, status)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_sessions (
  id           CHAR(36)        NOT NULL PRIMARY KEY,
  -- sha256 of the opaque cookie token. The cookie value is never stored.
  token_hash   CHAR(64)        NOT NULL,
  user_id      BIGINT UNSIGNED NOT NULL,
  ip           VARBINARY(16)   NULL,
  user_agent   VARCHAR(255)    NULL,
  created_at   DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  last_seen_at DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  expires_at   DATETIME        NOT NULL,
  UNIQUE KEY uq_sessions_token (token_hash),
  KEY ix_sessions_user (user_id),
  KEY ix_sessions_expires (expires_at),
  CONSTRAINT fk_sessions_user FOREIGN KEY (user_id) REFERENCES ih_users(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_api_keys (
  id           BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  -- NULL is the instance-wide guest key.
  user_id      BIGINT UNSIGNED NULL,
  label        VARCHAR(190)    NOT NULL DEFAULT '',
  -- The public lookup handle at the front of the key, and all we ever display.
  key_prefix   VARCHAR(48)     NOT NULL,
  secret_hash  CHAR(64)        NOT NULL,
  hmac_key     VARBINARY(32)   NOT NULL,
  is_active    TINYINT(1)      NOT NULL DEFAULT 1,
  last_used_at DATETIME        NULL,
  created_at   DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  revoked_at   DATETIME        NULL,
  UNIQUE KEY uq_api_keys_prefix (key_prefix),
  KEY ix_api_keys_user (user_id),
  CONSTRAINT fk_api_keys_user FOREIGN KEY (user_id) REFERENCES ih_users(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_storages (
  id             BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  name           VARCHAR(190)    NOT NULL,
  kind           ENUM('local','s3') NOT NULL,
  -- Bucket, endpoint, region, path-style flag, prefix, base path. Credentials
  -- are NOT here: they come from the environment, keyed by `name`, so that a
  -- database dump in a support ticket is not a credential leak.
  config         JSON            NOT NULL,
  url_prefix     VARCHAR(255)    NOT NULL,
  path_mode      ENUM('datefolder','direct','path') NOT NULL DEFAULT 'datefolder',
  path_template  VARCHAR(255)    NULL,
  capacity_bytes BIGINT UNSIGNED NULL,
  used_bytes     BIGINT UNSIGNED NOT NULL DEFAULT 0,
  file_count     BIGINT UNSIGNED NOT NULL DEFAULT 0,
  is_active      TINYINT(1)      NOT NULL DEFAULT 1,
  priority       INT             NOT NULL DEFAULT 0,
  created_at     DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at     DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  deleted_at     DATETIME        NULL,
  UNIQUE KEY uq_storages_name (name),
  KEY ix_storages_active (is_active, priority)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_categories (
  id          INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  name        VARCHAR(190) NOT NULL,
  url_key     VARCHAR(190) NOT NULL,
  description TEXT         NULL,
  file_count  INT UNSIGNED NOT NULL DEFAULT 0,
  sort_order  INT          NOT NULL DEFAULT 0,
  UNIQUE KEY uq_categories_url_key (url_key)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_albums (
  id                BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  id_encoded        VARCHAR(32)     NOT NULL,
  user_id           BIGINT UNSIGNED NULL,
  parent_id         BIGINT UNSIGNED NULL,
  name              VARCHAR(190)    NOT NULL,
  description       TEXT            NULL,
  -- Four states: `unlisted` is shareable by link but not listed.
  privacy           ENUM('public','unlisted','private','password') NOT NULL DEFAULT 'public',
  password_hash     VARCHAR(255)    NULL,
  cover_file_id     BIGINT UNSIGNED NULL,
  file_count        INT UNSIGNED    NOT NULL DEFAULT 0,
  view_count        BIGINT UNSIGNED NOT NULL DEFAULT 0,
  is_default        TINYINT(1)      NOT NULL DEFAULT 0,
  legacy_id_encoded VARCHAR(32)     NULL,
  created_at        DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at        DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  UNIQUE KEY uq_albums_id_encoded (id_encoded),
  KEY ix_albums_user (user_id, created_at),
  KEY ix_albums_parent (parent_id),
  CONSTRAINT fk_albums_user   FOREIGN KEY (user_id)   REFERENCES ih_users(id)  ON DELETE CASCADE,
  CONSTRAINT fk_albums_parent FOREIGN KEY (parent_id) REFERENCES ih_albums(id) ON DELETE SET NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_files (
  id                BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  id_encoded        VARCHAR(32)     NOT NULL,
  kind              ENUM('image','video','audio') NOT NULL,
  -- Stored basename without extension, including its random suffix.
  name              VARCHAR(190)    NOT NULL,
  extension         VARCHAR(16)     NOT NULL,
  mime              VARCHAR(127)    NOT NULL,
  original_filename VARCHAR(255)    NULL,
  title             VARCHAR(255)    NULL,
  description       TEXT            NULL,
  size              BIGINT UNSIGNED NOT NULL,
  -- Post-rotation dimensions: what a browser will actually render. Zero for audio.
  width             INT UNSIGNED    NOT NULL DEFAULT 0,
  height            INT UNSIGNED    NOT NULL DEFAULT 0,
  duration_ms       INT UNSIGNED    NULL,
  is_animated       TINYINT(1)      NOT NULL DEFAULT 0,
  frame_count       INT UNSIGNED    NULL,
  -- frame 16 | original 8 | image 4 | medium 2 | thumb 1. The legacy values,
  -- deliberately not tidied into sequential order: it is a wire contract.
  chain             TINYINT UNSIGNED NOT NULL,
  thumb_size        BIGINT UNSIGNED NULL,
  medium_size       BIGINT UNSIGNED NULL,
  frame_size        BIGINT UNSIGNED NULL,
  -- MD5 of the stored bytes and of the original source. They differ only when
  -- the upload was altered (a width resize, a watermark); the legacy API
  -- exposes both and so do we.
  checksum          CHAR(32)        NOT NULL,
  source_checksum   CHAR(32)        NULL,
  -- Ours, for integrity and real deduplication.
  sha256            CHAR(64)        NULL,
  storage_id        BIGINT UNSIGNED NULL,
  storage_mode      ENUM('datefolder','direct','path') NOT NULL DEFAULT 'datefolder',
  -- Key within the storage, no leading slash.
  storage_path      VARCHAR(512)    NOT NULL,
  -- NULL means a guest upload. Nobody inherits it.
  user_id           BIGINT UNSIGNED NULL,
  album_id          BIGINT UNSIGNED NULL,
  category_id       INT UNSIGNED    NULL,
  nsfw              TINYINT(1)      NOT NULL DEFAULT 0,
  is_approved       TINYINT(1)      NOT NULL DEFAULT 1,
  is_360            TINYINT(1)      NOT NULL DEFAULT 0,
  view_count        BIGINT UNSIGNED NOT NULL DEFAULT 0,
  like_count        INT UNSIGNED    NOT NULL DEFAULT 0,
  -- Full EXIF as JSON with MakerNote and oversized tags dropped. The stored
  -- file itself may have been stripped; this is what the viewer displays.
  exif              JSON            NULL,
  uploader_ip       VARBINARY(16)   NULL,
  -- Powers the one-shot delete_url, which is the only way a guest can remove
  -- their own upload.
  delete_token      CHAR(32)        NULL,
  expires_at        DATETIME        NULL,
  legacy_id_encoded VARCHAR(32)     NULL,
  created_at        DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at        DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  deleted_at        DATETIME        NULL,
  UNIQUE KEY uq_files_id_encoded (id_encoded),
  KEY ix_files_user_created    (user_id, created_at),
  KEY ix_files_album           (album_id, created_at),
  KEY ix_files_category        (category_id, created_at),
  KEY ix_files_checksum        (checksum),
  KEY ix_files_source_checksum (source_checksum),
  KEY ix_files_expires         (expires_at),
  KEY ix_files_moderation      (is_approved, created_at),
  KEY ix_files_storage         (storage_id),
  KEY ix_files_deleted         (deleted_at),
  -- Cursor pagination over the public listing.
  KEY ix_files_public_created  (is_approved, deleted_at, created_at, id),
  CONSTRAINT fk_files_user     FOREIGN KEY (user_id)     REFERENCES ih_users(id)      ON DELETE SET NULL,
  CONSTRAINT fk_files_album    FOREIGN KEY (album_id)    REFERENCES ih_albums(id)     ON DELETE SET NULL,
  CONSTRAINT fk_files_category FOREIGN KEY (category_id) REFERENCES ih_categories(id) ON DELETE SET NULL,
  CONSTRAINT fk_files_storage  FOREIGN KEY (storage_id)  REFERENCES ih_storages(id)   ON DELETE RESTRICT
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

ALTER TABLE ih_users
  ADD CONSTRAINT fk_users_avatar FOREIGN KEY (avatar_file_id) REFERENCES ih_files(id) ON DELETE SET NULL;
ALTER TABLE ih_albums
  ADD CONSTRAINT fk_albums_cover FOREIGN KEY (cover_file_id) REFERENCES ih_files(id) ON DELETE SET NULL;

CREATE TABLE ih_tags (
  id         BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  name       VARCHAR(100)    NOT NULL,
  slug       VARCHAR(100)    NOT NULL,
  file_count INT UNSIGNED    NOT NULL DEFAULT 0,
  UNIQUE KEY uq_tags_slug (slug)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_file_tags (
  file_id BIGINT UNSIGNED NOT NULL,
  tag_id  BIGINT UNSIGNED NOT NULL,
  PRIMARY KEY (file_id, tag_id),
  KEY ix_file_tags_tag (tag_id),
  CONSTRAINT fk_ft_file FOREIGN KEY (file_id) REFERENCES ih_files(id) ON DELETE CASCADE,
  CONSTRAINT fk_ft_tag  FOREIGN KEY (tag_id)  REFERENCES ih_tags(id)  ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_likes (
  user_id    BIGINT UNSIGNED NOT NULL,
  file_id    BIGINT UNSIGNED NOT NULL,
  created_at DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (user_id, file_id),
  KEY ix_likes_file (file_id),
  CONSTRAINT fk_likes_user FOREIGN KEY (user_id) REFERENCES ih_users(id) ON DELETE CASCADE,
  CONSTRAINT fk_likes_file FOREIGN KEY (file_id) REFERENCES ih_files(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_moderation_queue (
  file_id    BIGINT UNSIGNED NOT NULL PRIMARY KEY,
  reason     VARCHAR(190)    NULL,
  queued_at  DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  decided_at DATETIME        NULL,
  decided_by BIGINT UNSIGNED NULL,
  decision   ENUM('pending','approved','denied') NOT NULL DEFAULT 'pending',
  note       TEXT            NULL,
  KEY ix_modq_decision (decision, queued_at),
  CONSTRAINT fk_modq_file    FOREIGN KEY (file_id)    REFERENCES ih_files(id) ON DELETE CASCADE,
  CONSTRAINT fk_modq_decider FOREIGN KEY (decided_by) REFERENCES ih_users(id) ON DELETE SET NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_upload_sessions (
  id            CHAR(36)        NOT NULL PRIMARY KEY,
  user_id       BIGINT UNSIGNED NULL,
  filename      VARCHAR(255)    NOT NULL,
  declared_size BIGINT UNSIGNED NOT NULL,
  received_size BIGINT UNSIGNED NOT NULL DEFAULT 0,
  chunk_size    INT UNSIGNED    NOT NULL,
  chunks_total  INT UNSIGNED    NOT NULL,
  -- One bit per chunk; 4096 bytes caps a single upload at 32768 chunks.
  chunks_bitmap VARBINARY(4096) NOT NULL,
  temp_path     VARCHAR(512)    NOT NULL,
  -- The metadata the eventual UploadCommand will carry.
  params        JSON            NULL,
  status        ENUM('open','assembling','done','failed','expired') NOT NULL DEFAULT 'open',
  file_id       BIGINT UNSIGNED NULL,
  created_at    DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at    DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP ON UPDATE CURRENT_TIMESTAMP,
  expires_at    DATETIME        NOT NULL,
  KEY ix_upload_sessions_user (user_id),
  KEY ix_upload_sessions_expires (expires_at, status),
  CONSTRAINT fk_upload_sessions_user FOREIGN KEY (user_id) REFERENCES ih_users(id) ON DELETE CASCADE
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_jobs (
  id           BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  kind         VARCHAR(64)     NOT NULL,
  payload      JSON            NOT NULL,
  -- Lets the scheduler enqueue blindly: a duplicate is a no-op.
  dedupe_key   VARCHAR(190)    NULL,
  status       ENUM('pending','running','done','failed') NOT NULL DEFAULT 'pending',
  attempts     INT UNSIGNED    NOT NULL DEFAULT 0,
  max_attempts INT UNSIGNED    NOT NULL DEFAULT 5,
  last_error   TEXT            NULL,
  run_at       DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  locked_at    DATETIME        NULL,
  locked_by    VARCHAR(64)     NULL,
  created_at   DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  finished_at  DATETIME        NULL,
  UNIQUE KEY uq_jobs_dedupe (dedupe_key),
  KEY ix_jobs_claim (status, run_at)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_stats (
  scope    ENUM('global','user','album','storage') NOT NULL,
  scope_id BIGINT UNSIGNED NOT NULL DEFAULT 0,
  day      DATE            NOT NULL,
  uploads  INT UNSIGNED    NOT NULL DEFAULT 0,
  views    BIGINT UNSIGNED NOT NULL DEFAULT 0,
  bytes    BIGINT UNSIGNED NOT NULL DEFAULT 0,
  PRIMARY KEY (scope, scope_id, day)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_audit_log (
  id         BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  actor_id   BIGINT UNSIGNED NULL,
  action     VARCHAR(64)     NOT NULL,
  target     VARCHAR(64)     NULL,
  target_id  BIGINT UNSIGNED NULL,
  detail     JSON            NULL,
  ip         VARBINARY(16)   NULL,
  request_id VARCHAR(64)     NULL,
  created_at DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  KEY ix_audit_actor (actor_id, created_at),
  KEY ix_audit_action (action, created_at),
  CONSTRAINT fk_audit_actor FOREIGN KEY (actor_id) REFERENCES ih_users(id) ON DELETE SET NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;

CREATE TABLE ih_ip_bans (
  id         BIGINT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  -- Stored as text so a CIDR range and a single address share one column.
  cidr       VARCHAR(64)     NOT NULL,
  reason     VARCHAR(190)    NULL,
  created_by BIGINT UNSIGNED NULL,
  created_at DATETIME        NOT NULL DEFAULT CURRENT_TIMESTAMP,
  expires_at DATETIME        NULL,
  UNIQUE KEY uq_ip_bans_cidr (cidr),
  KEY ix_ip_bans_expires (expires_at),
  CONSTRAINT fk_ip_bans_creator FOREIGN KEY (created_by) REFERENCES ih_users(id) ON DELETE SET NULL
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;
