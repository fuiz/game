# Corkboard Server

Self-hostable image storage server built with actix-web.

## Running

```bash
cargo run -p corkboard-server
```

## REST API

### Upload

```http
POST /upload
```

Accepts a multipart form with an `image` field. Returns a `MediaID`. The image stays available for an hour.

### Retrieve

```http
GET /get/{media_id}
```

Returns the image as `image/png`.

### Compute Thumbnail

```http
POST /thumbnail
```

Accepts a multipart form with an `image` field. Returns a thumbnail as `image/png`.
