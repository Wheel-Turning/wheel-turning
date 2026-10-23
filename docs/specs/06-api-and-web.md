# Spec 06: API and web interface

Implements the web parts of M5 and M7: the Axum application in `wt-api`, server-rendered with Askama templates and htmx, with no JavaScript build step. A polished interface is out of scope for v1, so the page is plain, fast and readable on a phone. Styling is one hand-written `static/app.css`, and htmx is vendored at a pinned version under `static/`.

| Method and path | Session | Purpose | Detail in |
| --- | --- | --- | --- |
| `GET /healthz` | None | Liveness and a database check | This spec |
| `GET /robots.txt` | None | Disallows everything | This spec |
| `GET /static/*` | None | Stylesheet, vendored htmx, the menu, dana and progress scripts and `brand/*` | This spec |
| `GET /join/{token}` | None | Exchanges an invite token for a session cookie and redirects to `/` | This spec |
| `GET /rights` | None | The Rights and removal page: the plain promise, the status of every source, and the removal form. An optional `?excerpt=<id>` prefills the form | Spec 08 |
| `POST /rights/request` | None | Stores a removal or report request. Rate limited to 5 per hour per address, with a honeypot field | Spec 08 |
| `GET /` | Session | Home: heading, a three-step guide, question box, the logging policy line and three example questions | This spec |
| `POST /ask` | Session | Runs the query path, persists the answer, and responds `303` to `/a/{answer_id}` | Specs 05 and 06 |
| `GET /a/{answer_id}` | Session, owner only | Answer page, re-rendered from the database | This spec, Specs 05 and 08 |
| `POST /a/{answer_id}/dana` | Session, owner only | Returns the dana split fragment for the entered amounts and logs `split_computed` | Spec 05 |
| `GET /give/{answer_id}/{target}` | Session, owner only | Logs a click and redirects to the sangha's donation link. `target` is a sangha id or `site`. An optional `amount_cents` is used only for template substitution | This spec |
| `POST /a/{answer_id}/feedback` | Session, owner only | Records feedback or flags the query | This spec |
| `GET /talk/{talk_id}` | Session | Talk card with title, teacher, sangha, date, series, part and source link, and a "Read the transcript" link when the talk is at display level `full` | This spec |
| `GET /talk/{talk_id}/transcript` | Session | The full transcript of a talk at display level `full`, built from its servable passages in order; `404` at any other level | This spec, Spec 03 Display levels |
| `GET /text/{unit_id}` | Session | A text unit with at most two neighbors on each side, its credit line, source link and report link | Spec 08 |
| `GET /about` | Session | What the demo is, the logging policy, how credit lines work and how to leave | This spec |

Every route marked None works with no session, and every other route returns a generic `404` without one.

Three more routes need no session: `GET /favicon.ico`, which browsers request on their own, `GET /accessibility`, which publishes the accessibility statement, and `POST /accessibility/report`, which stores an accessibility problem report.

An ask is rate limited per tester by `ask_rate_per_hour` (default 30) with an in-memory token bucket, and excess requests get a friendly `429` page.

## Invite token access

The PRD puts accounts out of scope, so access is an invite link and a cookie.

1. `wt tester invite --label <text>` creates 32 random bytes from the operating system generator, encodes them URL-safe, stores only the SHA-256 in `tester.token_hash`, and prints the invite URL once. `wt tester list` and `wt tester revoke --label <text>` manage testers.
2. `GET /join/{token}` hashes the token and looks it up by the unique index. If the tester exists and is not revoked, it sets the cookie `wt_session` and redirects to `/`. Any other result returns a generic `404` with no hint.
3. The cookie value is the tester id and issue time, signed with HMAC-SHA256 under `WT_SESSION_SECRET`. It is `HttpOnly`, `Secure` outside `dev`, `SameSite=Lax`, with a 30-day lifetime.
4. Middleware verifies the signature on every request and checks that the tester is not revoked, with a 60-second cache. It updates `last_seen_at` at most once an hour.
5. State-changing routes are `POST` and require a CSRF token, an HMAC of the session value rendered into each form as a hidden field.
6. The token never appears in logs. The request logger records the route template, `/join/{token}`, never the path.
7. Every response carries `Content-Security-Policy: default-src 'self'; frame-ancestors 'none'`, `Permissions-Policy: camera=(), microphone=(), geolocation=(), browsing-topics=()`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer` and `X-Robots-Tag: noindex`. In `WT_ENV=prod` only, it also carries `Strict-Transport-Security: max-age=31536000` (Spec 11, PB3.5). And `/robots.txt` disallows everything. There are no inline scripts.

## Answer page structure

The answer page renders `AnswerView` from Spec 05 in this order. Templates live in `crates/wt-api/templates/`.

1. Header with the site name and a link to `/about`.
2. The question as asked, then one line stating the result honestly, such as "3 teachers, 4 teachings, 8 excerpts" (teachings are distinct talks, each noun singular for 1; Spec 11, PB2.3) or "Found passages from 2 teachers for this question". Sticky question (owner's decision of October 12, 2026): once the question's heading scrolls out of view, the question shows again in smaller text as a second row of the sticky header, so a reader deep in the excerpts still sees what was asked. The row is a visual copy, hidden from assistive technology so the question is announced once, by its `h1`. It appears and disappears without animation. While it shows, the page's scroll padding grows by its height, so a focused control or an anchor target (`#p3`, `#ref-2`) never lands beneath it. It is clamped to two lines, since the full question stays in the `h1`. Without JavaScript the row never appears and the page is otherwise the same.
3. A notice banner when `AnswerView.notice` is set, with fixed copy for each `Notice` variant.
4. The summary (Spec 05, Summary and verifier; owner's decision of October 12, 2026) in `<section id="summary">`: the heading "Summary", the headline, the framing paragraph, the sections "Where the talks agree", "Where they differ" and "Points to keep in mind", the sum-up and the drawn-from line. Each sentence is followed by numbered markers that link to its quoted lines, an ordered list at the end of the section in which each entry holds the verbatim quotation in a `<q>` element, the teacher, the talk title and date, and a link to the numbered excerpt below. No summary appears when its headline did not survive. While the summary is still being written (Spec 05, Summary and verifier), the section holds the heading and "Writing a summary of these excerpts. It appears here in a few seconds; the excerpts below are ready now.", and `answer.js` asks `GET /a/{answer_id}/summary` every second for up to 90 seconds: `200` with the section's HTML, which replaces the placeholder, `202` while it is pending and `204` when there is none, which removes it. A polite status line says "The summary is ready, above the excerpts." and focus never moves. Without JavaScript a line asks the reader to reload. A summary still pending after 90 seconds counts as none. An answer stored before the summary shows its themes under "How approaches differ" with chips that jump to the numbered passages. It has no feedback control of its own (owner's decision of October 15, 2026; rule 6).
5. The passages in score order, best first (Spec 05, Answer assembly, rule 1, as the owner decided on October 15, 2026), each run of consecutive passages from one teacher headed with the teacher name and the sangha name. A passage that answers a stored student question is preceded, outside its blockquote, by "Student:" and the edited question, with the line "Question lightly edited by machine; names and identifying details removed." (Spec 04, Student questions). Each passage is a `blockquote` containing the verbatim text with footnote markers, followed by a metadata line (talk title, date, series and part, linking to the talk's References entry), the source links of Spec 05's Answer assembly rule 2, each opening in a new tab with `rel="noopener noreferrer"` (the talk's own transcript at its source when the source made the transcript; otherwise "Listen at h:mm:ss" and, at display level `full`, "Read our transcript" linking to `/talk/{talk_id}/transcript#p{ordinal}`), and, when `give` exists, a "Give to this sangha" link to the app's `/give` route.
   Excerpt footer (owner's decision of October 12, 2026, moving Spec 00 Post-beta item 6 before beta). The links under a passage form one row in this order: Listen, Read, Cite, Give, Report. Each is a short visible word with a longer accessible name that begins with it: "Listen" (Listen at h:mm:ss, opens the source in a new tab), "Read" (the source's transcript or "Read our transcript", by Spec 05's Answer assembly, rule 2; absent when neither exists), "Cite" (a `<details>` that opens this excerpt's MLA entry, the same text as its References entry, with a "Copy citation" button), "Give to this sangha" (accessible name "Give to this sangha, \<sangha>", only when `give` exists; owner's decision of October 15, 2026) and "Report" (Report this excerpt, to `/rights?excerpt=<id>`). A player without a time shows "starts at h:mm:ss" after Listen, or in its place when there is no Listen link. The row keeps this order for every talk, so where Spec 05's Answer assembly, rule 2 puts the source's own transcript first, the footer shows it as Read, after Listen. Every item is a 44px target. The row wraps onto as many lines as it needs and never scrolls sideways, at 320px included. Cite works without JavaScript: the `<details>` opens and the citation text can be selected; the copy button is added by a script and announces "Citation copied." in a polite live region. The credit line follows the row.
6. One feedback control at the foot of the page, after the dana panel and before "Flag this query" (owner's decision of October 15, 2026, replacing the control under each passage and the summary's own control of October 12). It is a `<details>` element with id `fb-answer` whose summary reads "Feedback on these answers". Opened, it holds a fieldset of checkboxes, "What do you think of these answers? Choose any that apply.", with the answer reasons in Logging, rule 4, an optional note of up to 1,000 characters, and one "Send feedback" button, so a tester can give several reasons and a note in one submission. Sending nothing (no reason and no note) is refused with "Choose at least one reason or write a note." beside the button. The form posts to `/a/{answer_id}/feedback` with `target=answer`. With JavaScript the status ("Thank you. Your feedback on these answers was recorded.") appears in a polite live region inside the control, the choices clear and focus stays on the button; without it the page reloads at the control, open, with the same status. The server still records `target=excerpt` and `target=summary` posts from pages served before. "Report this excerpt" stays a separate link in the footer row, because it is the rights route, not feedback.
7. A References list in MLA style (Spec 05, Answer assembly, rule 5), closed by default, with the texts' confidence badges in words and the word "unresolved" shown as is.
8. The dana panel. The donor enters a gift total and an optional site amount in dollars and presses "Show split". The htmx fragment lists each eligible sangha with its amount and a link to open that sangha's giving page, then the site line on its own row, then total outlay. It states that no money passes through this site and that each link opens the sangha's own page. With no eligible recipient it says so plainly. Without JavaScript the form posts and the page reloads with the split shown.
9. A "Flag this query" button that states, before and after pressing, that a flagged query can be read by the founder.
10. A "Return to the top" link closing the answer, which leads to the top of the page (`#top`, the `body`), so the next Tab reaches the skip link and the header's question field. Owner's decision of October 12, 2026 (Spec 00, post-beta item 16).

Rendering rules:

- Passage text appears only inside passage blockquotes and the summary's quoted lines, which the code slices from the cited excerpt, and is HTML-escaped by the template engine. Footnote markers are inserted from `TextRun` values, never by string concatenation.
- Text from a model appears only in the summary's sentences, outside every quotation.
- Only the transcript page shows a whole talk, and only for talks at display level `full` (decision of October 7, 2026). No passage is truncated mid-word.
- Durations and offsets render as `h:mm:ss`.
- Error pages are plain and friendly and never show stack traces or database errors.
- The layout is single column, mobile first, with visible focus outlines, semantic headings, and text contrast of at least WCAG AA. All controls work from the keyboard.
- A question shows its live progress while it runs (Live progress while a question runs, below), since a query takes 10 to 15 seconds.

## Live progress while a question runs

Owner's decision of October 12, 2026: moved before beta from Spec 00's post-beta candidates (item 3). The display reports what the query path does and changes nothing it computes or stores.

1. Every form that asks a question (the home box, the header field, the menu panel, the examples, "Search the talks anyway" and the acute notice's "Show teachings on facing difficult feelings") carries `data-progress`. The script `static/progress.js` takes the submit and posts the same form with `Accept: text/event-stream`. Without the script the form posts as before: the browser waits and `POST /ask` answers `303` to the permalink.
2. With that header, `POST /ask` and `POST /ask/curated` answer `200` with `text/event-stream` and `Cache-Control: no-store`, and run exactly the path of a plain post: the same checks, rate limit, triage gate and stored rows. The question runs to the end even if the reader leaves, as a plain post's would. The stream carries server-sent events:
   - `stage`, `{"stage": key}`, as each stage starts: `reading` (Reading the question: the sensitivity check, alias detection and the embedding), `searching` (Searching the talks: full-text, vector and question search and fusion), `weighing` (Weighing excerpts: the reranker), `choosing` (Choosing excerpts: selection, reading the passages and cutting their excerpts) and `writing` (Writing the summary), which is sent only when synthesis runs before the answer opens: never on the answer page since October 15, 2026, where the summary fills in afterwards.
   - `count`, `{"name": name, "value": n}`: `searched` (passages searched: the servable passages in the search's scope, the named teacher's own in named mode), `candidates` (the fused list read back), `shortlisted` (candidates at or above the relevance threshold) and `selected` (passages chosen). Each count but `searched` is sent before the stage that follows it starts; `searched` is sent when it is read, which is usually before the search ends.
   - One closing event: `done`, `{"url": "/a/<id>"}`, or `page`, `{"html": ...}`, which carries the whole page a plain post would have shown: a sensitive notice, the outside-these-talks page, the friendly `429`, the short-question message or an error page.
3. The passages-searched count is read through `servable_passage`, alongside the query path so it never delays it, and cached for five minutes per scope. It is the only query the display adds, and only a streamed ask makes it.
4. The script replaces the main content with a progress section: an `h1`, "Finding excerpts for your question", which takes focus; the question as asked; and an ordered list of the five stages, each with its state in words (Waiting, Now, Done, or Not needed for a summary that does not run) and its counts in words beside it ("14,203 passages searched", "120 candidates", "34 shortlisted", "8 selected"). The current stage carries `aria-current="step"`. Nothing moves: no spinner and no animation, with or without reduced motion. The copy lives in a `<template>` in the base template, not in the script.
5. A polite live region announces each stage once, as it starts, with the counts the stage before it produced, for example "Weighing excerpts. 14,203 passages searched, 120 candidates." On `done` it announces "Opening the answer" and the script loads the permalink. On `page` the script writes the returned page into the document. If the stream breaks, the section says so in words and offers "Try again", which posts the form without the script.

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
- Animation. Nothing flashes. The only motion is the 150-millisecond menu slide, and `prefers-reduced-motion` removes it. The sticky question appears and disappears without motion.
- Layout. Every page has a unique title, one `h1`, headings in order, and landmarks for header, navigation, main and footer. Navigation order and labels are identical on every page. Content reflows without two-dimensional scrolling at 320 CSS pixels and at 400 percent zoom, and orientation is never locked.
- Process and task completion. There are no time limits other than the 30-day session cookie, which is stated on About. The dana panel preselects nothing, uses no urgency wording, and explains before any link opens that no money passes through this site and that the link opens the sangha's own page.
- Help and feedback. About, Rights and removal and Accessibility are linked in the same order in the footer of every page. Feedback exists at every excerpt (the feedback control, with several reasons and a note, and Report this excerpt), on the summary (its own feedback control), on every query (Flag this query) and for accessibility itself.
- User control. The site respects `prefers-reduced-motion`, forced colors, user text spacing and user stylesheets. Changes that htmx makes without a page load are announced: the dana split and feedback status appear in a polite live region and focus moves to the new heading when the content is a result the user asked for. Links to source media say what they open, for example "Listen at 12:34 (opens the source video in a new tab)".

Testing has two parts. Every pull request runs the checks in Accessibility checks in CI, below. Every release also gets a manual pass: keyboard only, NVDA with Firefox on Windows, VoiceOver with Safari on iOS and macOS, TalkBack with Chrome on Android, 200 percent text, 400 percent zoom, forced colors, a text spacing override, reduced motion and no JavaScript. Results go in the register. The owner develops on Windows, so a tester with an iPhone or a Mac does the VoiceOver passes, and the register records who ran each pass.

### Accessibility checks in CI

Owner's decision of October 12, 2026: the automated checks of Spec 00's post-beta item 15 run before beta. They need a browser, and `cargo test` makes no network calls and runs no browser (rule 9), so they are their own CI job, `a11y`, required by `ci ok`:

1. The job starts a Postgres service, migrates it, and runs `cargo run -p wt-api --example a11y_fixture`, which seeds synthetic teachers, talks and passages, starts the app with the fake embedder and a fake language model, asks one question to make an answer with a verified theme, and writes the paths the browser visits to a JSON file. Nothing leaves the runner except the job's own package and browser downloads.
2. Playwright with Chromium and `@axe-core/playwright`, pinned in `tests/a11y/package-lock.json`, visits home, answer, talk card, transcript, rights, about and accessibility, and the progress section while a question runs, at 360, 768 and 1280 pixels wide. The text unit page joins the list when its route exists.
3. On every page and width: `axe-core` with the WCAG 2.0, 2.1 and 2.2 A and AA tags reports no violation, which covers contrast, link and control names, language and landmarks; every control and every link that is not inside a sentence has a target of at least 44 by 44 pixels (the site's rule, stricter than criterion 2.5.8, which exempts inline links); at 320 by 256 pixels, which is 1280 at 400 percent zoom, the page does not scroll sideways, and a wide table may scroll inside its own container; and every status and confidence badge (`.badge`) reads as one of its words (resolved, probable, unresolved, verified, unverified).
4. A failure prints each violation with its rule, page, width and element, and fails the job.

The HTML validator that the testing paragraph above calls for is not in the job yet. The manual passes are not automated and stay with the first cohort's screen-reader and keyboard-only testers.

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

`wt_web` may insert into it. `wt accessibility reports list|show|set` is the CLI for reading and handling reports.

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
8. The home page opens with its heading in the serif face, 2rem on phones and 2.75rem from 900px, then a numbered three-step guide headed How to use this site with a link to Why this exists on the About page (Spec 09), then the question box (a textarea of at least 3 rows, full width) with an Ask button, then the logging policy line and three example questions as plain links. No images.
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
| Answer | A result line with the teacher count and the text count. A notice banner when one applies. The summary when its headline survived. Passages in score order under their teachers' names. Under each talk passage: the footer row Listen, Read, Cite, Give to this sangha (when eligible) and Report, and the credit line. The sticky question once the heading scrolls away. A "From the texts" section where each unit shows its label, author, translator, edition, page, source link, credit line, "Read in the text" and "Report this excerpt", with no give link. References, closed by default. The dana panel. "Feedback on these answers". "Flag this query". |
| Text unit | The unit, up to two neighbors on each side, the credit line, the source link and the report link. Never a whole work |
| Talk card | Title, teacher, sangha, date, series, part and source link: the source's own transcript when the source made it, otherwise the recording and, at display level `full`, "Read our transcript" |
| Transcript | Only for talks whose transcript the project made (Spec 03, Display levels, rule 2). At the top: title, date, teacher, sangha, the credit line for the talk's basis, the source link, "Give to \<sangha>" when a verified donation link exists, "Report this transcript", and a line saying the text was transcribed by machine and may contain errors, or that it was reviewed and by whom. Then the teacher's words passage by passage, each with an anchor `p{ordinal}` and "Listen at h:mm:ss", with each student question before its teacher answer, as "Student:" and the lightly edited question with the line "Question lightly edited by machine; names and identifying details removed.", or "A student asked a question" where none is stored (owner's decision of October 10, 2026), and a marked gap wherever time passes between passages, since the page reads only servable passages and the spans live in the transcript file. A passage under 40 characters, left out at indexing, is missing here; the review file has every word. No unedited student words or student names, ever |
| Rights and removal | The plain promise, the status of every source in plain words, the removal form (name and email required since October 15, 2026, what to remove, a message, a hidden honeypot), and the statement that named material is suspended in the order requests arrive, with no promised time (Spec 11, PB1.7) |
| About | What the demo is, how excerpts are chosen, the logging policy, how credit lines work, how to leave, and a link to `/rights` |
| Error pages | Plain friendly text with no stack trace or database message |

The credit line follows the rights basis and never claims more than exists:

- Permission: "Used with permission of \<name>." For talks, \<name> is the teacher who consented, or the sangha whose written yes covers the talk's source (owner's decision of October 14, 2026).
- License: the license name with a link to its text.
- Mandate, texts: the attribution line stored with the mandate, such as "Excerpted with attribution from \<title>, \<publisher>. Free to download at the source: \<url>."
- Mandate, talks: "Talk by \<teacher>, \<sangha>." with the source link.

The words permission and licensed never appear on a mandated excerpt.

## Logging, feedback and log access

The PRD's policy is that query logs are disclosed to testers, read in aggregate, and opened individually only when a tester flags a query. The design makes that mechanically true.

1. Each ask inserts one `query_log` row, one `answer` row and the `answer_passage` rows in a single transaction. If that transaction fails, the request fails with a friendly error and shows no answer. The system fails closed so no answer is ever shown without being logged as disclosed.
2. Stage timings (embed, search, rerank, select, synthesize, verify, assemble) are written to the structured log without query text. Only the total goes in `query_log.latency_ms`.
3. The three give routes work like this. `POST /a/{id}/dana` writes a `dana_event` of kind `split_computed` with the entered total. Opening the panel writes `panel_open`. `GET /give/...` writes `link_click` with the sangha id (null for the site line), then redirects. The redirect target comes from the registry row, never from a request parameter, and an unlisted or unverified sangha returns `404`.
4. Feedback (owner's decision of October 12, 2026, replacing the single-button kinds `good`, `not_faithful` and `not_relevant`). Each `feedback` row has a `target`: `answer` (the page's one control since October 15, 2026), `excerpt` (with `passage_id`), `summary` or `query`. One submission writes one row per chosen reason and, when a note is given, one `note` row holding the note, all sharing a `submission_id`. Reasons, with the label the page shows:
   - Answer: `good` "Helpful", `not_relevant` "Not relevant to my question", `unsupported` "The summary is not supported by the excerpts", `out_of_context` "An excerpt is cut off or missing its context", `not_faithful` "Words look wrong (a transcription error)", `wrong_details` "Wrong teacher, talk or date", `unclear` "Hard to follow". Migration `0042_feedback_on_the_answer.sql` adds it to the check constraint.
   - Excerpt (pages served before October 15, 2026): `good` "Helpful", `not_relevant` "Not relevant to my question", `out_of_context` "Cut off or missing its context", `not_faithful` "Words look wrong (a transcription error)", `wrong_details` "Wrong teacher, talk or date", `unclear` "Hard to follow".
   - Summary (pages served before October 15, 2026): `good` "Helpful", `unsupported` "Not supported by the excerpts", `misrepresents` "Misrepresents a teacher", `missing` "Missing something important", `unclear` "Unclear".
   - Query: `flag_query` "Flag this query". It also calls the database function `flag_query(id)`, which sets `query_log.flagged` and `flagged_at` on that row. It is the only write `wt_web` can make to `query_log` after the insert.

   A check constraint ties each kind to its target, and the server refuses a reason that belongs to the other target, a passage that is not in the answer, and a note over 1,000 characters. A note has the same personal details removed as a kept question (Privacy modes, rule 1) and is deleted with its answer. Earlier rows, which kept the passage id in `note`, move to `passage_id` in migration `0039_feedback_targets.sql`.
5. Disclosure copy appears under the search box and on `/about`: "Your questions are logged. They are read only in aggregate, and one is opened only if you flag it."
6. The reporting commands run as `wt_analyst`: `wt report answers --since <duration>` (from `v_answer_stats`), `wt report dana`, `wt report coverage` (from `v_consent_coverage`), `wt report flagged` (through `flagged_queries()`) and `wt report feedback` (from `v_feedback_counts`, and with `--notes` through `feedback_notes()`) and `wt report beta --since <duration>` (Spec 07, the weekly beta report). None of them can read unflagged query text.
7. The owner role can read every table, so reading raw queries with it is outside the stated policy. The CLI offers no command that does so, and the runbook says the role is for migrations and break-glass repair only.
8. `wt tester delete --label <text>` removes that tester and every query log, answer, feedback and dana event row for them, for a tester who asks to be forgotten. Query logs otherwise stay until the test ends.

## Privacy modes for questions

The project needs to see questions to improve, and testers deserve control over them. Every tester chooses between two modes, and this section replaces the disclosure line and the retention rule in the logging list above.

Modes:

1. Keep (`share`). The question text is saved with obvious personal details removed (email addresses, phone numbers, web addresses and any run of nine or more digits), and the text expires after `WT_QUERY_RETENTION_DAYS` (default 180).
2. Count only (`count_only`). The text is kept for `WT_COUNT_ONLY_TTL_HOURS` (default 24) so the tester can return to the answer, then deleted. Only counts remain: mode, teacher count, sangha count, passage count, theme counts, thin coverage, latency and dana events.

Choice and defaults:

- A switch labeled "Keep my questions to help improve Wheel Turning" sits on the home page and in About. It saves to `tester.privacy_mode` and applies to the next question. The default is `share` while the demo is private, with disclosure on the home page and in the invitation. `WT_PRIVACY_DEFAULT` sets the default and is `count_only` before any public launch, with the switch worded as an invitation to help.
- A request that carries `Sec-GPC: 1` from a tester who has never chosen is treated as `count_only`.
- Flagging a question is a deliberate act. A flagged question keeps its text for the full retention period even in count-only mode.
- A question the triage marks `medical` or `distress` is stored count-only, whatever the tester's mode, so its text is deleted after `WT_COUNT_ONLY_TTL_HOURS` (Spec 11, PB3.3). Acute and out-of-scope questions already store counts only.
- The privacy line has one source, `copy/privacy_line.md`. The footer, the home page and the invitation that `wt tester invite` prints all read it, and a test fails if they differ (Spec 11, PB3.1).

Expiry and deletion:

- A nightly job `purge_expired_queries` sets `query_text` to null for expired rows that are not flagged, and deletes their `answer` rows with the passage ids, themes and feedback attached to them. The counts stay. A permalink for a purged answer shows "This question was not kept."
- `dana_event` rows survive a purge with a null `answer_id`, so link-click counts, which the v1 tests depend on, are not lost.
- `POST /about/delete-my-data` deletes every question, answer, feedback row and dana event that belongs to the tester, after a confirmation. `POST /about/privacy` saves the mode. Both need a session and a CSRF token.

What the owner can learn without reading a question:

- `wt report topics` embeds the stored question texts with the local model, clusters them, and prints each cluster with a count and a label made from the most central question's keywords. A cluster with fewer than 3 questions is folded into "other", so no small group of people can be picked out.
- `wt report gaps` shows clusters with high shares of thin coverage or no excerpt above the relevance threshold. These are the topics the collection cannot answer yet and guide what to add next.
- `wt report feedback --since <duration>` shows submissions and reason counts per day by target (excerpt or summary), reason and mode, from `v_feedback_counts`, and by cluster once `wt report topics` exists. `--notes` adds each note with its target, reasons and date through `feedback_notes()`, never with the question, which stays readable only when flagged. The feedback form says so under the note field: "Notes are read by the project. Your question is not shown with them unless you flag it."
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

- [x] `/healthz` returns success only when the database answers. (`wt-api/tests/healthz.rs`.)
- [x] A valid invite token sets a correctly flagged cookie and redirects. An invalid or revoked token returns the same generic `404`. The token never appears in logs. (`wt-api/tests/pages.rs`; `/join` logs nothing itself, and since October 20, 2026 the request logger names the route by its template, `/join/{token}`, checked in `wt-api/tests/log_scrubber.rs`.)
- [x] A state-changing request without a valid CSRF token is rejected. (`wt-api/tests/pages.rs`.)
- [x] `POST /ask` persists an answer, responds `303` to the permalink, and the permalink is readable only by the tester who asked. (`wt-api/tests/pages.rs`.)
- [ ] Snapshot tests with `insta` cover the answer page for standard, named, thin coverage, none found, and multi-named cases. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] A passage whose text contains `<script>` renders escaped, and no model text appears outside the synthesis section. (`wt-api/tests/pages.rs`.)
- [x] `/give` logs a click and redirects only to the registry URL. A tampered `amount_cents`, an unverified sangha and an unlisted sangha are each rejected. (`wt-api/tests/pages.rs`; `amount_cents` must be whole cents within the ceiling and only fills a verified page's template, and `/give/<answer>/site` goes to `WT_SITE_DANA_URL`.)
- [x] The dana fragment equals the output of the `wt-dana` function for the same inputs, and the no-JavaScript path shows the same split. (`wt-api/tests/pages.rs`. Until htmx is vendored, `static/dana.js`, a short script of our own, posts the form with `HX-Request: true` and swaps the fragment into the live region, so htmx can replace it with no server change. `panel_open` is logged on the first focus inside the panel, or by the first split without the script.)
- [x] `flag_query` sets the flag, and the flagged query is readable only through `flagged_queries()`. (`the_web_role_reads_only_the_serving_views_and_flags_through_flag_query` (`wt-db/tests/web_role.rs`) and `feedback_and_flagging_are_recorded` (`wt-api/tests/pages.rs`).)
- [x] `wt report` commands run as `wt_analyst` and cannot read unflagged query text. (`wt-cli/tests/commands.rs`: each report runs as a `wt_analyst` login and prints no unflagged question, and `wt report` refuses a connection that can read `query_log`.)
- [ ] A failed log insert produces an error page and no answer. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] The rate limit returns `429` after `ask_rate_per_hour` asks in an hour. (`wt-api/tests/site.rs`; `ask_rate_per_hour` is in `[answer]` of `config/retrieval.toml`.)
- [x] Security headers and `robots` rules are present on every response. (`wt-api/tests/pages.rs` and `tests/site.rs`; the headers are set by one layer over the whole router.)
- [ ] A manual keyboard-only pass completes ask, feedback, dana split and flag with no mouse. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)

Acceptance criteria for texts, rights and design:

- [ ] Every route in the table exists. A route not marked None returns a generic `404` without a session, and `/rights` and `/robots.txt` work with none. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [ ] The answer page shows the "From the texts" section with a credit line, source link and report link on each unit and no give link. Text hits are absent from the teacher count and the dana split. (Not built: no `From the texts` section exists on the answer page (B1, B11).)
- [ ] `GET /text/{unit_id}` shows at most two neighbors on each side and returns `404` for a unit that is not in `servable_text_unit`. (Not built: there is no `GET /text/{unit_id}` route (B11).)
- [ ] Snapshot tests cover the credit line for permission, license and mandate, for talks and texts, and assert that permission and licensed are absent on a mandate. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [ ] "Report this excerpt" opens `/rights?excerpt=<id>` with the excerpt prefilled for both talk passages and text units, and the form stores a request. (Talk passages and the form: `wt-api/tests/site.rs` and `tests/pages.rs`. Text units wait for the answer page's "From the texts" section; the form already records a reported unit's id.) (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] `GET /rights` lists each source from `v_source_rights` with its plain-language status (used on the project's own decision with permission requested on a date, used with permission recorded on a date, or removed at the rights holder's request) and uses no permission language for a mandate. (Moved from Spec 08, decision of October 5, 2026. `wt-api/tests/site.rs`; a source switched off for any other reason reads "Not shown at present.", and a mandate with no request sent yet reads "Permission not yet requested.")
- [ ] A rendered mandated talk passage reads "Talk by" the teacher and sangha with the source link and the report link, and the words permission and licensed are absent; after a grant, a passage or text unit reads "Used with permission of" the grantor. (Moved from Spec 03, Mandated talks, and Spec 08, Rights lifecycle, decision of October 5, 2026.) (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] At 360px there is no horizontal scroll and the Menu button shows a text label. The panel opens from the button, closes from the Close button, the backdrop and Esc, and returns focus to the button. From 900px the links show inline with no Menu button. (`tests/a11y/a11y.spec.js` in the `a11y` CI job; reflow is checked at 320px, narrower than 360.)
- [x] The "Skip to main content" link is the first tab stop, and a test asserts that the viewport tag contains neither `user-scalable=no` nor `maximum-scale`. (`wt-api/tests/site.rs` and `tests/static_checks.rs`.)
- [x] Header links, buttons and feedback buttons have targets of at least 44px. (The `a11y` CI job, `tests/a11y/a11y.spec.js`, checks every control and every link not inside a sentence on every page at 360, 768 and 1280 pixels.)
- [x] The excerpt footer row reads Listen, Read, Cite, Give, Report in that order, every item is at least 44px, it wraps with no horizontal scroll at 320, 360 and 375px, and Cite opens the MLA entry without JavaScript. (`wt-api/tests/pages.rs` snapshot of the row; measured in Chrome on October 12, 2026 at 320, 360, 375 and 1280px with Cite and feedback open: no sideways scroll, every visible target 44px or more.)
- [x] The sticky question appears only after the `h1` scrolls away, is hidden from assistive technology, does not animate, and a focused control or an anchor target is never beneath it. (Checked in headless Chrome on October 12, 2026 at 375px: hidden at the top, shown after scrolling, scroll padding 137px against a header and row ending at 121px, so `#p2` lands below them; `aria-hidden` asserted in `wt-api/tests/pages.rs`.)
- [x] Excerpt and summary feedback accept several reasons and a note in one submission, refuse an empty submission, a reason from the other target and a passage outside the answer, and write one row per reason plus a `note` row under one `submission_id`; migration `0039` moves earlier passage ids out of `note`. (`wt-api/tests/pages.rs` against Postgres; `wt-db/tests/migrations.rs` for the backfill.)
- [ ] With JavaScript the feedback status appears in a polite live region in the control; without it the page reloads at the excerpt with the control open and the status shown. (The no-script reload and the script's text reply are tested in `wt-api/tests/pages.rs`; the script itself waits for the release's manual screen-reader pass.) (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] `wt report feedback` prints counts from `v_feedback_counts` as `wt_analyst`, and `--notes` prints notes without question text. (`wt-cli/tests/commands.rs`.)
- [ ] An automated accessibility run passes WCAG AA on home, answer, text unit, rights and about at 360px and at 1280px. (The `a11y` CI job passes on home, answer, talk card, transcript, rights, about, accessibility, the progress section and the acute notice at 360, 768 and 1280 pixels; the text unit route does not exist yet.) (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] Every page works without JavaScript: the menu opens through `<details>`, a question posts, and the dana split uses its fallback. (`tests/a11y/a11y.spec.js`, in a browser with JavaScript off.)
- [x] No template or stylesheet references an external host or font, checked by a grep test. (`wt-api/tests/static_checks.rs`.)
- [x] The footer appears on every page, and its last line contains no copyright claim over excerpts. (`wt-api/tests/site.rs`.)
- [x] With `prefers-reduced-motion`, the menu does not animate. (`wt-api/tests/static_checks.rs` checks the stylesheet rule.)

Acceptance criteria for the accessibility standard and the logo:

- [x] `docs/accessibility/wcag3-register.md` exists, names the draft of September 10, 2026 or a later published one, lists every applicable requirement with status and evidence, and the release gate passes only when its draft version matches the latest published one or records why not. (`wt-api` unit tests check the lines and the date; checking W3C for a newer draft is a manual release step written into the register, since tests make no network calls.)
- [x] A test computes the WCAG 2.2 ratio for every pair in the measured contrast table from the stylesheet tokens and fails below the thresholds, and a test fails if bright gold appears on a light surface. (`wt-api/tests/static_checks.rs`.)
- [x] Where WCAG 3.0 and WCAG 2.2 differ, the stricter outcome is applied and the difference is recorded in the register. (Each row whose value the draft leaves open names the WCAG 2.2 criterion applied.)
- [x] `GET /accessibility` and `POST /accessibility/report` work with no session, the report form stores a row, and the statement lists exactly the statuses in the register. (`wt-api/tests/site.rs`; the statement is built from the register at compile time.)
- [ ] Every page has one `h1`, a unique title, the header, navigation, main and footer landmarks, and `lang="en"`. (Done for home, About, Rights, Accessibility and the error page in `wt-api/tests/site.rs`; Pali, Sanskrit and Japanese `lang` attributes are not yet set.) Pali, Sanskrit and Japanese terms carry a `lang` attribute where the source gives one, and diacritics are preserved in rendered text. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [ ] Status and confidence badges carry words, and a grayscale screenshot test shows no information lost. (References show a text's confidence in words, as "(citation resolved)", and the `a11y` job checks that any `.badge` reads as one of its words; the grayscale screenshot test is not written.) (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [ ] At 320 CSS pixels and at 400 percent zoom, no page needs two-dimensional scrolling. With 200 percent text and a text spacing override, no content or function is lost. (The first sentence is checked by the `a11y` job at 320 by 256 pixels on every page; the text size and spacing checks are not automated yet.) (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [ ] Dynamic updates are announced: the dana split and feedback status appear in a polite live region, and focus moves to the result heading when the user asked for the result. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] The dana panel preselects nothing and states, before any link opens, that no money passes through the site. (The amount fields start empty, and Spec 09's introduction sits above the form.)
- [ ] Source links name what they open, for example "Listen at 12:34 (opens the source video in a new tab)". (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [ ] The manual pass for the release is recorded: keyboard only, NVDA with Firefox, VoiceOver on iOS and macOS, TalkBack with Chrome, forced colors, reduced motion and no JavaScript. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)
- [x] The header mark is inline, hidden from assistive technology, named by the adjacent text, stays visible in forced colors mode, and the footer mark follows `--on-dark`. (`wt-api/tests/site.rs` for the markup; the mark draws in `currentColor`, which forced colors map to the text color. The source files are `docs/brand/`, since `brand/source/` and `Logo.zip` are not in the repository.)
- [x] `GET /favicon.ico` works with no session, the head carries the SVG, ICO and 180px icon links, and the served SVG files carry no `<metadata>` element while the originals in `brand/source/` are unchanged. (`icons_and_the_menu_script_need_no_session` (`wt-api/tests/site.rs`) and `the_served_mark_has_no_metadata_and_the_source_keeps_it` (`wt-api/src/chrome.rs`).)
- [ ] The first tester cohort includes at least two people who use a screen reader and one who navigates by keyboard only, and each of their findings is fixed or recorded in the register before any wider launch. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)

Acceptance criteria for live progress and the return link (owner's decision of October 12, 2026):

- [x] A streamed ask sends `reading`, `searching`, `weighing`, `choosing` and, when synthesis runs, `writing`, with the candidates, shortlisted and selected counts before the stage each one leads into and the passages-searched count when read, and closes with `done` naming the permalink. (`wt-api/tests/progress.rs`.)
- [x] A streamed ask stores exactly what a plain post stores, and one that is refused or fails validation closes with `page` carrying the plain post's page and stores nothing. (`wt-api/tests/progress.rs`.)
- [x] Without the script the form posts and redirects as before. (`wt-api/tests/pages.rs`, and `tests/a11y/a11y.spec.js` with JavaScript off.)
- [x] The progress section's heading takes focus, each stage is announced once in the polite live region with its counts, states are in words, nothing animates, and axe and the target check pass on it at 360 and 1280 pixels. (`tests/a11y/a11y.spec.js`.)
- [x] The answer page closes with "Return to the top", which leads to the top of the page, and the next Tab reaches the skip link. (`wt-api/tests/progress.rs` and `tests/a11y/a11y.spec.js`.)

Acceptance criteria for question privacy:

- [x] The switch appears on the home page and in About, saves to `tester.privacy_mode`, and applies to the next question. (`wt-api/tests/privacy.rs`.)
- [x] In `share` mode the stored text has emails, phone numbers, web addresses and runs of nine or more digits removed, and a test covers each pattern. (`wt-api` unit tests in `privacy.rs` and `tests/privacy.rs`.)
- [x] In `count_only` mode the text is deleted after the TTL and the answer row goes with it, the counts stay, and the permalink shows "This question was not kept." (`wt-api/tests/privacy.rs`. The purge runs hourly inside `wt-api`, through `purge_expired_queries()`.)
- [x] A flagged question survives the purge in both modes. (`wt-api/tests/privacy.rs`; its text goes once the full retention period has passed.)
- [x] A request with `Sec-GPC: 1` from a tester who has never chosen is treated as `count_only`, and an explicit choice overrides it. (`wt-api/tests/privacy.rs`.)
- [x] A purge leaves `dana_event` rows with a null `answer_id`, and the daily click count does not change. (`wt-api/tests/privacy.rs`.)
- [x] `POST /about/delete-my-data` removes every row that belongs to the tester after a confirmation, and does not touch another tester's rows. (`wt-api/tests/privacy.rs`. A dana event already detached from a purged answer can no longer be traced to its tester and stays as a count.)
- [ ] `wt report topics` folds any cluster of fewer than 3 questions into "other" and prints no individual question text, and `wt report gaps` lists only cluster-level counts. (Known gap, B18, October 23, 2026: no test or check for this item was found by name.)

## Rendering sensitive questions

The `out_of_scope` page and the `unrelated` page (Spec 05, Sensitive questions, as amended on October 12, 2026) follow the same rules as the acute page below where they apply: a heading that takes focus, no giving panel, no feedback controls, `Cache-Control: no-store`, and no permalink until the tester presses "Search the talks anyway".

- The notice is a labeled region with a heading. On the acute page it is the `h1` and takes focus on load. A banner sits inside the main landmark above the question, is never collapsible or dismissed by a timer, and uses the body text size.
- `POST /ask` for an acute question responds `200` with the notice page and `Cache-Control: no-store`, not a `303` to a permalink, because no answer exists. The page has no giving panel, no feedback buttons, no "Flag this query" and no permalink.
- The numbers are links, so "call 988" is a `tel:988` link, "text 988" is an `sms:988` link, and the web addresses are plain links with names that say where they lead.
- The notices make no claim about confidentiality or outcomes.
- Nothing on these pages animates, counts down or asks for anything.

Acceptance criteria:

- [x] The acute page has the notice as its `h1`, focus lands on it, and axe passes with no giving panel or feedback controls present. (`tests/a11y/a11y.spec.js`, reached through the progress stream.)
- [x] The response carries `Cache-Control: no-store`, and no permalink or `answer` row is created.
- [x] Banners render inside the main landmark above the question and cannot be dismissed.
- [x] The `tel:` and `sms:` links and the two web links exist and are named in the page text.
