# Upstream captures

Real responses from a live upstream v4 instance's `POST /api/1/upload`,
captured 2026-10-21 and diffed against ours in an `#[ignore]`d conformance
test. The host name is replaced with `img.example.com`; headers are cut to
the status line, `content-type` and `location`. Every upload made for these
was deleted again.

`*.body` is the response byte for byte; `*.headers` the status and the
headers that matter.

What they settled, and where we deliberately differ: field order, the all-null derivative object (five
keys, not six), the error envelopes and statuses per `format`, and that the
ImgBB envelope switch needs `image` as a text field. Our numeric fields stay
strings by decision, where this instance emitted integers.
