# Haloid

A self-hosted image, video and audio host. One Rust binary, a MariaDB
database, and a web app compiled into the binary.

Existing Chevereto v4, ShareX and ImgBB clients work against it unchanged:
`POST /api/1/upload` is reproduced exactly, including the undocumented
behaviour real clients depend on. Everything else is served by a clean REST
API at `/api/v2`.

## Deploying

You need **MariaDB 10.6 or later** and a **reverse proxy for TLS** (Caddy,
nginx, Traefik); Haloid speaks plain HTTP. Releases are on the
[releases page](https://github.com/sopleb/haloid/releases), as a container
image and as a static Linux binary.

### With Docker

```yaml
# compose.yaml
services:
  db:
    image: mariadb:11
    environment:
      MARIADB_DATABASE: haloid
      MARIADB_USER: haloid
      MARIADB_PASSWORD: change-me
      MARIADB_RANDOM_ROOT_PASSWORD: "1"
    volumes: [db:/var/lib/mysql]
    restart: unless-stopped

  haloid:
    image: ghcr.io/sopleb/haloid:latest
    depends_on: [db]
    labels: [so.plb.haloid.updates=server]
    environment:
      DATABASE_URL: mysql://haloid:change-me@db:3306/haloid
      HALOID_SITE_BASE_URL: https://img.example.com
      HALOID_ALTCHA_SECRET: change-me-too       # openssl rand -hex 32
      HALOID_TRUSTED_PROXIES: 172.16.0.0/12      # where your proxy connects from
      HALOID_UPDATER_URL: http://updater:8090
      HALOID_UPDATER_TOKEN: and-this-one         # openssl rand -hex 32
    volumes: [data:/data]
    ports: ["127.0.0.1:8080:8080"]
    restart: unless-stopped

  # Optional: installs new releases when an administrator asks. It holds the
  # Docker socket (as good as root on the host); Haloid never does.
  updater:
    image: ghcr.io/sopleb/haloid-updater:latest
    environment:
      HALOID_UPDATER_TOKEN: and-this-one         # the same token
    volumes: ["/var/run/docker.sock:/var/run/docker.sock"]
    restart: unless-stopped

volumes: { db: {}, data: {} }
```

`docker compose up -d`, then point your proxy at `127.0.0.1:8080`.

### Without Docker

Download `haloid-server-x86_64-unknown-linux-musl` from the latest release
and install it where the service user can write, so it can update itself:

```sh
install -d -o haloid /opt/haloid/bin /var/lib/haloid
install -o haloid -m 755 haloid-server-x86_64-unknown-linux-musl /opt/haloid/bin/haloid-server
apt install libvips-tools ffmpeg libheif-plugin-libde265 libheif-plugin-dav1d   # the media tools it runs
```

Put the configuration in `/etc/haloid/env`:

```sh
DATABASE_URL=mysql://haloid:change-me@127.0.0.1:3306/haloid
HALOID_BIND=127.0.0.1:8080
HALOID_DATA_DIR=/var/lib/haloid
HALOID_SITE_BASE_URL=https://img.example.com
HALOID_ALTCHA_SECRET=change-me                  # openssl rand -hex 32
```

and run it under systemd:

```ini
[Service]
User=haloid
EnvironmentFile=/etc/haloid/env
ExecStart=/opt/haloid/bin/haloid-server
Restart=on-failure
ReadWritePaths=/var/lib/haloid /opt/haloid
```

### The reverse proxy

Allow request bodies as large as your biggest single upload; larger files go
up in 8 MiB chunks. With Caddy:

```
img.example.com {
    request_body {
        max_size 1GB
    }
    reverse_proxy 127.0.0.1:8080
}
```

### First start

**Register an account straight away:** the first account on an instance
becomes its administrator. Then go through **Admin → Settings**: upload
limits, guest uploads, moderation, registration. Locked out? On the server,
`haloid-server admin promote <user>` (in Docker, prefix
`docker compose exec haloid`). `haloid-server help` lists the rest.

### Updates

**Admin → Updates** shows new releases and installs them, or, with the
`auto` setting, installs them at an hour you choose. Releases are signed and
checked before anything is replaced. A binary keeps its previous version
(`haloid-server rollback`); in Docker the updater keeps the old container
until the new one is healthy. The database is not rolled back, so back up
first: `mariadb-dump --single-transaction haloid` and the data directory,
together.

## Licence

Source-available under the
[PolyForm Noncommercial License 1.0.0](LICENSE.md): free for personal,
hobby, learning and other noncommercial use. Commercial use, and the Pro
features (audio hosting, S3-compatible storage, branding removal), need a
Pro licence key.

Releases up to and including commit `7d6c225` were published under
AGPL-3.0-or-later, and remain available under it to anyone who received
them.
