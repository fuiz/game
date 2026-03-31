# Corkboard Server

Self-hostable image storage server built with actix-web. Images are persisted to the filesystem and served through a size-bounded in-memory cache.

## Running

```bash
cargo run -p corkboard-server
```

## Configuration

Server settings are loaded from environment variables prefixed with `CORKBOARD_`:

| Variable                    | Default               | Description                           |
| --------------------------- | --------------------- | ------------------------------------- |
| `CORKBOARD_HOSTNAME`        | `0.0.0.0`             | Address to bind to                    |
| `CORKBOARD_PORT`            | `5040`                | Port to listen on                     |
| `CORKBOARD_ALLOWED_ORIGINS` | `[]`                  | Allowed CORS origins (JSON array)     |
| `CORKBOARD_STORAGE_DIR`     | `./corkboard-data`    | Directory for storing images on disk  |
| `CORKBOARD_CACHE_SIZE`      | `268435456` (256 MiB) | Maximum in-memory cache size in bytes |

When `CORKBOARD_ALLOWED_ORIGINS` is empty, CORS is fully permissive.

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
