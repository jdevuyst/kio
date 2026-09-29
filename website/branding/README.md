# Branding masters

Logo source assets that are **not** shipped by the site. They live outside
`website/.vitepress/public/`, so the VitePress build does not copy them to the deployed
site. The landing page loads an optimized derivative from `public/`; the repo README
references its logo here directly.

| File | What it is | Referenced by |
| --- | --- | --- |
| `kio-logo-home.png` (1206×939 PNG) | Master for the hero image | `../.vitepress/public/kio-logo-home.webp` (600×467 WebP), shown on the landing hero |
| `kio-logo-readme.png` (600×467 PNG) | Repo README logo | the root `README.md` (GitHub renders it from this path) |

The hero is displayed at ≤300 px wide (≤600 px at 2× density), so its derivative is
resized to 600 px and encoded as WebP — visually identical at display size, a fraction
of the bytes. Regenerate it from the master with any image tool, e.g. with `sharp`:

```js
const sharp = require('sharp')
sharp('kio-logo-home.png').resize(600).webp({ quality: 85, effort: 6 })
  .toFile('../.vitepress/public/kio-logo-home.webp')
```
