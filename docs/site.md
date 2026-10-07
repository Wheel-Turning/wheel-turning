# Public site

`site/` holds the static pages served at the apex, `wheelturning.org`, while the demo runs privately at `demo.wheelturning.org` (Spec 01). The pages are a home page, `/rights`, `/privacy`, `/terms` and a 404 page. They are plain HTML and one stylesheet, with no build step and no scripts of their own. The one outside request is the analytics beacon described below.

`/rights` exists because the notices in Spec 09 send teachers, sanghas and publishers to `wheelturning.org/rights`. Its wording follows `copy/rights.md`; the removal form becomes an email address until the demo's own `/rights` route is public.

## Deploying

The site is a Worker with static assets, configured in `wrangler.jsonc` at the repository root. It serves `site/` at the custom domain `wheelturning.org`, which Cloudflare attaches and certifies on deploy. From the repository root:

```bash
npx wrangler deploy
```

Workers static assets serve `privacy.html` at `/privacy`, use `404.html` for unknown paths, and apply `_headers`, which sets a strict Content-Security-Policy. The only outside origins it allows are Cloudflare Web Analytics' script (`static.cloudflareinsights.com`) and beacon endpoint (`cloudflareinsights.com`).

Analytics are cookie-free Cloudflare Web Analytics, turned on in the dashboard under Analytics & Logs, Web Analytics, for `wheelturning.org` with automatic setup, which injects the beacon at the edge. The privacy page's "Visit counts" section describes it; change both together if the tool changes.

## Brand files

`docs/brand/` holds the logo files as delivered: the mark and the lockup, each in color, white and themeable versions, with their content-credential metadata. The site inlines the themeable mark, which draws in `currentColor` with dots in `--accent`, and sets the wordmark as text because the lockup's live Cormorant Garamond text would fall back to another face on most machines.

## Email

The pages name `hello@wheelturning.org` and `rights@wheelturning.org`. Cloudflare Email Routing forwards both to the owner's verified mailbox, set up with `wrangler email routing` (enable, `addresses create`, then one `rules create` per address). A destination address forwards nothing until its owner clicks Cloudflare's verification email. Enabling routing fails while the zone still has the null MX record (`0 .`) Cloudflare adds to a domain with no mail; delete it in the dashboard, or enable routing there and accept its offer to replace conflicting records.

## Previewing locally

```bash
python -m http.server 8787 --directory site
```

The local server does not strip `.html`, so open `/privacy.html` rather than `/privacy`.
