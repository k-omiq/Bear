// Writes release files to the cleaner-updates bucket for GitHub Actions, with
// Authorization: Bearer $PUBLISH_TOKEN. Nothing else is served here; reads go
// to https://cleaner.komiq.cc/<key>.
//
//   PUT    /<key>                                one request, the whole file
//   POST   /<key>?uploads                        start a multipart upload: {key, uploadId}
//   PUT    /<key>?uploadId=<id>&partNumber=<n>   one part: {partNumber, etag}
//   POST   /<key>?uploadId=<id>                  finish, body {parts: [{partNumber, etag}]}
//   DELETE /<key>?uploadId=<id>                  give the upload up
//
// Bodies stream straight into R2. Workers cap a request body at 100 MB on the
// free plan, and a larger one fails with 413 from the platform rather than
// being truncated. The Linux AppImage is larger than that, so a file over the
// cap goes up in parts (R2 wants every part but the last to be at least 5 MB).

function timingSafeEqual(a, b) {
  const enc = new TextEncoder();
  const x = enc.encode(a);
  const y = enc.encode(b);
  if (x.byteLength !== y.byteLength) return false;
  return crypto.subtle.timingSafeEqual(x, y);
}

const MAX_PARTS = 10000;

export default {
  async fetch(request, env) {
    const auth = request.headers.get("authorization") || "";
    const token = auth.startsWith("Bearer ") ? auth.slice(7) : "";
    if (!env.PUBLISH_TOKEN || !token || !timingSafeEqual(token, env.PUBLISH_TOKEN)) {
      return new Response("unauthorized", { status: 401 });
    }
    const url = new URL(request.url);
    const key = decodeURIComponent(url.pathname.replace(/^\/+/, ""));
    if (!key || key.includes("..")) {
      return new Response("bad key", { status: 400 });
    }
    const contentType = request.headers.get("content-type") || "application/octet-stream";
    const uploadId = url.searchParams.get("uploadId");

    try {
      if (request.method === "PUT" && uploadId === null) {
        const object = await env.UPDATES.put(key, request.body, { httpMetadata: { contentType } });
        return Response.json({ key: object.key, size: object.size, etag: object.httpEtag });
      }
      if (request.method === "POST" && url.searchParams.has("uploads")) {
        const upload = await env.UPDATES.createMultipartUpload(key, { httpMetadata: { contentType } });
        return Response.json({ key: upload.key, uploadId: upload.uploadId });
      }
      if (uploadId) {
        const upload = env.UPDATES.resumeMultipartUpload(key, uploadId);
        if (request.method === "PUT") {
          const partNumber = Number(url.searchParams.get("partNumber"));
          if (!Number.isInteger(partNumber) || partNumber < 1 || partNumber > MAX_PARTS) {
            return new Response("bad partNumber", { status: 400 });
          }
          if (!request.body) return new Response("empty part", { status: 400 });
          const part = await upload.uploadPart(partNumber, request.body);
          return Response.json({ partNumber: part.partNumber, etag: part.etag });
        }
        if (request.method === "POST") {
          const { parts } = await request.json();
          if (!Array.isArray(parts) || parts.length === 0 || parts.length > MAX_PARTS) {
            return new Response("bad parts", { status: 400 });
          }
          const object = await upload.complete(parts);
          return Response.json({ key: object.key, size: object.size, etag: object.httpEtag });
        }
        if (request.method === "DELETE") {
          await upload.abort();
          return new Response(null, { status: 204 });
        }
      }
    } catch (error) {
      // A wrong uploadId or part list is R2 refusing the request, not a fault here.
      return new Response(`upload failed: ${error.message}`, { status: 400 });
    }
    return new Response("method not allowed", { status: 405, headers: { allow: "PUT, POST, DELETE" } });
  },
};
