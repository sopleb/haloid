-- File lifecycle, deletion audit, and list ordering.
--
-- * `state`: the upload pipeline inserts the row before the bytes reach
--   storage, so it needs the id for path templates. Until they land the row is
--   `pending`, and nothing lists or serves a pending file. Defaulting to
--   `pending` means a code path that forgets to finish an upload leaves an
--   invisible row for the orphan sweep, not a broken one in someone's gallery.
--
-- * Deletion is retained, not destroyed. A deleted file keeps its row and its
--   bytes until an administrator purges it, so that what was removed, by whom
--   and why stays auditable. `deleted_by` is NULL for deletions no account
--   made: a guest's delete token, or expiry.
--
-- * `album_position`: a user's manual ordering within an album. NULL exactly
--   when `album_id` is NULL.

ALTER TABLE ih_files
  ADD COLUMN state          ENUM('pending','ready') NOT NULL DEFAULT 'pending' AFTER kind,
  ADD COLUMN album_position INT UNSIGNED    NULL AFTER album_id,
  ADD COLUMN deleted_by     BIGINT UNSIGNED NULL AFTER deleted_at,
  ADD COLUMN delete_reason  ENUM('owner','moderation','admin','delete_token','expired','album') NULL AFTER deleted_by,
  ADD CONSTRAINT fk_files_deleted_by FOREIGN KEY (deleted_by) REFERENCES ih_users(id) ON DELETE SET NULL;

-- Keyset pagination needs an index whose trailing columns are the sort key
-- and the id tie-breaker.
ALTER TABLE ih_files
  DROP INDEX ix_files_public_created,
  DROP INDEX ix_files_album,
  ADD KEY ix_files_listed_created (state, deleted_at, is_approved, created_at, id),
  ADD KEY ix_files_album_created  (album_id, created_at, id),
  ADD KEY ix_files_album_position (album_id, album_position, id),
  ADD KEY ix_files_views          (view_count, id),
  ADD KEY ix_files_likes          (like_count, id),
  ADD KEY ix_files_pending        (state, created_at);
