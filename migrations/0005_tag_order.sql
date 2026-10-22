-- A file's tags keep the order they were written in.
--
-- Until now they were read back sorted by slug, so "sunset, beach, 2024" came
-- back as "2024, beach, sunset". Existing rows get position 0 and so keep the
-- old order (slug breaks the tie) until the file's tags are next saved.
ALTER TABLE ih_file_tags
  ADD COLUMN position SMALLINT UNSIGNED NOT NULL DEFAULT 0 AFTER tag_id;
