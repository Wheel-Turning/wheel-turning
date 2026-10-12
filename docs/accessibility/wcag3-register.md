# WCAG 3.0 register

Spec 06, Accessibility standard, rule 2. One row per requirement of the WCAG 3.0 Working Draft that applies to this site. `GET /accessibility` is generated from this file at build time, so the statement never claims more than the register shows. Edit it by hand when W3C publishes a new draft, and after every evaluation.

Draft: 2026-09-10
Draft URL: https://www.w3.org/TR/2026/WD-wcag-3.0-20260910/
Last evaluated: 2026-10-11
Latest published draft checked on: 2026-10-11

## How to read and keep this register

- **Type** is the draft's tag: core, supplemental or assertion. Every requirement in this draft is at developing status.
- **Status** is one of: met, partly met, not met, not applicable. The statement page lists exactly these.
- Where the draft leaves a value or method to be determined, the WCAG 2.2 AA criterion named in the row applies (Spec 06, rule 3), and where the two differ the stricter outcome is applied.
- **Release gate.** Before each release, open the draft URL's latest version (`https://www.w3.org/TR/wcag-3.0/`). If its date differs from the `Draft:` line above, update this register, or add a line under "Draft check notes" saying why not. Tests make no network calls, so this check is a manual release step; `wt-api/tests/register.rs` checks only that the lines exist and that the draft is no older than September 10, 2026.
- **Automated browser tests.** Spec 06 asks for axe-core, an HTML validator and Playwright runs at 360, 768 and 1280 pixels on every pull request. CI has no browser yet, so these are not run; rows that depend on them say "partly met". The contrast, viewport, external-host, landmark, title and heading checks run in `cargo test` without a browser.
- **Manual passes.** Keyboard only, NVDA with Firefox, VoiceOver on iOS and macOS, TalkBack with Chrome, 200 percent text, 400 percent zoom, forced colors, text spacing override, reduced motion and no JavaScript. None has been run and recorded yet. Record each pass below with who ran it and when.

## Draft check notes

- 2026-10-11: register created against the draft of September 10, 2026, the latest published on that day.

## Manual passes

| Pass | Who | Date | Result |
| --- | --- | --- | --- |
| Keyboard only | not yet run | | |
| NVDA with Firefox | not yet run | | |
| VoiceOver, iOS | not yet run | | |
| VoiceOver, macOS | not yet run | | |
| TalkBack with Chrome | not yet run | | |
| 200 percent text, 400 percent zoom, text spacing | not yet run | | |
| Forced colors, reduced motion, no JavaScript | not yet run | | |

## Requirements

| ID | Requirement | Type | Status | How it is met | Test method | Evidence | Checked |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 2.1.1 | Images detectable | core | met | The only image is the decorative wheel mark, inline SVG | Template review | `templates/base.html` | 2026-10-11 |
| 2.1.1 | Decorative images hidden | core | met | The mark carries `aria-hidden="true"` and `focusable="false"`; the link is named by its text | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.1.1 | Image alternatives available | core | not applicable | No informative images | Template review | | 2026-10-11 |
| 2.1.1 | Image alternatives equivalent | core | not applicable | No informative images | Template review | | 2026-10-11 |
| 2.1.3 | Non-text content not relied on | core | met | Status and confidence are words, never color or icon alone | Template review | `templates/answer.html` | 2026-10-11 |
| 2.1.4 | Transcripts findable | core | met | Each talk at display level full links "Read the transcript"; the site plays no media itself | Automated test | `tests/pages.rs` | 2026-10-11 |
| 2.1.4 | Speakers identified in transcripts | supplemental | met | The teacher is named at the top; student questions are labeled "Student" | Template review | `templates/transcript.html` | 2026-10-11 |
| 2.1.8 | Hue not relied on | core | met | No meaning carried by color; links are underlined | Template and stylesheet review | `static/app.css` | 2026-10-11 |
| 2.1.8 | Graphical object contrast sufficient | core | met | WCAG 2.2 1.4.11: gold-deep on paper 3.5:1, gold on dark 8.2:1, control borders 4.7:1; bright gold never on a light surface | Automated test | `tests/site.rs` contrast tests | 2026-10-11 |
| 2.1.8 | Sound not relied on | core | not applicable | The site plays no audio | Review | | 2026-10-11 |
| 2.2.1 | Text contrast sufficient (minimum) | core | met | WCAG 2.2 1.4.3 (value undecided in the draft): every text pair at least 4.5:1, computed from the stylesheet tokens | Automated test | `tests/site.rs` contrast tests | 2026-10-11 |
| 2.2.1 | Blocks of text readable (minimum) | core | partly met | WCAG 2.2 1.4.12 default: line height 1.6 body and 1.65 passages, reading column at most 44rem. Text spacing override not yet tested by hand | Stylesheet review; manual pass pending | `static/app.css` | 2026-10-11 |
| 2.2.1 | Text style readable (minimum) | core | met | System fonts, sizes in rem, body 1.0625rem; no text in images | Stylesheet review | `static/app.css` | 2026-10-11 |
| 2.2.1 | Blocks of text adjustable | core | partly met | WCAG 2.2 1.4.12: no fixed heights on text containers; manual text spacing pass pending | Manual pass pending | | 2026-10-11 |
| 2.2.1 | Text style adjustable | core | met | User stylesheets and browser font settings apply; no `!important` on type | Stylesheet review | `static/app.css` | 2026-10-11 |
| 2.2.1 | Text size adjustable | core | partly met | WCAG 2.2 1.4.4: all sizes in rem; the 200 percent pass is pending | Manual pass pending | | 2026-10-11 |
| 2.2.1 | Text color adjustable | core | met | Colors are custom properties; forced colors are respected | Stylesheet review | `static/app.css` | 2026-10-11 |
| 2.2.2 | Text detectable | core | met | All text is live HTML text | Template review | | 2026-10-11 |
| 2.2.2 | Human language detectable | core | partly met | `lang="en"` on every page; Pali, Sanskrit and Japanese terms do not yet carry their own `lang` | Automated test for `lang="en"` | `tests/site.rs` | 2026-10-11 |
| 2.2.3 | Abbreviations explained | core | met | Dana and sangha are defined on About at first use | Copy review | `templates/about.html` | 2026-10-11 |
| 2.2.3 | Summaries available | core | met | About and Rights and removal open with a short summary | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.2.3 | Diacritics available | core | met | Names and terms are rendered exactly as stored | Template review | | 2026-10-11 |
| 2.2.3 | Clear language review | assertion | not met | A clear language review of every page is required before release and has not been done | Manual review pending | | 2026-10-11 |
| 2.3.1 | Focus indicator contrast sufficient | core | met | 3px ink outline with 2px offset on light surfaces (15.5:1), gold on the dark footer (8.2:1); never removed | Automated test | `tests/site.rs` contrast tests | 2026-10-11 |
| 2.3.1 | Focus indicator style guide | assertion | met | Spec 06, Design language, is the style guide for focus | Document | `docs/specs/06-api-and-web.md` | 2026-10-11 |
| 2.3.3 | Focus order meaningful | core | partly met | The skip link is first; source order follows visual order. Keyboard pass pending | Automated test for the skip link; manual pass pending | `tests/site.rs` | 2026-10-11 |
| 2.3.4 | Consistent interactions | assertion | met | One base template gives every page the same header, menu and footer | Template review | `templates/base.html` | 2026-10-11 |
| 2.3.5 | Interactive element contrast sufficient | core | met | Control borders 4.7:1 against paper; buttons are ink fill | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.3.5 | Interactive element names available | core | met | Native controls with visible labels; the home link is named "Wheel Turning, home" | Template review | | 2026-10-11 |
| 2.3.5 | Label included in programmatic name | core | met | Accessible names contain the visible label | Template review | | 2026-10-11 |
| 2.3.5 | Roles, values, states, properties available | core | met | Native elements only; the menu is a `<details>` element; the current page carries `aria-current` | Template review | | 2026-10-11 |
| 2.3.5 | Changes to elements notified | core | partly met | Feedback and form results use `role="status"`; htmx-driven updates are not built yet | Template review | | 2026-10-11 |
| 2.3.5 | Input constraints used | core | met | Length limits are stated in text and set on the fields | Template review | | 2026-10-11 |
| 2.4.1 | Keyboard operable | core | partly met | Native controls throughout; keyboard-only pass pending | Manual pass pending | | 2026-10-11 |
| 2.4.1 | No keyboard traps | core | partly met | No custom focus handling except the menu, which closes with Esc and returns focus; keyboard pass pending | Manual pass pending | `static/menu.js` | 2026-10-11 |
| 2.4.1 | Focus user-controlled | core | met | Focus moves only when the user acts | Template review | | 2026-10-11 |
| 2.4.3 | Pointer activation controllable | core | met | Native buttons and links activate on release | Review | | 2026-10-11 |
| 2.4.3 | Pointer pressure not relied on | core | met | No pressure input | Review | | 2026-10-11 |
| 2.4.3 | Pointer speed not relied on | core | met | No speed-dependent input | Review | | 2026-10-11 |
| 2.4.4 | Speech not relied on | core | met | No speech input | Review | | 2026-10-11 |
| 2.4.5 | Hover or focus content dismissible | core | met | No content appears on hover or focus | Review | | 2026-10-11 |
| 2.4.5 | Path-based gesture not relied on | core | met | No gestures | Review | | 2026-10-11 |
| 2.4.5 | Pointer accessible | core | partly met | WCAG 2.2 2.5.8 and the site's 44px rule: buttons, header links and feedback buttons are at least 44px tall; inline text links are exempt. Browser measurement pending | Stylesheet review; Playwright pending | `static/app.css` | 2026-10-11 |
| 2.4.6 | Biometrics not relied on | core | met | Access is an invite link and a cookie | Review | | 2026-10-11 |
| 2.5.1 | Error notifications available | core | met | Errors appear in text next to the field | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.5.1 | Errors associated | core | met | The error is tied to its field with `aria-describedby` and `aria-invalid` | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.5.1 | Error suggestions provided | supplemental | met | Each message says how to fix the problem | Copy review | | 2026-10-11 |
| 2.5.2 | Errors preventable | core | met | Nothing is irreversible without a confirmation step | Review | | 2026-10-11 |
| 2.5.2 | Submission status notified | supplemental | met | Sent forms show a status message; the rate limit page says when to try again | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.6.1 | No flashing over threshold | core | met | Nothing flashes | Review | | 2026-10-11 |
| 2.6.1 | No visual motion | core | met | The only motion is the 150 ms menu slide, removed under `prefers-reduced-motion` | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.7.2 | Page/view title available | core | met | Every page has a unique title | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.7.3 | Sections labeled | core | met | Sections open with headings; landmarks for header, navigation, main and footer | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.7.3 | Heading structure available | supplemental | met | One `h1` per page, headings in order | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.7.3 | Order detectable | core | met | Single-column source order matches reading order | Template review | | 2026-10-11 |
| 2.7.4 | Overlay content dismissible | core | met | The menu panel closes from its button, Close, the backdrop and Esc | Script review; browser test pending | `static/menu.js` | 2026-10-11 |
| 2.8.1 | Consistent navigation order | supplemental | met | One header and footer template on every page | Template review | `templates/base.html` | 2026-10-11 |
| 2.8.1 | Consistent navigation labels | supplemental | met | Ask, About, Rights and removal on every page | Template review | `templates/base.html` | 2026-10-11 |
| 2.9.1 | Cognitive test alternatives available | core | met | No CAPTCHA; the public forms use a hidden honeypot and a rate limit | Review | `src/rights.rs` | 2026-10-11 |
| 2.9.2 | No time limits | supplemental | met | The only limit is the 30-day session cookie, stated on About | Copy review | `templates/about.html` | 2026-10-11 |
| 2.9.3 | Preselections visible | core | met | No control is preselected | Template review | | 2026-10-11 |
| 2.10.1 | Consequences of choices explained | core | met | Flagging says before and after that the founder can read the query | Template review | `templates/answer.html` | 2026-10-11 |
| 2.11.1 | Sensory characteristics not relied on | core | met | Instructions never refer to shape, color or position alone | Copy review | | 2026-10-11 |
| 2.11.1 | Consistent help available | supplemental | met | About, Rights and removal and Accessibility are linked in the footer of every page | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.11.2 | Feedback mechanism available | supplemental | met | Good, Not faithful, Not relevant, Report this excerpt, Flag this query, and the accessibility report form | Automated test | `tests/site.rs` | 2026-10-11 |
| 2.12.1 | Assistive technology supported | core | not met | No screen reader pass has been run yet; the first tester cohort includes two screen reader users and one keyboard-only user | Manual pass pending | | 2026-10-11 |
| 2.12.1 | User settings supported | core | met | Reduced motion, forced colors, user text spacing and user stylesheets are respected | Stylesheet review | `static/app.css` | 2026-10-11 |
| 2.12.1 | Virtual cursor supported | core | partly met | Semantic HTML only; screen reader pass pending | Manual pass pending | | 2026-10-11 |
| 2.12.3 | Orientation supported (minimum) | core | met | Orientation is never locked | Review | | 2026-10-11 |
| 2.12.3 | Layout reflow supported | supplemental | partly met | WCAG 2.2 1.4.10: one column at every width and the viewport never blocks zoom; a 320px browser check is pending | Automated viewport test; Playwright pending | `tests/site.rs` | 2026-10-11 |
| 2.12.5 | Change of content notified | core | partly met | Status messages use `role="status"`; htmx updates are not built yet | Template review | | 2026-10-11 |
| 2.12.5 | Change of focus notified | core | met | Focus moves only on user action | Review | | 2026-10-11 |
| 2.12.5 | Change of user agent notified | core | met | Source links say they open in a new tab | Automated test | `tests/pages.rs` | 2026-10-11 |
