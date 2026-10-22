# Spec 09: Copy and templates

This tab holds the words the product and its letters use. Page copy goes in `copy/` as one markdown file per page and is rendered into the templates. Letters go in `templates/`. Text in \[square brackets\] is filled in by a person or by the code. Every page keeps sentences short, defines a term the first time it appears, and avoids idiom, in line with the accessibility standard in Spec 06. The project is run by an individual for now, so the copy says so plainly and the owner updates it when a nonprofit exists.

## Home page

File: `copy/home.md`.

- Heading: Find what Zen teachers have said about your question
- Guide, under the heading How to use this site, as a numbered list: 1. Type a question about your practice or your life, in your own words. 2. Press Ask. 3. Read short parts of talks, called excerpts. Each one shows the teacher's name and where it comes from. Then the link: Why this site exists (to the About section Why this exists). The guide replaced the tagline on October 17, 2026, after the first private-beta testers, among them an older reader and a reader whose first language is not English, said they could not tell what the site does or how to use it. The tagline "What have Zen teachers said about the question you are living with?" stays on the holding site and the link preview image.
- Question label: Your question
- Help text under the box: Write it the way you would say it. For example: I keep judging myself during zazen.
- Button: Ask
- Line under the button: An answer takes a few seconds.
- Privacy line, in `copy/privacy_line.md`, its one source: the footer, the home page and the invitation `wt tester invite` prints all read it, and a test fails if they differ (Spec 11, PB3.1). It states only what the system enforces: Unless you choose counts only, your questions are kept for 180 days with obvious personal details, such as email addresses and phone numbers, removed. Kept questions can be read at any time by the site code, the owner and approved team members. Questions are also sent to a language model provider to check them and to find and summarize excerpts. With counts only, the text of a question is deleted after 24 hours and only counts remain. You can delete all your questions at any time. Then the link: Read how this works
- Examples, under the heading Examples and the line "Not sure what to ask? Press one of these.": example questions, one link each, for the owner to replace with real ones: I keep judging myself during zazen. / How do I stay with grief without being swallowed by it? / What is the difference between practice and trying to improve myself?

## About page

File: `copy/about.md`. The page opens with a two-sentence summary, then short sections with these headings.

**Summary.** Wheel Turning helps you find what Zen teachers and foundational texts have said about the questions that come up in practice. Every answer is made of short excerpts, each with its source, and the site writes none of the excerpts itself.

**Why this exists.** (anchor `why`) Written in the owner's voice and signed with the owner's name. I built Wheel Turning because I wanted to learn from Soto Zen teachers in the United States, and I could not find what I was looking for.

The talks were spread across many sangha websites, each in its own format. When I found a talk on my topic, I often could not tell if it spoke to the part I cared about. To know, I had to listen for 30 minutes or more. That time is never wasted. But it does not always answer the question in front of you.

I came to see that others have the same problem. Students want to learn. Teachers, as they prepare their own talks, want to know what others have said. Tens of thousands of talks exist outside of published books, and almost none of them can be searched.

So I built a place where these talks can be found. Here you can find a talk, the part of it that speaks to your question, and the teacher and sangha who offered it. You can give dana to that sangha on its own giving page, so the teaching can keep being offered.

A talk that shares the Dharma, the Buddha's teaching, is said to turn the wheel of the Dharma. That is where the name comes from. I hope this site helps these talks reach further, so that all beings can find the insight that carries them to the other shore. The other shore is an old Buddhist image for awakening.

To teachers and sanghas whose talks are here: thank you. This site exists only because of your teaching, and it works best with your blessing. You can tell us how you want your talks used, or have them removed, on the rights page. Rights and removal

\[Owner's name\]

**What this is.** You describe a question from your practice. The site finds excerpts from recorded talks and from texts such as the Platform Sutra and the Dhammapada. It shows the closest matches first, each under its teacher's name, so you can see how different teachers approach the same difficulty. It does not speak for any teacher.

**How answers are chosen.** Excerpts are ranked by how well they match your question and by nothing else. Who gave the talk, which sangha it came from, and whether a sangha has a giving page do not change the order. A sangha is a community that practices together. When an answer opens with a summary, a language model writes it from the excerpts below it, and each sentence is checked against the excerpts it cites before you see it. Each number in the summary leads to the exact words it rests on, and the excerpts themselves are always shown in full.

**Where the material comes from.** Talks come from sangha archives and recordings. Texts come from published editions. The project uses these sources on its own decision, then asks each teacher, sangha and publisher for permission and records the answer. Each excerpt carries a credit line that says exactly what is true: used with permission, used under a named license, or simply where it came from. The page never says a source gave permission unless it did. See every source and its status

**Giving.** Dana is the Sanskrit and Pali word for generosity. Every excerpt from a talk links to the giving page of the sangha that offered it. After an answer, you can choose to share a gift equally among the sanghas it drew on. This site handles no money and takes no share. The links open each sangha's own page. A separate, optional line is for the site's own running costs, and giving is never needed to use the site.

**Your questions.** (anchor `privacy`) The privacy page (Spec 11, PB3.2). You choose how your questions are kept.

- Keep, the default while the test is private: a question is saved for 180 days with obvious personal details removed, such as email addresses, phone numbers, web addresses and long runs of digits. Then its text is deleted and only counts remain.
- Counts only: a question is saved for 24 hours so you can return to your answer. Then its text is deleted and only counts remain, such as how many teachers and excerpts it drew on, how long it took and clicks on giving links.

If your browser sends the Global Privacy Control signal and you have not chosen, counts only applies. A question that looks like it is about a medical matter or about distress is kept as counts only, whatever you chose. The text of the most serious questions, about harm to yourself or someone else, is never saved. If you flag a question, its text is kept even with counts only, so the project can see what went wrong.

Kept questions can be read at any time by the site's code, its owner and approved team members, through database queries or a simple dashboard. The project reads them to learn which questions the collection cannot answer yet. Feedback notes are read too, but the reports that show them never show your question with them unless you flagged it.

Then the switch and the button Delete all my questions now. Under them: You can change your choice or delete all your questions at any time. Deleting removes every question you asked here, with its answers, your feedback and your clicks on giving links.

Your question, and the excerpts found for it, are sent to a language model provider to check the question for sensitive topics, to check which excerpts fit it and where to cut them, and to write the summary. Under its current terms, the provider does not train on what it receives and deletes it after no more than 30 days. It may keep a request longer if it flags it for a misuse review. The provider can change, and this page will say so if its terms do. The provider is not named, since it can change (Spec 11, open question 7). \[The owner confirms the provider's current terms before each launch.\]

The site reaches you through Cloudflare, which protects it from abuse. Like any web host, Cloudflare receives each request, your questions included, and your IP address, so that it can deliver the page. It handles them under its own privacy policy (linked). We do not use Cloudflare to identify you. (Owner's decision of October 17, 2026, with the edge change in `docs/adr-edge-cost.md`.)

The site also keeps a technical log of each page you open: the kind of page, the time, whether it worked and how long it took, marked with your tester number but never your questions or the excerpts. It is kept for 30 days and read to count visits and fix problems. (Owner's decision of October 20, 2026: the request log of Spec 07, one line per request with the tester id, the route, the status and the latency, kept as long as the web service's log group, 30 days.)

Student voices from recorded classes are never stored. When a teacher answers a student's question, the question is shown before the answer in a lightly edited form, with names and identifying details removed, and labeled "Student". To make that edit, the student's question is sent to the same provider under the same terms, and only the edited text is kept.

For any other privacy request, such as asking what we hold about you, write to hello@wheelturning.org.

**Who runs this.** Wheel Turning is run by \[name\], an individual, as a free project with no advertising and no fees. The plan is to form a nonprofit. The site uses a single cookie to keep you signed in for 30 days.

**Leaving.** If you hold rights in anything shown here, you can have it removed at any time, and we will remove it without argument. Rights and removal

**Problems.** If something on this site is hard to use, tell us on the accessibility page.

## Rights and removal page

File: `copy/rights.md`.

**Summary.** If you hold rights in any talk or text shown here, you can have it removed. Tell us and we will remove it, without argument.

**What we add.** We add a talk or text only where, in our judgement, the rights holder's terms allow it. Some terms allow it explicitly, such as a license or written permission. Others allow it implicitly, such as material shared freely so that anyone can hear or read it. Either way, we ask the rights holder for permission.

- Where the rights are unclear, we ask first and add nothing until the rights holder says yes. This is called opting in.
- Where the terms are more permissive, we add the material, ask for permission, and remove it at once if the rights holder says no. This is called opting out. Foundational texts are in this group: their teachings have been passed on freely for hundreds of years, some for thousands, and the project exists to make them easier to find.

Our judgement that terms allow something is our own decision, not the rights holder's permission, and we never describe it as permission.

**How it works.**

1. We email you a link to confirm the request. Once you confirm it, we stop showing the material you name, before checking that you hold the rights. Requests are handled in the order they arrive. The site is run by one person as a side project, so timing can vary, but each request is handled as promptly as possible.
2. Then we delete the text and our stored copies, and we block the same material from being added again, even from a different web address.
3. If it turns out you do not hold the rights, we may restore it, and we will tell you first.
4. If you would rather give permission or set limits, such as leaving out certain talks or topics, tell us that instead. We will record it and follow it.

**Form.** Fields: Name. Email (help: We email you a link to confirm the request, and reply there.). Both are required (owner's decision of October 15, 2026); an empty name or email, or an address without one `@` and a dotted domain, is marked on its own field. What should be removed? (a web address, a title, or the code shown next to an excerpt). Message (optional). Button: Send request. Note under the button: The link works for 24 hours. We act on the request first and reply after. After sending: Thank you. Please check your email and open the link to confirm the request. Nothing is removed until you do.

**Confirmation page** (`/rights/confirm`, from the emailed link). Heading: Confirm your removal request. Text: Nothing is removed until you confirm. Once you confirm, we stop showing the material you named. Requests are handled in the order they arrive. The site is run by one person as a side project, so timing can vary, but each request is handled as promptly as possible. Button: Confirm request. Confirmed: Request confirmed. Thank you. Requests are handled in the order they arrive, as promptly as possible. We act on the request first and reply after, by email. Expired or used: This link no longer works. The link has expired or was already used. Links work once, for 24 hours. If you still want material removed, send the request again.

**Confirmation email.** Subject: Confirm your removal request to Wheel Turning. Body: Someone asked Wheel Turning to remove material, and gave this email address. If it was you, open this link and confirm the request. It works once, for 24 hours: \[link\]. Nothing is removed until you confirm. Once you confirm, we stop showing the material you named. Requests are handled in the order they arrive. The site is run by one person as a side project, so timing can vary, but each request is handled as promptly as possible. If it was not you, ignore this email. The request lapses and nothing changes. The email repeats nothing the sender typed.

**Student voices.** Recordings of students are never stored. A student's question is kept only in a lightly edited form, with names and identifying details removed, attributed to "Student" and shown as context for the teacher's answer. If you recognize your own question and want it removed, use the form below.

**Sources and their status.** One line per source, in plain words, generated from the ledger. The four statuses are:

- Used on the project's own decision. Permission requested on \[date\].
- Used with permission, recorded on \[date\].
- Used under \[license name\], recorded on \[date\].
- Removed at the rights holder's request.

## Accessibility statement

File: `copy/accessibility.md`.

**Summary.** Wheel Turning aims to conform to WCAG 3.0, the draft accessibility guidelines from the W3C. Because WCAG 3.0 is still a draft, we also meet WCAG 2.2 level AA wherever the draft is undecided.

**Standard and date.** Evaluated against the WCAG 3.0 Working Draft of \[draft date\]. Last evaluated on \[date\]. Tested with a keyboard alone, NVDA with Firefox, VoiceOver on iOS and macOS, TalkBack with Chrome, 200 percent text, 400 percent zoom, forced colors and reduced motion. \[List of requirements met, partly met and not met, generated from the register.\]

**Known gaps.** \[Generated from the register. If there are none, say so.\]

**Tell us about a problem.** Form fields: the page address (filled in when you arrive from a link), what happened, the assistive technology you use (optional), and how to reach you (optional). Button: Send report. Note: We read every report. Replies may take a few days.

## Question privacy controls

On the home page and under About, in the section Your questions.

- Checkbox label: Keep my questions to help improve Wheel Turning.
- Help text when checked: Your question is saved for up to 180 days with personal details such as emails and phone numbers removed. Totals by topic are read. Your question is opened only if you flag it.
- Help text when not checked: Your question is kept for 24 hours so you can come back to your answer, then deleted. Only counts remain.
- Button: Delete all my questions now. Confirmation: This deletes every question you have asked here, with its answer and your feedback. It cannot be undone. Buttons: Delete everything, Cancel.
- Confirmation after a change: Saved. This applies to your next question.

## Dana panel

- Heading: Dana
- Introduction: Dana means giving. If this answer helped, you can give to the sanghas whose teaching it drew on. This site never handles your money. Each link opens that sangha's own page.
- Field labels: Total to share among the sanghas (US dollars). Optional gift to this site (US dollars).
- Button: Show the split
- Result line for each sangha: \[Sangha\]: $\[amount\]. \[Open the giving page of Sangha\].
- Site line, separate: This site, optional: $\[amount\]. \[Open the site's giving page\].
- Note under the result: Shares are equal. The site line is extra and is not taken from the sanghas.
- When no sangha qualifies: None of the sanghas in this answer has a verified giving page yet, so there is nothing to split.
- Nothing is preselected and no wording urges a gift.

## Messages

- Question too short or long: Please write between 3 and 500 characters.
- Too many questions: You have asked a lot of questions this hour. Please try again in \[minutes\] minutes.
- Result line under the question (Spec 11, PB2.3): \[x] teacher(s), \[y] teaching(s), \[z] excerpt(s). Teachings are distinct talks; texts count once texts ship. Each noun is singular when its count is 1. Final wording is the owner's.
- No results: We found no excerpts that match this question well enough. Try describing the situation in different words.
- Fewer than three teachers: Found excerpts from \[n\] teachers for this question.
- Named teacher with nothing: This collection has no excerpts from \[name\].
- Named teacher with few: This collection has \[n\] excerpts from \[name\].
- Invite link not valid: That link did not work. Ask for a new invitation.
- Page not found: That page does not exist.
- Server error: Something went wrong on our side. Your question was not saved. Please try again.
- Excerpt withdrawn: This excerpt was removed at the rights holder's request.
- Question not kept: This question was not kept.

## Summary

Owner's decision of October 12, 2026 (Spec 05, Summary and verifier). The one-sangha note was removed and the drawn-from line reworded at the owner's request of October 15, 2026 (B10, Spec 11, PB2.3).

- Section heading: Summary
- Section headings inside it: Where the talks agree; Where they differ; Points to keep in mind; In sum
- Few talks note: Only \[n] talk(s) in this collection matched this question closely.
- Drawn-from line (Spec 11, PB2.3): The summary draws on \[n] of these excerpts. Each sentence is checked against the excerpts it cites; the excerpts are below.
- Model note, one line under every summary, after the drawn-from line, and never on an answer without a summary (Spec 11, PB2.5). It never names the provider: Written by a language model from the excerpts on this page and checked against them. The teachers have not reviewed it.
- Quoted lines heading: Quoted lines
- Also related (Spec 11, PB2.2), collapsed below the teacher groups: Also related (\[n]). Under it: The summary does not draw on these excerpts.
- A quoted line: “\[quotation]” \[teacher], “\[talk title]”, \[date]. Excerpt \[n]
- Marker label for screen readers: Quoted line \[n]

## References and source links

Owner's decision of October 12, 2026 (Spec 05, Answer assembly, rules 2 and 5).

- Source transcript link: Read the talk's transcript at \[source] (opens their page in a new tab)
- Recording link: Listen at \[h:mm:ss] (opens the source in a new tab). Without a player time: starts at \[h:mm:ss]
- Our transcript link: Read our transcript (made by machine, may contain errors), or Read our transcript (reviewed)
- References summary: References (\[n])
- References introduction, inside the list: Entries follow MLA style. The access date is the day this answer was made.
- Confidence words after a text entry: resolved, probable, unresolved

## Excerpt footer and feedback

Owner's decisions of October 12 and 15, 2026 (Spec 06, Answer page structure, rules 5 and 6, and Logging, rule 4).

- Footer row, in order: Listen, Read, Cite, Give to this sangha, Report. Accessible names: Listen at \[h:mm:ss] (opens the source in a new tab). Read the talk's transcript at \[source] (opens their page in a new tab), or Read our transcript (made by machine, may contain errors). Cite this excerpt. Give to this sangha, \[sangha]. Report this excerpt.
- Inside Cite: the MLA entry, then the button Copy citation. After copying: Citation copied.
- Sticky question: the question text only, no label.
- Control summary, once at the foot of the page: Feedback on these answers
- Question: What do you think of these answers? Choose any that apply.
- Reasons: Helpful. Not relevant to my question. The summary is not supported by the excerpts. An excerpt is cut off or missing its context. Words look wrong (a transcription error). Wrong teacher, talk or date. Hard to follow.
- Note label: Anything else? (optional)
- Note help: Notes are read by the project. Your question is not shown with them unless you flag it.
- Button: Send feedback
- Status after sending: Thank you. Your feedback on these answers was recorded.
- Nothing chosen: Choose at least one reason or write a note.
- Note too long: Please keep the note under 1,000 characters.

## Outside the talks

- Out-of-scope page heading: This is outside what Wheel Turning can help with
- Out-of-scope body: Wheel Turning shares what Zen teachers have said about practice. It cannot help with this question.
- Unrelated page heading: This question may be outside what these talks cover
- Unrelated body: The talks here are about Zen practice. You can still search them for this question.
- Unrelated button: Search the talks anyway

## Footer

Three columns from 900px, stacked below. About: Wheel Turning helps you find what Zen teachers and texts have said about your practice. \[About\] \[Accessibility\]. Rights and removal: If you hold rights in anything shown here, we will remove it on request. \[Rights and removal\]. Sources: \[Every source and its status\]. Last line: Excerpts remain the work of their teachers and publishers.

## Teacher notice

File: `templates/teacher_notice.md`. `wt rights notice-draft --teacher <name>` fills the brackets from the database and writes a copy to `data-inbox/notices/`. A person reads it, edits it and sends it by hand. It is written to be honest about what already happened, and to make leaving as easy as staying.

```markdown
Subject: Your talks are in a search tool I am building, and you can leave at any time

Dear [Teacher name],

I am building Wheel Turning, a free search tool for Zen teachings. A person describes a question from their practice, and the tool shows short excerpts from teachers' talks and from foundational texts. Each excerpt comes with a link back to where it came from. The tool shows no advertising, charges nothing and sells nothing. Any gift goes straight to the sangha whose teaching was received, through that sangha's own giving page. The site never touches the money.

I want you to know that [N] of your talks from [Sangha or source] are already in it, in a private test with a small group of invited people. I made that choice myself, before hearing from you. I thought a test without real teachers would teach me nothing, and I would rather tell you plainly than have you find out some other way.

Here is what it holds. A written transcript of each talk, made from the recording. The recording itself is not kept. When you answer a student's question, the question is kept in a lightly edited form, with names and identifying details removed, and shown as "Student" before your answer. Here is what people see. Short excerpts in answers, each with your name, your sangha, a link to the talk at that moment, and a link to your sangha's giving page, and a page with the full written transcript of each talk, carrying the same name, sangha, source and giving links. You can choose less at any time with one message: excerpts only, nothing shown publicly, a copy of your transcripts for your own use, or removal of everything.

Here is what you can do.

1. Leave at any time. Reply to this message, or use the form at wheelturning.org/rights. Your talks stop appearing, with no reason needed. I handle requests in the order they arrive. I run the site alone as a side project, so timing can vary, but I handle each one as promptly as I can. Then I delete the transcripts and block the same material from being added again.
2. Set limits. Tell me which talks, or which kinds of talk, to leave out, and I will follow it.
3. Say yes in writing. I will record it, and the pages will then say your talks are used with your permission.

If I do not hear from you, I will keep going as described. The site will say openly that I have not heard from you, and I will write to you again from time to time. The test stays private, and I will tell you before it opens to anyone else.

I would be glad to show you how it looks and to hear what is wrong with it. Thank you for the teaching.

With respect,
[Your name]
[How to reach you]
```

Variants: for a teacher whose talks come from a source that holds a license, replace the second paragraph with a sentence that names the license and says the talks are used under it. For a teacher the owner has already heard from, start from the permission reply below.

## Sangha or publisher notice and permission request

File: `templates/source_notice.md`. For a sangha that publishes talks, or a publisher or site that offers texts. It tells them what is already happening and asks for permission in the same letter. The BDK America request drafted earlier is an instance of it.

```markdown
Subject: Request for permission to excerpt [Title or collection] in a free, noncommercial search tool

Dear [Name or team],

I am building Wheel Turning, a free search tool that helps people find what Zen teachers and foundational texts have said about the questions that come up in practice. It carries no advertising and no fees. Any gift goes straight to the sangha whose teaching was received, through that sangha's own giving page. The site handles no money. I run it as an individual for now and plan to form a nonprofit.

Your [talks, texts, archive] at [URL] are a valuable source, and they are already in a private test with a small group of invited people. I included them on my own decision, before asking, and I am writing to ask now.

Here is how excerpts are used. Each excerpt is short, up to about 250 words. It appears with the work's title, [the translator,] the volume and page or the moment in the talk, a credit to [Name], and a link back to your page. The tool never shows a whole work or a whole chapter. The text is kept only so it can be searched. Our credit line says only what is true. Until you reply, it reads that the material is used on the project's own decision.

I have three questions.

1. Do you give permission for this use, and is there wording you would like for the credit?
2. Is there anything we should leave out, for example where a translator or another publisher holds rights?
3. [Who holds the rights to [specific work]?]

If you would rather not take part, tell me and I will remove everything and make sure it is not added again. I handle requests in the order they arrive, as promptly as I can. You can also use wheelturning.org/rights at any time.

Thank you for making this material available, and for considering this.

With appreciation,
[Your name]
[How to reach you]
```

## Reply to a removal or report request

File: `templates/reply_removal.md`.

```markdown
Subject: Re: Your request to remove [target]

Thank you for telling me.

As of [date and time], [target] no longer appears on Wheel Turning. I acted before checking anything, as promised on the site.

Within [7] days I will delete the text and my stored copies, and block the same material from being added again, even from another address. You do not need to do anything else.

If you meant something smaller, such as leaving out one talk or one passage, tell me and I will restore the rest. If you hold the rights and would rather give permission or set limits, you can tell me that instead.

If it turns out you do not hold the rights, I may restore it, and I will write to you first.

[Your name]
```

## Reply when permission is granted

File: `templates/reply_permission.md`.

```markdown
Subject: Re: Permission to use [title or collection]

Thank you. I have recorded your permission, dated [date], covering [scope in your words].

From now on the pages say that [title or collection] is used with permission of [name]. The credit line will read: [credit line]. If you want different wording, tell me.

You can change your mind about any part of this at any time, by replying here or using wheelturning.org/rights. Thank you again.

[Your name]
```

## Attribution lines

Every excerpt shows one credit line chosen by the rights basis. A line may use `{title}`, `{author}`, `{translator}`, `{publisher}`, `{teacher}`, `{sangha}`, `{grantor}`, `{license_name}`, `{license_url}` and `{url}`. A sentence that contains a variable with no value is left out, so a text with no translator simply loses its translator sentence. Lines are kept under 40 words. A mandate line never contains the words permission or licensed, and a test enforces that, unless the talk's source holds a sangha's grant (Sangha permission, talks).

| Basis and material | Credit line |
| --- | --- |
| Permission, texts | Used with permission of {grantor}. {title}. Translated by {translator}. Read it at the source: {url} |
| Permission, talks | Talk by {teacher}, {sangha}. Used with permission of {teacher}. |
| Sangha permission, talks | Talk by {teacher}, {sangha}. Used with permission of {grantor}. Shown when the talk's source has a sangha's written yes recorded with `wt rights grant` (owner's decision of October 14, 2026); {grantor} is the source's publisher, the sangha. |
| License, texts or talks | {title}. {license\_name}, {license\_url}. Read it at the source: {url} |
| Mandate, BDK America | Excerpt from {title}. Translated by {translator}. Published by BDK America, which offers the full text free. Read it at the source: {url} |
| Mandate, Terebess | Excerpt from {title} by {author}. Translated by {translator}. Found on the Terebess Zen archive. The work belongs to its author or translator, not to this site. Read it at the source: {url} |
| Mandate, any other text | Excerpt from {title} by {author}. Translated by {translator}. Published by {publisher}. Read it at the source: {url} |
| Mandate, talks | Talk by {teacher}, {sangha}. Listen at the source: {url} |

## Commands that record the two mandates

You type these yourself, because the mandate is your recorded decision. They need the repository built to milestone M4b, and the `wt` command replaces the earlier `dg`. Replace the bracketed name and date.

```text
wt source add --kind texts --key bdkamerica --publisher "BDK America" \
  --prefix https://www.bdkamerica.org/ --prefix https://bdkamerica.org/ \
  --by "[your full name]" \
  --rationale "Free PDFs published by the rights holder for reading and study. Used for finding passages, shown briefly with credit and a link to the source, noncommercial, removable on request. Permission requested on [date]." \
  --display-notice "Excerpt from {title}. Translated by {translator}. Published by BDK America, which offers the full text free. Read it at the source: {url}"

wt source add --kind texts --key terebess --publisher "Terebess" \
  --prefix https://terebess.hu/ \
  --by "[your full name]" \
  --rationale "A long-standing public archive of Zen texts by many authors. Used for finding passages, shown briefly with credit and a link to the source, noncommercial, removable on request. Documents that carry another party's copyright notice are flagged and can be held individually." \
  --display-notice "Excerpt from {title} by {author}. Translated by {translator}. Found on the Terebess Zen archive. The work belongs to its author or translator, not to this site. Read it at the source: {url}"
```

Then record the permission request for BDK America that was drafted earlier, and each one you send afterward:

```text
wt rights request --source bdkamerica --to "https://www.bdkamerica.org/contact/" --channel form --draft-file data-inbox/notices/bdk-request.md
wt rights sent --source bdkamerica --on [date]
```

## Sensitive question notices

File: `copy/sensitive.md`. The tone is plain and kind and makes no claims it cannot keep.

**Acute, full page.** Heading: Please reach out for help now. Text: If you are thinking about hurting yourself or someone else, please contact someone right away. In the United States you can call 988 or text 988, or chat at 988lifeline.org, at any time. If you are in danger now, call 911. Outside the United States, you can find a local line at findahelpline.com. You can also tell a person you trust, a teacher or a doctor today. This site shows what teachers have said. It is not counseling, medical advice or emergency help. Links: Show teachings on facing difficult feelings. Ask something else.

**Distress, banner.** If you are going through something painful, you do not have to carry it alone. In the United States you can call or text 988 at any time. This site shows what teachers have said. It is not counseling.

**Medical, banner.** This site shows what teachers have said. It is not medical advice. For symptoms, medicines or a diagnosis, please talk with a doctor or pharmacist. In an emergency in the United States, call 911.

## About page additions

The Your questions section above now holds what this section added: what goes to the language model provider, and that the text of the most serious questions is never saved. If a question looks like it may involve crisis, the page leads with places to find help and leaves out the summary and the giving panel.
