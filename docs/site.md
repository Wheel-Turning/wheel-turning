# Public site

`site/` holds the static pages served at the apex, `wheelturning.org`, while the demo runs privately at `demo.wheelturning.org` (Spec 01). The pages are a home page, `/rights`, `/privacy`, `/terms` and a 404 page. They are plain HTML and one stylesheet, with no build step, no scripts and no third-party requests.

`/rights` exists because the notices in Spec 09 send teachers, sanghas and publishers to `wheelturning.org/rights`. Its wording follows `copy/rights.md`; the removal form becomes an email address until the demo's own `/rights` route is public.

## Deploying on Cloudflare Pages

1. In the Cloudflare dashboard, open Workers & Pages, create a Pages project, and connect this repository.
2. Set the production branch to `main`, leave the build command empty, and set the build output directory to `site`.
3. Under Custom domains, add `wheelturning.org` (and `www.wheelturning.org` if wanted, redirected to the apex).

Or deploy from a terminal without connecting the repository:

```bash
npx wrangler pages deploy site --project-name wheel-turning
```

Cloudflare Pages serves `privacy.html` at `/privacy`, uses `404.html` for unknown paths, and applies `_headers`, which sets a strict Content-Security-Policy. Turning on Cloudflare Web Analytics injects a script that this policy blocks, and the privacy page says the site runs no analytics, so change both together.

## Email

The pages name `hello@wheelturning.org` and `rights@wheelturning.org`. Cloudflare Email Routing can forward both to the owner's mailbox at no cost: Email, Email Routing, enable it for the zone, and add the two addresses as custom addresses. Until that is done, mail to them bounces.

## Previewing locally

```bash
python -m http.server 8787 --directory site
```

The local server does not strip `.html`, so open `/privacy.html` rather than `/privacy`.
