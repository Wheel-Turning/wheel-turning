# Spec 06: API and web interface

Implements the web parts of M5 and M7: the Axum application in `dg-api`, server-rendered with Askama templates and htmx, with no JavaScript build step. A polished interface is out of scope for v1, so the page is plain, fast and readable on a phone. Styling is one hand-written `static/app.css`, and htmx is vendored at a pinned version under `static/`.

| Method and path | Session | Purpose | Detail in |
| --- | --- | --- | --- |
| `GET /healthz` | None | Liveness and a database check | This spec |
| `GET /robots.txt` | None | Disallows everything | This spec |
| `GET /static/*` | None | Stylesheet, vendored htmx, the menu script and `brand/*` | This spec |
| `GET /join/{token}` | None | Exchanges an invite token for a session cookie and redirects to `/` | This spec |
| `GET /rights` | None | The Rights and removal page: the plain promise, the status of every source, and the removal form. An optional `?excerpt=<id>` prefills the form | Spec 08 |
| `POST /rights/request` | None | Stores a removal or report request. Rate limited to 5 per hour per address, with a honeypot field | Spec 08 |
| `GET /` | Session | Home: tagline, question box, the logging policy line and three example questions | This spec |
| `POST /ask` | Session | Runs the query path, persists the answer, and responds `303` to `/a/{answer_id}` | Specs 05 and 06 |
| `GET /a/{answer_id}` | Session, owner only | Answer page, re-rendered from the database | This spec, Specs 05 and 08 |
| `POST /a/{answer_id}/dana` | Session, owner only | Returns the dana split fragment for the entered amounts and logs `split_computed` | Spec 05 |
| `GET /give/{answer_id}/{target}` | Session, owner only | Logs a click and redirects to the sangha's donation link. `target` is a sangha id or `site`. An optional `amount_cents` is used only for template substitution | This spec |
| `POST /a/{answer_id}/feedback` | Session, owner only | Records feedback or flags the query | This spec |
| `GET /talk/{talk_id}` | Session | Talk card with title, teacher, sangha, date, series, part and source link. No transcript | This spec |
| `GET /text/{unit_id}` | Session | A text unit with at most two neighbors on each side, its credit line, source link and report link | Spec 08 |
| `GET /about` | Session | What the demo is, the logging policy, how credit lines work and how to leave | This spec |

Every route marked None works with no session, and every other route returns a generic `404` without one.

Three more routes need no session: `GET /favicon.ico`, which browsers request on their own, `GET /accessibility`, which publishes the accessibility statement, and `POST /accessibility/report`, which stores an accessibility problem report.

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
5. One block per teacher group, headed with the teacher name and the sangha name. Each passage is a `blockquote` containing the verbatim text with footnote markers, followed by a metadata line (talk title, date, series and part), a link reading "Listen at h:mm:ss" that opens `source_link` in a new tab with `rel="noopener noreferrer"`, and, when `give` exists, a "Give to \<sangha>" link to the app's `/give` route.
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

## Accessibility standard

The site conforms to WCAG 3.0, and that standard takes precedence over every other rule in this spec set. If a design, layout, palette or behavior rule here or in any other spec conflicts with a WCAG 3.0 requirement, the requirement wins and the rule changes.

WCAG 3.0 is still a draft, and the spec says what that means for the work. The latest published version, read on October 3, 2026, is a W3C Working Draft dated September 10, 2026. It describes itself as work in progress, says requirements will be added, combined and removed, says it does not replace WCAG 2, and leaves several values to be determined, including the text contrast measure and the readability values for blocks of text and text style. Nobody can certify full WCAG 3.0 conformance today, because final conformance levels do not exist yet. The project therefore works like this:

1. Governing rule. The latest published WCAG 3.0 Working Draft is the standard. Every requirement in it at developing status that applies to this site is in scope, core requirements first. Where the draft and WCAG 2.2 differ, the stricter outcome applies, because WCAG 3.0 does not deprecate WCAG 2.2 and many laws and procurement rules still cite it.
2. Register. `docs/accessibility/wcag3-register.md` lists every applicable requirement from the draft with its name, whether it is core or supplemental, its status (met, partly met, not met or not applicable), how it is met, the test method, the evidence, the date checked and the draft version. It is revised by hand whenever W3C publishes a new draft. A release gate checks that the register names the latest published draft or records why not.
3. Default to WCAG 2.x where the draft is undecided. Wherever the draft leaves a value or a method to be determined, the site meets the WCAG 2.x criterion that covers the same need, and the register records which one. WCAG 2.x here means 2.2 at Level AA, which includes every WCAG 2.0 criterion. A WCAG 3.0 value replaces the default only when the draft settles it and it is stricter. Contrast uses the WCAG 2.x ratios: 4.5:1 for text, and 3:1 for large text, graphical objects and interface component boundaries (criteria 1.4.3 and 1.4.11). Text appearance follows 1.4.4 resize to 200 percent, 1.4.10 reflow at 320 CSS pixels and 1.4.12 text spacing, with a line height of at least 1.5 as the authored default. Target size follows 2.5.8, and the site's 44px rule is stricter, so it stays.
4. Assertions. The draft lets an organization assert practices it follows, and the project asserts only what is true. The design language in this spec is the style guide for focus indicators and design. Every page's text receives a clear language review before release. The first tester cohort includes at least two people who use a screen reader and one who navigates by keyboard only, and their findings are fixed before any wider launch.
5. Public statement. `GET /accessibility` states the standard and draft version, the date of the last evaluation, which core requirements are met, partly met or not met, known gaps and how to report a problem. It never claims more than the register shows.
6. Reporting. `/accessibility` carries a form with the page address, a description, the assistive technology used and optional contact details. It writes `accessibility_report`. Replies are sent by hand until domain email is enabled. The footer links to the page on every screen.

Measured contrast on October 3, 2026, as WCAG 2.2 ratios:

| Pair | WCAG 2.2 ratio | Use |
| --- | --- | --- |
| Ink on paper | 15.5:1 | Body text |
| Ink on band | 14.1:1 | Text on the giving band and tinted areas |
| Muted on paper | 8.1:1 | Dates and captions |
| Muted on band | 7.4:1 | Secondary text on bands |
| Paper on ink | 15.5:1 | Primary button label |
| Error on paper | 7.6:1 | Error text |
| Visited on paper | 7.9:1 | Visited links |
| On-dark on dark | 14.7:1 | Footer text |
| On-dark-muted on dark | 11.8:1 | Footer secondary text |
| Control on paper | 4.7:1 | Input and button borders, which need 3:1 |
| Gold-deep on paper | 3.5:1 | Decorative graphics, which need 3:1 |
| Gold on dark | 8.2:1 | Focus outline and logo dots on the footer |

The logo's bright gold measures 2.0:1 on paper, below the 3:1 that graphics need, so it never appears on a light surface. Buttons stay ink for consistency, although ink on that gold would pass at 7.9:1. A test computes the ratio for every pair in this table from the stylesheet tokens and fails the build on any drop below the thresholds above.

How the requirement areas of the draft shape the interface:

- Images and media. The logo is decorative and hidden from assistive technology, there are no other images, and the site plays no audio or video. Status and confidence are shown in words (resolved, probable, unresolved, verified, unverified), never by color alone. Graphics meet 3:1.
- Text and wording. Text meets the contrast thresholds, resizes to 200 percent and has no text in images. The page language is declared as `lang="en"`, and Pali, Sanskrit and Japanese terms and quotations carry their own `lang` attributes where known. Diacritics in names and terms are preserved exactly as stored. An abbreviation or a foreign term is explained at its first use on a page, so `dana` is defined on About. Long-form pages of 300 words or more, About and the rights statement, open with a two-sentence summary. Scriptural text is exempt from the non-literal language rules, and the site's own wording is kept plain.
- Interactive components. Native controls throughout, a visible focus indicator on everything focusable, a meaningful focus order that follows the visual order, consistent behavior for the same control on every page, and accessible names that contain the visible label.
- Input and operation. Everything works from the keyboard with no traps, no path-based gestures and no reliance on speech or biometrics. Activation happens on release so a press can be cancelled. Targets are at least 44px. No content appears only on hover, and anything that appears on focus can be dismissed.
- Errors. Errors appear in text next to the field, are associated with it programmatically, say how to fix the problem and persist until fixed. The question field validates its 3 to 500 characters with a plain message. A submission announces its status, including the rate limit page, which says when to try again.
- Animation. Nothing flashes. The only motion is the 150-millisecond menu slide, and `prefers-reduced-motion` removes it.
- Layout. Every page has a unique title, one `h1`, headings in order, and landmarks for header, navigation, main and footer. Navigation order and labels are identical on every page. Content reflows without two-dimensional scrolling at 320 CSS pixels and at 400 percent zoom, and orientation is never locked.
- Process and task completion. There are no time limits other than the 30-day session cookie, which is stated on About. The dana panel preselects nothing, uses no urgency wording, and explains before any link opens that no money passes through this site and that the link opens the sangha's own page.
- Help and feedback. About, Rights and removal and Accessibility are linked in the same order in the footer of every page. Feedback exists at every excerpt (Good, Not faithful, Not relevant, Report this excerpt), on every query (Flag this query) and for accessibility itself.
- User control. The site respects `prefers-reduced-motion`, forced colors, user text spacing and user stylesheets. Changes that htmx makes without a page load are announced: the dana split and feedback status appear in a polite live region and focus moves to the new heading when the content is a result the user asked for. Links to source media say what they open, for example "Listen at 12:34 (opens the source video in a new tab)".

Testing has two parts. Every pull request runs `axe-core` with the WCAG 2.2 AA rule sets, an HTML validator, and the contrast, target size, link name and language tests through Playwright at 360, 768 and 1280 pixels on home, answer, text unit, rights, about and accessibility. Every release also gets a manual pass: keyboard only, NVDA with Firefox on Windows, VoiceOver with Safari on iOS and macOS, TalkBack with Chrome on Android, 200 percent text, 400 percent zoom, forced colors, a text spacing override, reduced motion and no JavaScript. Results go in the register. The owner develops on Windows, so a tester with an iPhone or a Mac does the VoiceOver passes, and the register records who ran each pass.

Migration `0017_accessibility_report.sql`:

```sql
create table accessibility_report (
  id uuid primary key default gen_random_uuid(),
  received_at timestamptz not null default now(),
  page_path text,
  description text not null check (char_length(description) between 5 and 4000),
  assistive_tech text,
  contact text,
  status text not null default 'open' check (status in ('open','fixing','fixed','declined')),
  handled_by text,
  handled_at timestamptz,
  note text
);
```

`dg_web` may insert into it. `dg accessibility reports list|show|set` is the CLI for reading and handling reports.

## Logo and icons

The owner's files are the source. Copy `Logo.zip` and the favicon set into the repo under `brand/source/` untouched, and serve optimized copies from `static/brand/`. Each SVG carries an embedded provenance metadata block of about 14 KB, so the build strips the `<metadata>` element from the served copies and leaves the originals alone.

- Header. The mark is the themeable SVG, `logo-mark-themeable.svg`, inlined with `aria-hidden="true"` and `focusable="false"` at 40px, with `color` set to `--ink` and `--logo-accent` set to `--gold-deep`. The name "Wheel Turning" beside it is live text in the serif face, not the lockup image, so it scales, can be selected and meets the text requirements. The link's accessible name is "Wheel Turning, home". Because the mark is inlined and uses `currentColor`, it stays visible in forced colors mode.
- Footer. The same inline mark at 32px with `color` set to `--on-dark` and `--logo-accent` set to `--gold`.
- Other files. `logo-mark-color.svg`, `logo-mark-white.svg` and the three lockup files are for places outside the site, such as the repository README and printed material. The site does not use the lockup images.
- Favicons. The head has `<link rel="icon" href="/static/brand/favicon.svg" type="image/svg+xml">`, `<link rel="icon" href="/favicon.ico" sizes="any">` and `<link rel="apple-touch-icon" href="/static/brand/favicon-180.png">`, plus `<meta name="theme-color" content="#F7F5F0">`. The 32px and 512px PNGs are kept for later use. v1 has no web app manifest.
- Rules. The mark is never recolored except through the two custom properties, never stretched, and never set below 24px. The site's colors come from the mark: ink `#1B1D1F`, cream `#ECEAE4`, gold `#E3A62B`, deep gold `#B8730A` and the tile `#17191B`.

## Design language

The structure borrows from everydayzen.org, read on October 3, 2026: a calm top header with the name at the left and the menu at the right, a slide-out menu on small screens, a quiet opening section with one line of text, sections that each open with a plain heading and a short paragraph, lists of dated items with the date above a linked title, a full-width giving band, and a footer in columns. The treatment is ours: no photographs or stock imagery, a palette and mark taken from the owner's logo files, and system fonts. v1 stays plain and fast, as the PRD requires. Where anything in this section conflicts with the Accessibility standard section, the Accessibility standard wins.

Layout:

1. One column at every width. The container is at most 72rem wide with 1rem side padding on phones and 2rem from 900px. Reading text, passages and the answer page sit in a column of at most 44rem inside it.
2. Two breakpoints, 600px and 900px. Nothing scrolls sideways at 320px, and wide content such as tables scrolls inside its own container.
3. The header is on every page and sticks to the top. It is 4rem tall on phones and 4.5rem from 900px, with a 1px rule beneath it. At the left sit the wheel mark and the name "Wheel Turning" in the serif face, linking to `/`.
4. From 900px the right side of the header holds the text links Ask, About and Rights and removal, each with a 44px-tall target and the current page underlined. On pages other than home a compact question field sits between the name and the links.
5. Below 900px the right side holds a Menu button with a text label, never an icon alone. It opens a panel that slides in from the right, 85 percent of the screen wide up to 22rem, with a dimmed backdrop. The panel begins with the question field and then stacks the same three links in larger type, with a visible Close button at the top. It is built on a `<details>` element so it works without JavaScript, and a small script in `static/` adds Esc to close, a tap on the backdrop to close, and focus return to the Menu button.
6. A "Skip to main content" link is the first focusable element on every page.
7. The viewport tag is `width=device-width, initial-scale=1`. The page never disables zoom or sets a maximum scale.
8. The home page opens with the tagline in the serif face, 2rem on phones and 2.75rem from 900px, then the question box (a textarea of at least 3 rows, full width) with an Ask button, then the logging policy line and three example questions as plain links. No images.
9. Every other section opens with an H2 in the serif face and a short paragraph. Vertical rhythm is 3rem between sections, with a thin rule between major ones.
10. A list of dated items, such as the talks cited, shows the date small and muted above the linked title.
11. The dana panel is a full-width band inside the container with a tinted background, a 3px top rule in the dana color, the heading "Dana" and a primary button "Show split". On phones the amount fields stack.
12. The footer is on every page. From 900px it has three columns, stacked below that. About holds a short description and the logging policy line. Rights and removal holds the one-sentence promise and a link to `/rights`. Sources links to the source status list. The last line reads "Excerpts remain the work of their teachers and publishers." It makes no copyright claim over excerpts.

Type and color:

- Only system font stacks, because the content security policy blocks external fonts. Headings, the name and passages use `Georgia, "Iowan Old Style", "Palatino Linotype", serif`. Body and controls use `system-ui, -apple-system, "Segoe UI", Roboto, sans-serif`.
- Body text is 1.0625rem with a line height of 1.6. A passage is set in the serif face at 1.125rem with a line height of 1.65 and a 3px left rule. Every size is in rem, so text scales with the reader's settings.
- The palette comes from the logo files, as custom properties: `--paper #F7F5F0`, `--band #ECEAE4` (the logo's cream), `--ink #1B1D1F` (the logo's ink), `--muted #444B51`, `--control #6A6F73`, `--rule #CFCABD`, `--gold #E3A62B` (the logo's gold, for dark surfaces only), `--gold-deep #B8730A` (the logo's gold for light surfaces, for graphics only), `--dark #17191B` (the favicon tile), `--on-dark #ECEAE4`, `--on-dark-muted #D6D3CA`, `--error #8C2F1B` and `--visited #5B3F7A`. The light theme is the only theme in v1.
- Text and links use ink. Links are always underlined, and the underline thickens on hover and focus. Visited links use `--visited` and keep the underline.
- Gold never carries text or meaning by itself. `--gold-deep` appears only as the 3px top rule of the dana band, in the logo dots and in similar decoration, always beside words. `--gold` appears only on the dark footer, in the logo there and in focus outlines there.
- Buttons have an ink fill, paper text, a height of at least 44px and a 0.5rem radius. Secondary actions are text links, or buttons outlined in `--control` at 2px. A disabled control stays visible and is explained in words beside it.
- The footer uses `--dark` with `--on-dark` text, which echoes the favicon tile.
- Focus is a 3px outline in ink with a 2px offset on light surfaces and in gold on the dark footer, and is never removed.
- The menu slides for 150 milliseconds, and `prefers-reduced-motion` turns that off.
- Measured contrast for every pair is in the Accessibility standard section.

## Page inventory and rights elements

This table is the single list of what every page must show, including everything the rights and texts specs add.

| Page | Must show |
| --- | --- |
| Home | Tagline, question box, logging policy line, three example questions, the header and the footer |
| Answer | A result line with the teacher count and the text count. A notice banner when one applies. The synthesis section when verified themes exist. Teacher groups, each with passages. Under each talk passage: the source link ("Listen at h:mm:ss"), the credit line, the give link when eligible, the Good, Not faithful and Not relevant buttons, and "Report this excerpt". A "From the texts" section where each unit shows its label, author, translator, edition, page, source link, credit line, "Read in the text" and "Report this excerpt", with no give link. Talks cited. Texts referenced. The dana panel. "Flag this query". |
| Text unit | The unit, up to two neighbors on each side, the credit line, the source link and the report link. Never a whole work |
| Talk card | Title, teacher, sangha, date, series, part and source link. No transcript |
| Rights and removal | The plain promise, the status of every source in plain words, the removal form (name and email optional, what to remove, a message, a hidden honeypot), and the statement that named material is suspended within one business day |
| About | What the demo is, how excerpts are chosen, the logging policy, how credit lines work, how to leave, and a link to `/rights` |
| Error pages | Plain friendly text with no stack trace or database message |

The credit line follows the rights basis and never claims more than exists:

- Permission: "Used with permission of \<name>."
- License: the license name with a link to its text.
- Mandate, texts: the attribution line stored with the mandate, such as "Excerpted with attribution from \<title>, \<publisher>. Free to download at the source: \<url>."
- Mandate, talks: "Talk by \<teacher>, \<sangha>." with the source link.

The words permission and licensed never appear on a mandated excerpt.

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

## Privacy modes for questions

The project needs to see questions to improve, and testers deserve control over them. Every tester chooses between two modes, and this section replaces the disclosure line and the retention rule in the logging list above.

Modes:

1. Keep (`share`). The question text is saved with obvious personal details removed (email addresses, phone numbers, web addresses and any run of nine or more digits), and the text expires after `WT_QUERY_RETENTION_DAYS` (default 180).
2. Count only (`count_only`). The text is kept for `WT_COUNT_ONLY_TTL_HOURS` (default 24) so the tester can return to the answer, then deleted. Only counts remain: mode, teacher count, sangha count, passage count, theme counts, thin coverage, latency and dana events.

Choice and defaults:

- A switch labeled "Keep my questions to help improve Wheel Turning" sits on the home page and in About. It saves to `tester.privacy_mode` and applies to the next question. The default is `share` while the demo is private, with disclosure on the home page and in the invitation. `WT_PRIVACY_DEFAULT` sets the default and is `count_only` before any public launch, with the switch worded as an invitation to help.
- A request that carries `Sec-GPC: 1` from a tester who has never chosen is treated as `count_only`.
- Flagging a question is a deliberate act. A flagged question keeps its text for the full retention period even in count-only mode.

Expiry and deletion:

- A nightly job `purge_expired_queries` sets `query_text` to null for expired rows that are not flagged, and deletes their `answer` rows with the passage ids, themes and feedback attached to them. The counts stay. A permalink for a purged answer shows "This question was not kept."
- `dana_event` rows survive a purge with a null `answer_id`, so link-click counts, which the v1 tests depend on, are not lost.
- `POST /about/delete-my-data` deletes every question, answer, feedback row and dana event that belongs to the tester, after a confirmation. `POST /about/privacy` saves the mode. Both need a session and a CSRF token.

What the owner can learn without reading a question:

- `wt report topics` embeds the stored question texts with the local model, clusters them, and prints each cluster with a count and a label made from the most central question's keywords. A cluster with fewer than 3 questions is folded into "other", so no small group of people can be picked out.
- `wt report gaps` shows clusters with high shares of thin coverage or no excerpt above the relevance threshold. These are the topics the collection cannot answer yet and guide what to add next.
- `wt report feedback` shows Good, Not faithful and Not relevant counts by cluster and by mode.
- Individual question text is printed only for flagged questions, through `flagged_queries()` as before.

Migration `0019_privacy_modes.sql`:

```sql
alter table tester add column privacy_mode text not null default 'share'
  check (privacy_mode in ('share','count_only'));
alter table tester add column privacy_chosen boolean not null default false;

alter table query_log alter column query_text drop not null;
alter table query_log add column privacy_mode text not null default 'share'
  check (privacy_mode in ('share','count_only'));
alter table query_log add column text_expires_at timestamptz;
create index query_log_expiry_idx on query_log (text_expires_at) where query_text is not null;

alter table dana_event drop constraint dana_event_answer_id_fkey;
alter table dana_event alter column answer_id drop not null;
alter table dana_event add constraint dana_event_answer_id_fkey
  foreign key (answer_id) references answer(id) on delete set null;
```

The owner role can still read every table, as the logging list says, so this section describes what the product does and not what a database administrator could do. The disclosure copy is in Spec 09.

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

Acceptance criteria for texts, rights and design:

- [ ] Every route in the table exists. A route not marked None returns a generic `404` without a session, and `/rights` and `/robots.txt` work with none.
- [ ] The answer page shows the "From the texts" section with a credit line, source link and report link on each unit and no give link. Text hits are absent from the teacher count and the dana split.
- [ ] `GET /text/{unit_id}` shows at most two neighbors on each side and returns `404` for a unit that is not in `servable_text_unit`.
- [ ] Snapshot tests cover the credit line for permission, license and mandate, for talks and texts, and assert that permission and licensed are absent on a mandate.
- [ ] "Report this excerpt" opens `/rights?excerpt=<id>` with the excerpt prefilled for both talk passages and text units, and the form stores a request.
- [ ] At 360px there is no horizontal scroll and the Menu button shows a text label. The panel opens from the button, closes from the Close button, the backdrop and Esc, and returns focus to the button. From 900px the links show inline with no Menu button.
- [ ] The "Skip to main content" link is the first tab stop, and a test asserts that the viewport tag contains neither `user-scalable=no` nor `maximum-scale`.
- [ ] Header links, buttons and feedback buttons have targets of at least 44px.
- [ ] An automated accessibility run passes WCAG AA on home, answer, text unit, rights and about at 360px and at 1280px.
- [ ] Every page works without JavaScript: the menu opens through `<details>`, a question posts, and the dana split uses its fallback.
- [ ] No template or stylesheet references an external host or font, checked by a grep test.
- [ ] The footer appears on every page, and its last line contains no copyright claim over excerpts.
- [ ] With `prefers-reduced-motion`, the menu does not animate.

Acceptance criteria for the accessibility standard and the logo:

- [ ] `docs/accessibility/wcag3-register.md` exists, names the draft of September 10, 2026 or a later published one, lists every applicable requirement with status and evidence, and the release gate passes only when its draft version matches the latest published one or records why not.
- [ ] A test computes the WCAG 2.2 ratio for every pair in the measured contrast table from the stylesheet tokens and fails below the thresholds, and a test fails if bright gold appears on a light surface.
- [ ] Where WCAG 3.0 and WCAG 2.2 differ, the stricter outcome is applied and the difference is recorded in the register.
- [ ] `GET /accessibility` and `POST /accessibility/report` work with no session, the report form stores a row, and the statement lists exactly the statuses in the register.
- [ ] Every page has one `h1`, a unique title, the header, navigation, main and footer landmarks, and `lang="en"`. Pali, Sanskrit and Japanese terms carry a `lang` attribute where the source gives one, and diacritics are preserved in rendered text.
- [ ] Status and confidence badges carry words, and a grayscale screenshot test shows no information lost.
- [ ] At 320 CSS pixels and at 400 percent zoom, no page needs two-dimensional scrolling. With 200 percent text and a text spacing override, no content or function is lost.
- [ ] Dynamic updates are announced: the dana split and feedback status appear in a polite live region, and focus moves to the result heading when the user asked for the result.
- [ ] The dana panel preselects nothing and states, before any link opens, that no money passes through the site.
- [ ] Source links name what they open, for example "Listen at 12:34 (opens the source video in a new tab)".
- [ ] The manual pass for the release is recorded: keyboard only, NVDA with Firefox, VoiceOver on iOS and macOS, TalkBack with Chrome, forced colors, reduced motion and no JavaScript.
- [ ] The header mark is inline, hidden from assistive technology, named by the adjacent text, stays visible in forced colors mode, and the footer mark follows `--on-dark`.
- [ ] `GET /favicon.ico` works with no session, the head carries the SVG, ICO and 180px icon links, and the served SVG files carry no `<metadata>` element while the originals in `brand/source/` are unchanged.
- [ ] The first tester cohort includes at least two people who use a screen reader and one who navigates by keyboard only, and each of their findings is fixed or recorded in the register before any wider launch.

Acceptance criteria for question privacy:

- [ ] The switch appears on the home page and in About, saves to `tester.privacy_mode`, and applies to the next question.
- [ ] In `share` mode the stored text has emails, phone numbers, web addresses and runs of nine or more digits removed, and a test covers each pattern.
- [ ] In `count_only` mode the text is deleted after the TTL and the answer row goes with it, the counts stay, and the permalink shows "This question was not kept."
- [ ] A flagged question survives the purge in both modes.
- [ ] A request with `Sec-GPC: 1` from a tester who has never chosen is treated as `count_only`, and an explicit choice overrides it.
- [ ] A purge leaves `dana_event` rows with a null `answer_id`, and the daily click count does not change.
- [ ] `POST /about/delete-my-data` removes every row that belongs to the tester after a confirmation, and does not touch another tester's rows.
- [ ] `wt report topics` folds any cluster of fewer than 3 questions into "other" and prints no individual question text, and `wt report gaps` lists only cluster-level counts.
