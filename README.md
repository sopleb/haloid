# Haloid

A self-hosted image, video and audio host. One Rust binary, a MariaDB
database, and a SvelteKit frontend.

Existing Chevereto v4, ShareX and ImgBB clients work against it unchanged:
`POST /api/1/upload` is reproduced exactly, including the undocumented
behaviour real clients depend on. Everything else is served by a clean REST
API at `/api/v2`.

## Getting it

Each [release](https://github.com/sopleb/haloid/releases) has a static Linux
binary and a container image, `ghcr.io/sopleb/haloid`. Both are signed, and
an installed Haloid finds and installs new releases itself, from
**Admin → Updates**. Run `haloid-server help` for the commands.

## Licence

Source-available under the
[PolyForm Noncommercial License 1.0.0](LICENSE.md): free for personal,
hobby, learning and other noncommercial use. Commercial use, and the Pro
features (audio hosting, S3-compatible storage, branding removal), need a
Pro licence key.

Releases up to and including commit `7d6c225` were published under
AGPL-3.0-or-later, and remain available under it to anyone who received
them.
