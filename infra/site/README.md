# cleaner.komiq.cc

The public site and the read side of the release bucket, in one Cloudflare
Worker.

- `public/` is served by Workers Assets: the landing page, the icon, and the
  demo video the README also links.
- `src/index.js` handles everything Assets does not match by reading it from
  the `cleaner-updates` R2 bucket. That is `latest.json`, which installed apps
  poll for updates, and `releases/v<version>/<file>` for the installers and
  their signatures. It is read-only, and supports range requests and
  conditional GETs so large installers resume.

Writes go to the separate `cleaner-publish` worker beside this one, which holds
the only write token and is called by the release workflow.

## Deploy

```bash
cd infra/site
npx wrangler deploy
```

The first deploy takes `cleaner.komiq.cc` over from the bucket's own R2 custom
domain. Remove that custom domain in the Cloudflare dashboard first, under
**R2 › cleaner-updates › Settings › Custom Domains**, or the hostname will be
claimed twice. Published URLs do not change: the worker serves the same keys
from the same bucket.

## Download links

The four buttons (macOS, Windows, Linux `.deb`, Linux AppImage) start with a
GitHub releases fallback, then rewrite themselves from `/latest.json`. The
visitor's own system goes first and its first-open help starts expanded. The file names follow the release workflow's rename step,
`MangaCleaner_<version>_<platform>.<ext>`, so a new release needs no edit here.
