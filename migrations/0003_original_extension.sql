-- The retained original's extension.
--
-- When a resize alters the served file the untouched original is kept as
-- `{name}.orig.{ext}`, and that extension can differ from the served one: a
-- resized HEIC is served as JPEG. Without this column the original's key
-- could not be rebuilt from the row, and so could never be quarantined or
-- purged. NULL exactly when no original is retained.
ALTER TABLE ih_files
  ADD COLUMN original_extension VARCHAR(16) NULL AFTER extension;
