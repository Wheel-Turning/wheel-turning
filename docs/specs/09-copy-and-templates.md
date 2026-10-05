# Spec 09: Copy and templates

This tab holds the words the product and its letters use. Page copy goes in `copy/` as one markdown file per page and is rendered into the templates. Letters go in `templates/`. Text in \[square brackets\] is filled in by a person or by the code. Every page keeps sentences short, defines a term the first time it appears, and avoids idiom, in line with the accessibility standard in Spec 06. The project is run by an individual for now, so the copy says so plainly and the owner updates it when a nonprofit exists.

## Home page

File: `copy/home.md`.

- Tagline: What have Zen teachers said about the question you are living with?
- Question label: Your question
- Help text under the box: Write it the way you would say it. For example: I keep judging myself during zazen.
- Button: Ask
- Line under the button: Everything you see is a short excerpt, with a link to where it came from.
- Privacy line: Your questions are kept so the project can improve. Only totals are read, and one question is opened only if you flag it. You can switch this off or delete your questions at any time. Read how this works
- Example questions, one link each, for the owner to replace with real ones: I keep judging myself during zazen. / How do I stay with grief without being swallowed by it? / What is the difference between practice and trying to improve myself?

## About page

File: `copy/about.md`. The page opens with a two-sentence summary, then short sections with these headings.

**Summary.** Wheel Turning helps you find what Zen teachers and foundational texts have said about the questions that come up in practice. Every answer is made of short excerpts, each with its source, and the site writes none of the excerpts itself.

**What this is.** You describe a question from your practice. The site finds excerpts from recorded talks and from texts such as the Platform Sutra and the Dhammapada. It shows them grouped by teacher, so you can see how different teachers approach the same difficulty. It does not speak for any teacher.

**How answers are chosen.** Excerpts are ranked by how well they match your question and by nothing else. Who gave the talk, which sangha it came from, and whether a sangha has a giving page do not change the order. A sangha is a community that practices together. When the answer includes a short summary line, each part of it is backed by at least two excerpts and is checked against them before you see it.

**Where the material comes from.** Talks come from sangha archives and recordings. Texts come from published editions. The project uses these sources on its own decision, then asks each teacher, sangha and publisher for permission and records the answer. Each excerpt carries a credit line that says exactly what is true: used with permission, used under a named license, or simply where it came from. The page never says a source gave permission unless it did. See every source and its status

**Giving.** Dana is the Sanskrit and Pali word for generosity. Every excerpt from a talk links to the giving page of the sangha that offered it. After an answer, you can choose to share a gift equally among the sanghas it drew on. This site handles no money and takes no share. The links open each sangha's own page. A separate, optional line is for the site's own running costs, and giving is never needed to use the site.

**Your questions.** (anchor `privacy`) You choose whether your questions are kept. If you keep them, a question is saved for up to 180 days with obvious personal details such as email addresses and phone numbers removed. The project reads totals by topic, to learn which questions the collection cannot answer yet, and opens one question only if you flag it. If you switch keeping off, your question is kept for 24 hours so you can return to your answer, then deleted, and only counts remain. You can change the choice or delete all your questions at any time. Student questions and student voices from recorded classes are never stored.

**Who runs this.** Wheel Turning is run by \[name\], an individual, as a free project with no advertising and no fees. The plan is to form a nonprofit. The site uses a single cookie to keep you signed in for 30 days.

**Leaving.** If you hold rights in anything shown here, you can have it removed at any time, and we will remove it without argument. Rights and removal

**Problems.** If something on this site is hard to use, tell us on the accessibility page.

## Rights and removal page

File: `copy/rights.md`.

**Summary.** If you hold rights in any talk or text shown here, you can have it removed. Tell us and we will remove it, without argument.

**How it works.**

1. Within one business day of your request, the material you name stops appearing. We do this before checking who you are.
2. Then we delete the text and our stored copies, and we block the same material from being added again, even from a different web address.
3. If it turns out you do not hold the rights, we may restore it, and we will tell you first.
4. If you would rather give permission or set limits, such as leaving out certain talks or topics, tell us that instead. We will record it and follow it.

**Form.** Fields: Name (optional). Email (optional, but we can only reply if you give a way to reach you). What should be removed? (a web address, a title, or the code shown next to an excerpt). Message (optional). Button: Send request. Note under the button: The project cannot send email yet, so replies may take a few days. We act on the request first and reply after.

**Student voices.** Questions and voices from students in recorded classes are never stored or shown.

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
- No results: We found no excerpts that match this question well enough. Try describing the situation in different words.
- Fewer than three teachers: Found excerpts from \[n\] teachers for this question.
- Named teacher with nothing: This collection has no excerpts from \[name\].
- Named teacher with few: This collection has \[n\] excerpts from \[name\].
- Invite link not valid: That link did not work. Ask for a new invitation.
- Page not found: That page does not exist.
- Server error: Something went wrong on our side. Your question was not saved. Please try again.
- Excerpt withdrawn: This excerpt was removed at the rights holder's request.
- Question not kept: This question was not kept.

## Footer

Three columns from 900px, stacked below. About: Wheel Turning helps you find what Zen teachers and texts have said about your practice. \[About\] \[Accessibility\]. Rights and removal: If you hold rights in anything shown here, we will remove it on request. \[Rights and removal\]. Sources: \[Every source and its status\]. Last line: Excerpts remain the work of their teachers and publishers.

## Teacher notice

File: `templates/teacher_notice.md`. `wt rights notice-draft --teacher <name>` fills the brackets from the database and writes a copy to `data-inbox/notices/`. A person reads it, edits it and sends it by hand. It is written to be honest about what already happened, and to make leaving as easy as staying.

```markdown
Subject: Your talks are in a search tool I am building, and you can leave at any time

Dear [Teacher name],

I am building Wheel Turning, a free search tool for Zen teachings. A person describes a question from their practice, and the tool shows short excerpts from teachers' talks and from foundational texts. Each excerpt comes with a link back to where it came from. The tool shows no advertising, charges nothing and sells nothing. Any gift goes straight to the sangha whose teaching was received, through that sangha's own giving page. The site never touches the money.

I want you to know that [N] of your talks from [Sangha or source] are already in it, in a private test with a small group of invited people. I made that choice myself, before hearing from you. I thought a test without real teachers would teach me nothing, and I would rather tell you plainly than have you find out some other way.

Here is what it holds. A written transcript of each talk, made from the recording. The recording itself is not kept. Questions and voices from students are never stored. Here is what people see. Short excerpts only, never a whole talk, each with your name, your sangha, a link to the talk at that moment, and a link to your sangha's giving page.

Here is what you can do.

1. Leave at any time. Reply to this message, or use the form at wheelturning.org/rights. Within one business day your talks stop appearing, with no reason needed. Then I delete the transcripts and block the same material from being added again.
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

If you would rather not take part, tell me and I will remove everything within one business day and make sure it is not added again. You can also use wheelturning.org/rights at any time.

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

Every excerpt shows one credit line chosen by the rights basis. A line may use `{title}`, `{author}`, `{translator}`, `{publisher}`, `{teacher}`, `{sangha}`, `{grantor}`, `{license_name}`, `{license_url}` and `{url}`. A sentence that contains a variable with no value is left out, so a text with no translator simply loses its translator sentence. Lines are kept under 40 words. A mandate line never contains the words permission or licensed, and a test enforces that.

| Basis and material | Credit line |
| --- | --- |
| Permission, texts | Used with permission of {grantor}. {title}. Translated by {translator}. Read it at the source: {url} |
| Permission, talks | Talk by {teacher}, {sangha}. Used with permission of {teacher}. |
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

Add to the Your questions section of `copy/about.md`:

- To write the short summary line and to check for sensitive topics, the text of your question may be sent to a language model provider. \[Confirm before launch that the provider's zero-retention terms are in place, and say so here in one sentence.\]
- If a question looks like it may involve crisis, the page leads with places to find help, leaves out the summary line and the giving panel, and does not save the text of the most serious questions.
