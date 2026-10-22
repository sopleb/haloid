-- A small Chevereto v4 database for the importer's tests: the tables and
-- columns the importer reads, written for these tests (not copied from
-- Chevereto's schema files), with a handful of rows.

CREATE TABLE chv_variables (
  variable_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  variable_name VARCHAR(255) NOT NULL,
  variable_value TEXT
);
INSERT INTO chv_variables (variable_name, variable_value) VALUES ('crypt_salt', 'salty'), ('id_padding', '0');

CREATE TABLE chv_storage_apis (
  storage_api_id INT UNSIGNED NOT NULL PRIMARY KEY,
  storage_api_name VARCHAR(255) NOT NULL,
  storage_api_type VARCHAR(255) NOT NULL
);
INSERT INTO chv_storage_apis VALUES (1, 'Amazon S3', 's3'), (9, 'FTP', 'ftp');

CREATE TABLE chv_storages (
  storage_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  storage_api_id INT UNSIGNED NOT NULL,
  storage_name VARCHAR(255) NOT NULL,
  storage_url VARCHAR(255) NOT NULL,
  storage_bucket VARCHAR(255) NULL,
  storage_region VARCHAR(255) NULL,
  storage_server VARCHAR(255) NULL,
  storage_use_path_style_endpoint TINYINT UNSIGNED NOT NULL DEFAULT 0
);
INSERT INTO chv_storages VALUES
  (1, 1, 'bucket', 'https://cdn.example.com', 'media', 'eu-west-1', NULL, 0),
  (2, 9, 'old ftp', 'https://ftp.example.com', NULL, NULL, 'ftp.example.com', 0);

CREATE TABLE chv_categories (
  category_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  category_name VARCHAR(32) NOT NULL,
  category_url_key VARCHAR(32) NOT NULL,
  category_description TEXT
);
INSERT INTO chv_categories VALUES (1, 'Nature', 'nature', 'Outdoors');

CREATE TABLE chv_users (
  user_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  user_name VARCHAR(255) NULL,
  user_username VARCHAR(255) NOT NULL,
  user_date_gmt DATETIME NOT NULL,
  user_email VARCHAR(255) NULL,
  user_timezone VARCHAR(255) NOT NULL,
  user_status ENUM('valid','awaiting-confirmation','awaiting-email','banned') NOT NULL,
  user_is_admin TINYINT UNSIGNED NOT NULL DEFAULT 0,
  user_is_manager TINYINT UNSIGNED NOT NULL DEFAULT 0,
  user_is_private TINYINT UNSIGNED NOT NULL DEFAULT 0
);
INSERT INTO chv_users VALUES
  (1, 'Rodrigo', 'admin', '2020-01-02 03:04:05', 'admin@example.com', 'UTC', 'valid', 1, 0, 0),
  (2, NULL, 'mira', '2021-05-06 07:08:09', 'Mira@Example.com', 'Europe/Paris', 'awaiting-email', 0, 0, 1),
  (3, NULL, 'ftpuser', '2021-05-06 07:08:09', NULL, 'UTC', 'banned', 0, 0, 0);

CREATE TABLE chv_login_passwords (
  login_password_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  login_password_user_id INT UNSIGNED NOT NULL,
  login_password_date_gmt DATETIME NOT NULL,
  login_password_hash TEXT NOT NULL
);
-- bcrypt of "correct horse battery", cost 4, as PHP's password_hash writes it;
-- and an older one for the same user that must lose to the newer.
INSERT INTO chv_login_passwords VALUES
  (1, 2, '2021-05-06 07:08:09', '$2y$04$oldoldoldoldoldoldoldOoldoldoldoldoldoldoldoldoldoldol'),
  (2, 2, '2022-01-01 00:00:00', '$2y$04$IpH.kt2ZZ/Qdh3ncMlrk8O3nRU9s5cLCRd9W5OGlxJiZtHqWL/j9i');

CREATE TABLE chv_albums (
  album_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  album_name VARCHAR(100) NOT NULL,
  album_user_id INT UNSIGNED NULL,
  album_date_gmt DATETIME NOT NULL,
  album_privacy ENUM('public','password','private','private_but_link','custom') DEFAULT 'public',
  album_password TEXT,
  album_description TEXT,
  album_views INT UNSIGNED NOT NULL DEFAULT 0,
  album_cover_id INT UNSIGNED NULL,
  album_parent_id INT UNSIGNED NULL
);
-- The child comes before its parent, as ids allow.
INSERT INTO chv_albums VALUES
  (1, 'Trips', 2, '2021-06-01 10:00:00', 'private_but_link', NULL, 'Child of Everything', 12, 2, 2),
  (2, 'Everything', 2, '2021-06-01 09:00:00', 'public', NULL, NULL, 3, NULL, NULL),
  (3, 'Secret', 2, '2021-06-01 09:00:00', 'password', 'open sesame', NULL, 0, NULL, NULL);

CREATE TABLE chv_images (
  image_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  image_name VARCHAR(255) NOT NULL,
  image_extension VARCHAR(255) NOT NULL,
  image_size BIGINT UNSIGNED NOT NULL,
  image_width INT UNSIGNED NOT NULL,
  image_height INT UNSIGNED NOT NULL,
  image_date DATETIME NOT NULL,
  image_date_gmt DATETIME NOT NULL,
  image_title VARCHAR(100) NULL,
  image_description TEXT,
  image_nsfw TINYINT UNSIGNED NOT NULL DEFAULT 0,
  image_user_id INT UNSIGNED NULL,
  image_album_id INT UNSIGNED NULL,
  image_uploader_ip VARCHAR(255) NOT NULL,
  image_storage_mode ENUM('datefolder','direct','old','path') NOT NULL DEFAULT 'datefolder',
  image_path VARCHAR(4096) NULL,
  image_storage_id INT UNSIGNED NULL,
  image_checksum VARCHAR(32) NOT NULL,
  image_source_checksum VARCHAR(32) NULL,
  image_original_filename VARCHAR(255) NOT NULL,
  image_original_exifdata MEDIUMTEXT,
  image_views INT UNSIGNED NOT NULL DEFAULT 0,
  image_category_id INT UNSIGNED NULL,
  image_chain TINYINT UNSIGNED NOT NULL,
  image_thumb_size INT UNSIGNED NOT NULL,
  image_medium_size INT UNSIGNED NOT NULL DEFAULT 0,
  image_frame_size INT UNSIGNED NOT NULL DEFAULT 0,
  image_expiration_date_gmt DATETIME NULL,
  image_likes INT UNSIGNED NOT NULL DEFAULT 0,
  image_is_animated TINYINT UNSIGNED NOT NULL DEFAULT 0,
  image_is_approved TINYINT UNSIGNED NOT NULL DEFAULT 1,
  image_is_360 TINYINT UNSIGNED NOT NULL DEFAULT 0,
  image_duration INT UNSIGNED NOT NULL DEFAULT 0
);
INSERT INTO chv_images VALUES
  -- Local date is a day ahead of GMT: the folder follows the local date.
  (1, 'sunset-Ab12', 'jpg', 1000, 800, 600, '2021-06-02 01:30:00', '2021-06-01 23:30:00', 'Sunset', 'Over the bay', 0, 2, 1, '203.0.113.7', 'datefolder', NULL, NULL, 'd41d8cd98f00b204e9800998ecf8427e', NULL, 'IMG_0001.JPG', '{"Make":"Canon"}', 42, 1, 7, 200, 500, 0, NULL, 1, 0, 1, 0, 0),
  (2, 'harbour-Cd34', 'png', 2000, 1200, 900, '2021-06-03 12:00:00', '2021-06-03 10:00:00', NULL, NULL, 1, 2, 1, '2001:db8::1', 'datefolder', NULL, NULL, '0cc175b9c0f1b6a831c399e269772661', NULL, 'harbour.png', 'not json', 3, NULL, 7, 300, 600, 0, NULL, 0, 0, 0, 0, 0),
  (3, 'clip-Ef56', 'mp4', 50000, 640, 360, '2021-07-01 08:00:00', '2021-07-01 08:00:00', 'Clip', NULL, 0, 1, NULL, '198.51.100.2', 'datefolder', NULL, 1, '92eb5ffee6ae2fec3ad71c777531578f', NULL, 'clip.mp4', NULL, 0, NULL, 21, 100, 0, 400, '2030-01-01 00:00:00', 0, 0, 1, 0, 12),
  (4, 'doc-Gh78', 'pdf', 10, 0, 0, '2021-07-01 08:00:00', '2021-07-01 08:00:00', NULL, NULL, 0, 1, NULL, '198.51.100.2', 'datefolder', NULL, NULL, '4a8a08f09d37b73795649038408b5f33', NULL, 'doc.pdf', NULL, 0, NULL, 4, 0, 0, 0, NULL, 0, 0, 1, 0, 0),
  (5, 'ftp-Ij90', 'gif', 10, 1, 1, '2021-07-01 08:00:00', '2021-07-01 08:00:00', NULL, NULL, 0, 3, NULL, '198.51.100.2', 'datefolder', NULL, 2, '8277e0910d750195b448797616e091ad', NULL, 'x.gif', NULL, 0, NULL, 4, 0, 0, 0, NULL, 0, 0, 1, 0, 0),
  (6, 'guest-Kl12', 'webp', 10, 1, 1, '2021-07-01 08:00:00', '2021-07-01 08:00:00', NULL, NULL, 0, NULL, NULL, 'not an ip', 'direct', NULL, NULL, 'e1671797c52e15f763380b45e841ec32', NULL, 'g.webp', NULL, 0, NULL, 4, 0, 0, 0, NULL, 0, 0, 1, 0, 0);

CREATE TABLE chv_tags (
  tag_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  tag_name VARCHAR(32) NOT NULL
);
INSERT INTO chv_tags VALUES (1, 'sunset'), (2, 'bay'), (3, 'boats');

CREATE TABLE chv_tags_files (
  tag_file_tag_id INT UNSIGNED NOT NULL,
  tag_file_file_id INT UNSIGNED NOT NULL
);
INSERT INTO chv_tags_files VALUES (1, 1), (2, 1), (3, 2);

CREATE TABLE chv_likes (
  like_id INT UNSIGNED NOT NULL AUTO_INCREMENT PRIMARY KEY,
  like_date_gmt DATETIME NOT NULL,
  like_user_id INT UNSIGNED NULL,
  like_content_type ENUM('image','album') NULL,
  like_content_id INT UNSIGNED NOT NULL
);
INSERT INTO chv_likes VALUES (1, '2021-06-05 00:00:00', 1, 'image', 1), (2, '2021-06-05 00:00:00', NULL, 'image', 1), (3, '2021-06-05 00:00:00', 1, 'album', 2);
