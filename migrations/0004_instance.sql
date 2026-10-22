-- This installation's identity, and the licence key installed on it.
--
-- One row, made at first start. The instance id is what a licence key names,
-- so a key issued for one installation does not work on another. It is not a
-- secret: the admin area shows it, to be quoted when buying a key.
CREATE TABLE ih_instance (
  id           TINYINT UNSIGNED NOT NULL PRIMARY KEY,
  instance_id  VARCHAR(32) NOT NULL,
  -- The key as pasted in the admin area; NULL when none is installed. A key
  -- in HALOID_LICENCE_KEY takes precedence and is never written here.
  licence_key  TEXT NULL,
  created_at   DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6),
  updated_at   DATETIME(6) NOT NULL DEFAULT CURRENT_TIMESTAMP(6) ON UPDATE CURRENT_TIMESTAMP(6),
  CONSTRAINT ck_instance_singleton CHECK (id = 1)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_520_ci;
