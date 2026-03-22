# Corkboard

Temporary image storage service for Fuiz. Handles image uploads, retrieval, and thumbnail generation.

## Crates

| Crate                          | Description                                        |
| ------------------------------ | -------------------------------------------------- |
| [`server`](server/)            | Self-hostable image storage server (actix-web)     |
| [`cloudflare`](cloudflare/)    | Serverless image storage on Cloudflare Workers     |

## API

### Upload

```http
POST /upload
```

Accepts a multipart form with an `image` field. Returns a `MediaID`. Images expire after an hour (server) or 24 hours (cloudflare).

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
