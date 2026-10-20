"""Upload one release file through the publish worker (infra/publish-worker).

    python publish-upload.py <local file> <remote key> [content type]

A file up to SINGLE_LIMIT goes in one PUT. A larger one, which the Workers
request body cap (100 MB) would refuse with 413, goes up as an R2 multipart
upload in PART_SIZE parts, and is given up on the worker if a part fails.
The token is read from CLEANER_PUBLISH_TOKEN.
"""
import json
import os
import sys
import urllib.parse
import urllib.request

BASE = "https://cleaner-publish.komiq.cc"
SINGLE_LIMIT = 90 * 1024 * 1024
PART_SIZE = 50 * 1024 * 1024


def call(method, key, query="", body=None, content_type="application/octet-stream"):
    url = f"{BASE}/{urllib.parse.quote(key)}" + (f"?{query}" if query else "")
    request = urllib.request.Request(url, data=body, method=method, headers={
        "Authorization": f"Bearer {os.environ['CLEANER_PUBLISH_TOKEN']}",
        "Content-Type": content_type,
        # Cloudflare's bot rules answer Python-urllib's own user agent with 403.
        "User-Agent": "manga-cleaner-release",
    })
    with urllib.request.urlopen(request, timeout=600) as response:
        text = response.read().decode()
    return json.loads(text) if text else None


def upload(local, key, content_type="application/octet-stream"):
    size = os.path.getsize(local)
    if size <= SINGLE_LIMIT:
        with open(local, "rb") as source:
            return call("PUT", key, body=source.read(), content_type=content_type)
    upload_id = call("POST", key, "uploads", content_type=content_type)["uploadId"]
    query = f"uploadId={urllib.parse.quote(upload_id)}"
    try:
        parts = []
        with open(local, "rb") as source:
            number = 1
            while chunk := source.read(PART_SIZE):
                part = call("PUT", key, f"{query}&partNumber={number}", body=chunk)
                parts.append({"partNumber": part["partNumber"], "etag": part["etag"]})
                print(f"  part {number}: {len(chunk)} bytes", flush=True)
                number += 1
        return call("POST", key, query, body=json.dumps({"parts": parts}).encode(),
                    content_type="application/json")
    except BaseException:
        try:
            call("DELETE", key, query)
        except Exception:
            pass
        raise


if __name__ == "__main__":
    result = upload(*sys.argv[1:4])
    print(json.dumps(result))
