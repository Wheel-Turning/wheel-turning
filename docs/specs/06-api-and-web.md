<!-- Exported from the "Dharma Gates: V1 Technical Specs" doc (https://claude.ai/code/artifact/68e63636-e0f6-43ee-ab79-ccc03b76946d), tab "06 API and web", on 2026-10-03. The doc is the source of truth until this file is edited in the repo. -->
# Spec 06: API and web interface

Implements the web parts of M5 and M7: the Axum application in `dg-api`, server-rendered with Askama templates and htmx, with no JavaScript build step. A polished interface is out of scope for v1, so the page is plain, fast and readable on a phone. Styling is one hand-written `static/app.css`, and htmx is vendored at a pinned version under `static/`.

| Method and path | Auth | Purpose |
| --- | --- | --- |
| `GET /healthz` | None | Liveness and a database check |
| `GET /join/{token}` | None | Exchanges an invite token for a session cookie and redirects to `/` |
| `GET /` | Session | Search page with the question box, the logging policy line, and three example questions |
| `POST /ask` | Session | Runs the query path, persists the answer, and responds `303` to `/a/{answer_id}` |
| `GET /a/{answer_id}` | Session, owner only | Answer page, re-rendered from the database |
| `POST /a/{answer_id}/dana` | Session, owner only | Returns the dana split fragment for the entered amounts and logs `split_computed` |
| `GET /give/{answer_id}/{target}` | Session, owner only | Logs a click and redirects to the sangha's donation link. `target` is a sangha id or `site`. An optional `amount_cents` query parameter is used only for template substitution. |
| `POST /a/{answer_id}/feedback` | Session, owner only | Records feedback or flags the query |
| `GET /talk/{talk_id}` | Session | Talk card with title, teacher, sangha, date, series, part and source link. No transcript. |
| `GET /about` | Session | What the demo is, the logging policy, and consent and attribution notes |

An ask is rate limited per tester by `ask_rate_per_hour` (default 30) with an in-memory token bucket, and excess requests get a friendly `429` page.

## Invite token access

The PRD puts accounts out of scope, so access is an invite link and a cookie.

1. `dg tester invite --label <text>` creates 32 random bytes from the operating system generator, encodes them URL-safe, stores only the SHA-256 in `tester.token_hash`, and prints the invite URL once. `dg tester list` and `dg tester revoke --label <text>` manage testers.
2. `GET /join/{token}` hashes the token and looks it up by the unique index. If the tester exists and is not revoked, it sets the cookie `dg_session` and redirects to `/`. Any other result returns a generic `404` with no hint.
3. The cookie value is the tester id and issue time, signed with HMAC-SHA256 under `DG_SESSION_SECRET`. It is `HttpOnly`, `Secure` outside `dev`, `SameSite=Lax`, with a 30-day lifetime.
4. Middleware verifies the signature on every request and checks that the tester is not revoked, with a 60-second cache. It updates `last_seen_at` at most once an hour.
5. State-changing routes are `POST` and require a CSRF token, an HMAC of the session value rendered into each form as a hidden field.
6. The token never appears in logs. The request logger records the path as `/join/[redacted]`.
7. Every response carries `Content-Security-Policy: default-src 'self'`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and `X-Robots-Tag: noindex`, and `/robots.txt` disallows everything. There are no inline scripts.

## Answer page structure

The answer page renders `AnswerView` from Spec 05 in this order. Templates live in `crates/dg-api/templates/`.

1. Header with the site name and a link to `/about`.
2. The question as asked, then one line stating the result honestly, such as "Passages from 3 teachers" or "Found passages from 2 teachers for this question".
3. A notice banner when `AnswerView.notice` is set, with fixed copy for each `Notice` variant.
4. If themes exist, a section titled "How approaches differ" with one line per theme, each followed by chips that jump to the numbered passages that support it. No synthesis section appears when there are no verified themes.
5. One block per teacher group, headed with the teacher name and the sangha name. Each passage is a `blockquote` containing the verbatim text with footnote markers, followed by a metadata line (talk title, date, series and part), a link reading "Listen at h:mm:ss" that opens `source_link` in a new tab with `rel="noopener noreferrer"`, and, when `give` exists, a "Give to <sangha>" link to the app's `/give` route.
6. Under each passage, three small feedback buttons: Good, Not faithful, Not relevant. They post by htmx and degrade to plain forms.
7. "Talks cited" and "Texts referenced" lists, with confidence badges on the texts and the word "unresolved" shown as is.
8. The dana panel. The donor enters a gift total and an optional site amount in dollars and presses "Show split". The htmx fragment lists each eligible sangha with its amount and a link to open that sangha's giving page, then the site line on its own row, then total outlay. It states that no money passes through this site and that each link opens the sangha's own page. With no eligible recipient it says so plainly. Without JavaScript the form posts and the page reloads with the split shown.
9. A "Flag this query" button that states, before and after pressing, that a flagged query can be read by the founder.

Rendering rules:

- Passage text appears only inside passage blockquotes and is HTML-escaped by the template engine. Footnote markers are inserted from `TextRun` values, never by string concatenation.
- Text from a model appears only in the synthesis section.
- Nothing shows a full transcript, and no passage is truncated mid-word.
- Durations and offsets render as `h:mm:ss`.
- Error pages are plain and friendly and never show stack traces or database errors.
- The layout is single column, mobile first, with visible focus outlines, semantic headings, and text contrast of at least WCAG AA. All controls work from the keyboard.
- A loading indicator shows while `/ask` runs, since a query takes several seconds.

## Logging, feedback and log access

The PRD's policy is that query logs are disclosed to testers, read in aggregate, and opened individually only when a tester flags a query. The design makes that mechanically true.

1. Each ask inserts one `query_log` row, one `answer` row and the `answer_passage` rows in a single transaction. If that transaction fails, the request fails with a friendly error and shows no answer. The system fails closed so no answer is ever shown without being logged as disclosed.
2. Stage timings (embed, search, rerank, select, synthesize, verify, assemble) are written to the structured log without query text. Only the total goes in `query_log.latency_ms`.
3. The three give routes work like this. `POST /a/{id}/dana` writes a `dana_event` of kind `split_computed` with the entered total. Opening the panel writes `panel_open`. `GET /give/...` writes `link_click` with the sangha id (null for the site line), then redirects. The redirect target comes from the registry row, never from a request parameter, and an unlisted or unverified sangha returns `404`.
4. Feedback kinds are `good`, `not_faithful`, `not_relevant`, `note` and `flag_query`. `flag_query` also sets `query_log.flagged` and `flagged_at`, which `dg_web` is allowed to update and nothing else.
5. Disclosure copy appears under the search box and on `/about`: "Your questions are logged. They are read only in aggregate, and one is opened only if you flag it."
6. The reporting commands run as `dg_analyst`: `dg report answers --since <duration>` (from `v_answer_stats`), `dg report dana`, `dg report coverage` (from `v_consent_coverage`), and `dg report flagged` (through `flagged_queries()`). None of them can read unflagged query text.
7. The owner role can read every table, so reading raw queries with it is outside the stated policy. The CLI offers no command that does so, and the runbook says the role is for migrations and break-glass repair only.
8. `dg tester delete --label <text>` removes that tester and every query log, answer, feedback and dana event row for them, for a tester who asks to be forgotten. Query logs otherwise stay until the test ends.

## Acceptance criteria

- [ ] `/healthz` returns success only when the database answers.
- [ ] A valid invite token sets a correctly flagged cookie and redirects. An invalid or revoked token returns the same generic `404`. The token never appears in logs.
- [ ] A state-changing request without a valid CSRF token is rejected.
- [ ] `POST /ask` persists an answer, responds `303` to the permalink, and the permalink is readable only by the tester who asked.
- [ ] Snapshot tests with `insta` cover the answer page for standard, named, thin coverage, none found, and multi-named cases.
- [ ] A passage whose text contains `<script>` renders escaped, and no model text appears outside the synthesis section.
- [ ] `/give` logs a click and redirects only to the registry URL. A tampered `amount_cents`, an unverified sangha and an unlisted sangha are each rejected.
- [ ] The dana fragment equals the output of the `dg-dana` function for the same inputs, and the no-JavaScript path shows the same split.
- [ ] `flag_query` sets the flag, and the flagged query is readable only through `flagged_queries()`.
- [ ] `dg report` commands run as `dg_analyst` and cannot read unflagged query text.
- [ ] A failed log insert produces an error page and no answer.
- [ ] The rate limit returns `429` after `ask_rate_per_hour` asks in an hour.
- [ ] Security headers and `robots` rules are present on every response.
- [ ] A manual keyboard-only pass completes ask, feedback, dana split and flag with no mouse.
