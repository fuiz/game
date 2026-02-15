# Corkboard

Service to store images temporarily.

## REST API

### Upload

```http
POST -F image=bytes /upload
```

Ok Response: `"MediaID"`. The image stays availble for an hour.

### Retrieve

```http
GET /get/{media_id}
```

It responds with bytes of content-type: `image/png`.

### Compute Thumbnail

```http
POST -F image=bytes /thumbnail
```

It responds with bytes of content-type: `image/png`.
