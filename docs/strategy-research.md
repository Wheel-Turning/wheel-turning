# Strategy research, October 15, 2026

Four questions, each ending in actions that `docs/roadmap.md` turns into stories. Action IDs: C (competition), F (finance), L (legal), V (value proposition).

Sources are web searches run on October 15, 2026, listed at the end. Several are secondary (aggregators, law-firm summaries, forum posts), and the text says so where it matters. Competitive analysis goes stale quickly: refresh it at each increment gate.

The legal section names issues for counsel to review. It is not legal advice.

## 1. Competitive brief: Zen and dharma information for practitioners and teachers

### The competitive set

| Level | Who | What they offer practitioners |
| --- | --- | --- |
| Direct (AI answers grounded in Buddhist sources) | BuddhaBot Plus (Kyoto University and Teraverse, piloted with Bhutan's monastic body); NORBU (trained on the Pali canon and scholarship); Gotama AI (Sutta Nipata); Roshibot (Suzuki Roshi transcripts on Character.ai); general AI study guides | Fluent conversational answers. They paraphrase, mix traditions, and do not show verifiable verbatim sources. Roshibot's own creator worried it "might confuse someone into wrong views of dharma" |
| Direct (talk archives) | SFZC dharma talk archive; Dharma Seed (80,000+ recordings, Insight tradition); EngageWisdom-hosted sangha archives (Berkeley Zen Center and others); Upaya, ZMM and other center sites | Full talks by teacher, date and title. Little or no search by situation. Few full transcripts |
| Direct (texts) | SuttaCentral (CC0 early texts with parallels); 84000; BDK America free PDFs; Terebess; Dharmamitra (Berkeley: machine translation plus semantic search across Pali, Sanskrit, Chinese and Tibetan) | Texts and translations. Dharmamitra is the most capable semantic search, but over canonical texts, not living teachers' talks |
| Indirect | Insight Timer (free library from 17,000+ teachers, Member Plus subscription); Plum Village app (free, donation-funded); Ten Percent Happier, Calm, Headspace | Meditation guidance and courses, organized by teacher and topic, not answers to situational questions |
| Indirect | Lion's Roar and Tricycle (magazines, video workshops, archives, paid membership); the Zen Studies Podcast (Domyo Burk, 340+ episodes) | Edited teaching content by topic |
| Substitutes | Asking one's teacher (dokusan); books; r/zen and r/Buddhism; sangha study groups; general chatbots | The incumbent for most questions. A general chatbot is now the default for many people |

### Feature comparison

Ratings: Strong, Adequate, Weak, Absent. Ratings for Wheel Turning are for the private beta as built. Competitor ratings come from public descriptions, not hands-on testing.

| Capability | Wheel Turning | Dharma AI chatbots | Talk archives | Text sites (SuttaCentral, Dharmamitra) | Insight Timer, Plum Village |
| --- | --- | --- | --- | --- | --- |
| Ask in your own words | Strong | Strong | Absent | Adequate (Dharmamitra) | Weak |
| Verbatim, traceable source for every claim | Strong | Absent | n/a | Strong | n/a |
| Many teachers side by side, agreement and difference | Strong | Weak (blended) | Absent | Absent | Weak |
| Jump to the moment in the talk | Strong | Absent | Weak (whole talk) | n/a | Absent |
| Breadth of corpus | Weak (a few sanghas) | Adequate | Strong per center, fragmented | Strong | Strong |
| Canonical texts alongside talks | Absent (B11) | Adequate | Absent | Strong | Absent |
| Teacher control: opt out, display levels | Strong | Absent | Adequate (own site) | n/a | Adequate |
| Giving to the sangha behind the teaching | Adequate (dana panel) | Absent | Adequate (site donation) | Adequate | Adequate (teacher donations) |
| Full transcripts for reading and accessibility | Adequate (pipeline exists, display gated) | Absent | Weak | n/a | Weak |
| Mobile app, audio, offline | Absent | Varies | Adequate | Adequate | Strong |
| Community, courses, live teaching | Absent | Absent | Weak | Adequate (forum) | Strong |

### Table stakes for this space

A practitioner will not return without these:

1. **Trustworthy attribution.** Every word is clearly a named teacher's or a named text's. In this audience, any doubt about faithfulness ends trust.
2. **A corpus wide enough to answer most questions.** A few sanghas will read as thin. PRD Gate B sets 15 to 25 centers.
3. **Free access.** Every dharma source in the set, apart from the commercial apps, is free or by donation.
4. **Speed and mobile reading.** An answer within seconds on a phone. Spec 10 targets a summary within 5 seconds.
5. **A way back to the source.** Listen, read the full talk, find the teacher's sangha.
6. **Respect for the tradition.** Clear labels on anything a model wrote. No impersonating a teacher, which is the main criticism of the chatbots.

### Real differentiators

No one else in the set does these well:

1. **Verbatim, multi-teacher answers with timestamps.** Chatbots blend voices; archives make you find the talk yourself.
2. **Rights and consent built into the system**: opt-out, display levels, takedown, honest labels. This answers the main objection teachers raise about AI and dharma.
3. **"Dana follows citation."** The sanghas whose teaching was used are the ones the reader is asked to support.
4. **Talks linked to texts** (PRD problem statement): which talks discuss this koan or passage, and what a talk is quoting. No one does this for living teachers. Built in part (work-level citations), not served (B11).
5. **Transcripts as a by-product.** The pipeline produces timed, speaker-cleaned transcripts that most sanghas do not have. This is a gift to sanghas and an accessibility gain for deaf and hard-of-hearing practitioners.

Not differentiators, and not worth chasing for parity: guided meditation, courses, community features, a native app.

### Threats

- **General chatbots are good enough for most casual questions.** They are free and already on people's phones. The defense is faithfulness and attribution, so those must hold at every release.
- **A well-funded institution builds this over its own archive.** The likely candidates are SFZC, EngageWisdom (it already hosts the archives) or a Dharmamitra-style university project. The defense is to be the cross-sangha layer, and to partner with them rather than compete.
- **A teacher's public objection.** Use of a teacher's talks without explicit permission could become the story. The defense is the consent-first public posture (L1) and fast, visible removal.
- **The Suzuki Roshi corpus is shared with Roshibot.** Readers may lump the two together. Copy should draw the contrast: quotes, not an imitation.

### Actions

| ID | Action | Increment |
| --- | --- | --- |
| C1 | State the positioning against chatbots on the site and answer page: "teachers' own words, never an imitation of a teacher" | Public beta |
| C2 | Grow the corpus to the PRD Gate B breadth (15 to 25 centers, three or more teachers in most answers) | Public beta to full launch |
| C3 | Serve texts and the talk-to-text links (B11), the clearest differentiator not yet live | Public beta |
| C4 | Offer partnership, not competition, to EngageWisdom and to one university digital-dharma project (such as Dharmamitra) | Public beta |
| C5 | Refresh this brief at each gate, including a hands-on test of the top two chatbots against the eval questions | Every gate |

## 2. Financial sustainability: how comparable groups fund themselves

### Models in the space

| Group | Model | What it shows |
| --- | --- | --- |
| Dharma Seed | Donations, mostly volunteers. Teachings offered on dana since 2000 | Works for decades but strains: its 2025 appeal says staying alive is "a greater challenge each year" |
| SuttaCentral | Site donations (forum: "ninety-nine percent" of support), a Development Trust, occasional large gifts, paid developers plus volunteer translators | Recurring small gifts can fund a small paid team. Grants were suggested but are not core |
| Plum Village app | Free, donations to a registered foundation that also runs paid retreats | A large community underwrites the app (aggregator source) |
| SFZC archive | Suggested donation of $0 to $10 per talk | Per-item giving prompts at the point of value |
| Insight Timer | Freemium: free library, Member Plus subscription, teacher course marketplace, corporate program | Commercial. Reports differ on whether the premium tier carries the business |
| Lion's Roar, Tricycle | Paid membership with workshops, archive and magazine, plus group plans and scholarships | Commercial. Ruled out by PRD principle 8 |
| EngageWisdom | A platform that digitizes, remasters and hosts sangha archives (pricing not public) | Sanghas will pay for archive services. A potential partner or channel |
| Dharmamitra | University research project, with Tsadra Foundation collaborating on a three-year Dharma AI effort | Foundations will fund Buddhist AI tied to scholarship |
| Khyentse Foundation | Grants for preservation and practice | As of March 2026 not accepting applications for its grant programs, and says it must "adapt and change in the age of AI". Watch for its new programs |

### What this means for Wheel Turning

The PRD rules out ads, paid tiers and selling data, and keeps the site's own dana optional. CLAUDE.md rule 7 keeps payment code out of the product. Money must therefore arrive outside the app: through a fiscal sponsor's or nonprofit's donation page, and through grants.

**The cost to cover.** About $170 a month for AWS, plus the language model. On Claude Haiku 4.5 that is about $0.054 a question (October 15 measurement).

**Prices changed.** The October 15 Claude price list puts Claude Haiku 5.5 at $0.10 per million input tokens and $0.50 per million output tokens, for prompts up to 100,000 tokens. Haiku 4.5 is $1 and $5, so Haiku 5.5 is a tenth of the price. Every prompt here is far under 100,000 tokens. Haiku 5.5 uses the newer tokenizer, which the price list says produces about 30% more tokens for the same text. The net saving is therefore about sevenfold: roughly $0.007 a question with no other change. Spec 10's removal of the ranking and excerpt calls would bring it to roughly $0.003.

| Daily questions | Haiku 4.5 today | Haiku 5.5 | Haiku 5.5 plus Spec 10 |
| --- | --- | --- | --- |
| 100 | about $160 a month | about $21 | about $9 |
| 1,000 | about $1,620 | about $210 | about $90 |
| 10,000 | about $16,200 | about $2,100 | about $900 |

Add AWS: about $170 a month now, and about $135 more for two availability zones and a second task at the public beta.

These estimates scale the measured baseline by price and tokenizer. They hold only if Haiku 5.5 keeps quality on each stage, which roadmap story PB2.0 tests. A cheaper model from a second provider is no longer needed for cost, which avoids its data-handling and privacy changes.

With the model this cheap, fixed AWS cost becomes the larger bill until about 1,000 questions a day. The AWS levers matter as much as model work: scale to zero between rounds, a tunnel instead of the load balancer, ARM, and a Savings Plan once steady.

### Recommended mix, in order

1. **A legal home for money now.** Use a fiscal sponsor until the planned nonprofit exists. The sponsor handles state charitable-solicitation registration and tax-deductible receipts. Fees run up to about 15% of gifts.
2. **Reader dana at the point of value.** An optional "support Wheel Turning" line beside the sangha dana panel, never mixed with it, using the fiscal sponsor's page.
3. **Sponsor a sangha's archive.** A donor funds transcribing and indexing a named sangha's archive, the way 84000 sponsors translations. It is concrete, fits the mission, and gives sanghas a reason to join.
4. **Grants.** Foundations funding Buddhist digital projects (Tsadra-style AI collaborations, Khyentse once its programs reopen, others to research), and digital-humanities partnerships with a university.
5. **Archive services to sanghas on a dana or cost-recovery basis**: transcripts, private archive search, owner exports. This needs a PRD decision on whether cost recovery from sanghas fits principle 8.

### Actions

| ID | Action | Increment |
| --- | --- | --- |
| F1 | Build a cost model at 10x and 100x current use, AWS and model cost per question, and a monthly ceiling that triggers scaling down | Private beta (gate input) |
| F2 | Choose a fiscal sponsor and sign its agreement, so gifts are legal and deductible before any public ask | Before the public beta |
| F3 | Add the site's own optional dana line through the sponsor's page, kept separate from the sangha panel | Public beta |
| F4 | Pilot "sponsor a sangha's archive" with one sangha and one donor | Public beta |
| F5 | Apply to two grant programs with the beta results as evidence | Public beta |
| F6 | Decide whether archive services to sanghas may recover costs (a PRD principle 8 decision) | Public beta gate |
| F7 | Move eligible stages to Claude Haiku 5.5 (PB2.0), then hit a cost-per-question target (Spec 10) that keeps 1,000 questions a day within a funded budget | Private beta, then before the full launch |
| F8 | Have 12 months of projected costs covered by committed gifts and grants | Full launch gate |

## 3. Legal: high-risk, high-value elements by increment

This lists issues to take to qualified counsel, not answers. The main risk comes from the product's own design: using talks by default and removing them on request (PRD principle 6), and processing practitioners' questions, some of which touch on mental health.

### Compliance check summary

The private beta can proceed with conditions. The public beta needs further review: L1, L2, L3, L6 and L8 need counsel's written answers first.

### Risk register

| ID | Risk | Severity | Why | Mitigation to discuss with counsel | Before |
| --- | --- | --- | --- | --- | --- |
| L1 | **Copyright in talks used without permission (the mandate basis)** | High | Spoken talks are copyrighted works, and a transcript is likely a reproduction or derivative work. The closest precedent, Authors Guild v. Google, held search with short snippets transformative, but it rested on not substituting for the work. Full transcripts and long excerpts weaken that | A written fair-use opinion covering excerpts, summaries and full-transcript display. Until then, serve the public only talks with consent or a license, using the existing `talks_basis` switch, and keep mandated talks to invited testers | Public beta |
| L2 | **Creative Commons NC and ND terms** | High | Spec 03 already holds an open question for counsel. A transcript of a CC-ND talk may be a derivative. NC may conflict with any cost recovery (F6) | Counsel's answer, recorded in Spec 03. Exclude ND sources from transcript display until then | Public beta |
| L3 | **Consumer health data (Washington's My Health My Data Act)** | High | The Act covers nonprofits, has no size threshold and has a private right of action. Mental-health signals count as consumer health data. Testers include Seattle sangha members, and the sensitive-question triage processes such signals | Counsel's view on whether question text and triage results are covered. If so: a consumer health data privacy policy linked from the home page, consent before collection, a deletion path, and minimal retention of sensitive questions | Private beta |
| L4 | **Accuracy of privacy statements** | High | Saying something inaccurate about data handling is itself a legal exposure (B9). Questions go to a language-model provider without zero data retention | Correct the privacy line (B9). The privacy page names a language-model provider as a processor and states retention | Private beta |
| L5 | **Personal liability of the operator** | High | The project appears to run under the owner personally. A copyright or privacy claim would reach personal assets | An entity or fiscal sponsor (F2), and media or professional liability insurance quotes | Public beta |
| L6 | **Religious-belief data under the GDPR and UK GDPR** | Medium, High once open to the EU | Religious or philosophical beliefs are a special category. Questions about Zen practice can reveal them, and inference may count | Before EU users arrive: decide to geo-limit or comply (lawful basis, explicit consent, notice) | Public beta |
| L7 | **Self-harm and crisis questions** | High | Acute-risk questions are routed to a support page. Missing or wrong crisis resources could cause harm | Review the support page's resources by region, add a "not a substitute for care" notice, keep the rule and model triage tested in CI, and log no acute question text | Private beta |
| L8 | **Students' voices and words** | Medium | Students in recordings did not agree to transcription. Edited student questions are stored and searchable | Counsel's view on the edited-question approach. Confirm names and identifying details are removed, and give students a request path | Public beta |
| L9 | **Implying teacher endorsement** | Medium | Teacher names, "used with permission" labels and summaries could imply endorsement or misstate a view | Keep labels exact (principle 7), keep the verifier, and add copy saying teachers did not write or review the summary | Public beta |
| L10 | **Terms of the source platforms** | Medium | Downloading media from Vimeo, podcast hosts or archive platforms may break their terms even when the sangha agrees | The sangha's written permission names the download route. Prefer files the sangha supplies | Public beta |
| L11 | **Translations of canonical texts** | Medium | Most English translations are copyrighted. Aggregators such as Terebess republish them | Load texts only under a license or CC0 (SuttaCentral). Treat aggregator mandates as excerpt-and-link only (the B11 link-out order) | Public beta |
| L12 | **Notice-and-takedown process** | Medium | A clear, published process limits exposure and is expected by rights holders | Publish the removal policy (exists, B14 wording), register a DMCA designated agent, and keep the takedown audit (A1) | Public beta |
| L13 | **AI transparency rules** | Medium | The EU AI Act's transparency duties and some US state laws require disclosing AI-generated text | Label every summary as model-written with its verification. Already close to the design | Public beta |
| L14 | **Charitable solicitation** | Medium | An online donation button can require state registration | Covered by the fiscal sponsor (F2). The sangha dana panel only links out, with no tax claim | Public beta |
| L15 | **Name clearance** | Medium | "Turning Wheel" is used by the Buddhist Peace Fellowship magazine and a Leicester temple | A trademark search and counsel's view before spending on the brand | Full launch |
| L16 | **Accessibility claims** | Low to medium | The PRD commits to an honest public conformance statement | Keep the statement accurate as features change | Every gate |
| L17 | **Children** | Low | Users under 13 trigger COPPA duties | Terms state a minimum age of 13 | Public beta |

### Approvals needed

| Approver | For | Status |
| --- | --- | --- |
| Copyright counsel | L1, L2, L8, L11, L12 | Not started. Spec 03 already lists the NC/ND question |
| Privacy counsel | L3, L4, L6, L7 | Not started |
| Fiscal sponsor or nonprofit counsel | L5, L14, F2 | Not started |
| Trademark counsel | L15 | Not started |

## 4. Value proposition: valid, and where higher value may lie

### The job

The PRD's job, restated: *"When something arises in my practice between dokusan, I want to see how teachers I trust have actually addressed it, so that I can keep practicing with their words rather than a guess or an imitation."*

### Verdict

**Valid but narrow on the demand side, and stronger than it looks on the supply side.**

- **Valid.** The gap is real. Chatbots answer fluently but cannot be trusted to be a teacher's words, and teachers and Buddhist media are wary of them. Archives hold the words but cannot be searched by situation. No competitor gives verbatim, multi-teacher, time-linked answers with consent built in.
- **Narrow.** Three assumptions are untested:
  - **Frequency:** practitioners may have a "between dokusan" question only occasionally, and a free chatbot is already on their phone.
  - **The tradition:** Zen's own emphasis on practice over words may make some teachers discourage use.
  - **Breadth:** a few sanghas cannot answer most questions.
- **Stronger on supply.** Sanghas sit on archives that are hard to search, rarely transcribed and inaccessible to deaf practitioners. Teachers preparing talks want to trace references and their own past talks (B8). The pipeline already makes what they lack. Serving them also solves the project's two hardest problems: rights (consent becomes a fair exchange) and sustainability (F4, F6).

### Riskiest assumptions, cheapest tests

| ID | Assumption | Cheapest test | When |
| --- | --- | --- | --- |
| V1 | Practitioners come back with new questions often enough to form a habit | Private beta: share of testers asking in two or more separate weeks, and questions per active tester | Private beta |
| V2 | Teachers will stand behind the excerpts and the summary | Gate A ratings, plus three teachers reviewing answers drawn from their own talks | Private beta |
| V3 | Readers follow dana links (the PRD v1 hypothesis) | Click-through on dana links per answer | Private beta |
| V4 | Sanghas value free transcripts and archive search enough to give explicit permission | Offer three sanghas their transcripts (the owner export at display level d) in exchange for permission; count yeses | Private beta, outside the app |
| V5 | Priests and teachers want a research search returning all relevant passages | The B8 fake door: count recorded interest | Public beta |
| V6 | Talk-to-text links are something readers use | Clicks on text references once B11 ships | Public beta |

### Higher-value gaps worth considering

These come from a short ideation pass and are ranked by value against effort. Each fits the existing pipeline.

1. **A sangha's own archive, searchable and transcribed** (supply side). Each participating sangha gets its talks searchable, plus transcripts for accessibility, embedded or linked from its own site. **Recommended as the second pillar.** It turns rights holders into partners and opens funding (F4).
2. **Research search for teachers and priests** (B8): all relevant passages, references traced, own talks first. Run the fake door, then build if V5 holds.
3. **Talk-to-text map**: "every talk on this koan" and "what this talk quotes". A differentiator, and indexable public pages if teachers allow (S8).
4. **Find a teacher or sangha near me, from the answer.** Every answer already names sanghas, and linking practitioners to living teachers is what teachers want most. Low build from the registry.
5. Set aside: practice companion, journaling and daily-reading apps (crowded and off-mission); liturgy and chant guidance (real but niche; stays in the PRD backlog).

### Actions

| ID | Action | Increment |
| --- | --- | --- |
| V-A1 | Instrument V1 and V3 in the private beta report (return weeks, questions per tester, dana click-through) | Private beta |
| V-A2 | Run the V4 transcript-for-permission offer with three sanghas | Private beta |
| V-A3 | Spec "sangha archive" (search scoped to one sangha, transcripts, an embeddable link) if V4 holds | Public beta |
| V-A4 | Run the B8 fake door and decide on research search from V5 | Public beta |
| V-A5 | Add "find this sangha" links from each answer's teacher groups to the sangha's own page | Public beta |
| V-A6 | Interview 10 testers and 5 teachers at the private beta gate on the job, alternatives and frequency | Private beta gate |

## Sources

Competition and AI tools:
- [Buddhism and AI: Another Look (Digital Orientalist, March 2026)](https://digitalorientalist.com/2026/03/24/buddhism-and-ai-another-look/)
- [BuddhaBot: AI for Buddhism (Asia News Network)](https://asianews.network/buddhabot-ai-for-buddhism/)
- [Bhutan monks turn to AI chatbot for Buddhist guidance](https://mdn.gov.mm/en/bhutan-monks-turn-ai-chatbot-buddhist-guidance)
- [Buddharoid (Wikipedia)](https://en.wikipedia.org/wiki/Buddharoid)
- [Five websites and AI for the teachings of the Buddha (Christopher Titmuss)](https://www.christophertitmussdharma.org/p/five-websites-ai-which-support-diving)
- [Can a Chatbot Share True Dharma? (Lion's Roar)](https://www.lionsroar.com/can-a-chatbot-share-true-dharma/)
- [What AI Means for Buddhism (Lion's Roar)](https://www.lionsroar.com/what-a-i-means-for-buddhism/)
- [Roshibot: Suzuki Roshi as AI (Treeleaf forum)](https://forum.treeleaf.org/forum/treeleaf/the-buddhism-of-the-future-forum-building-the-future-buddha/15597-futurebuddha-roshibot-suzuki-roshi-as-ai?p=450682)
- [Dharmamitra about page](https://dharmamitra.org/zh/about)
- [Dharmamitra on the SuttaCentral forum](https://discourse.suttacentral.net/t/dharmamitra-org-machine-translation-for-classical-asian-languages/35304)
- [Tsadra Dharma AI Project](https://www.tsadra.org/?p=23346)
- [SFZC dharma talk archive](https://www.sfzc.org/offerings/dharma-talk-archive?page=291)
- [Berkeley Zen Center audio archive on EngageWisdom](https://berkeleyzen.engagewisdom.com/bz/about)
- [The Zen Studies Podcast (Apple Podcasts)](https://podcasts.apple.com/podcast/id1209162288)

Funding:
- [Dharma Seed: about](https://dharmaseed.org/about/us)
- [Dharma Seed: history](https://www.dharmaseed.org/about/history)
- [Dharma Seed: summer 2025 support](https://dharmaseed.org/about/summer-2025-support/)
- [Help fund SuttaCentral (forum)](https://discourse.suttacentral.net/t/help-fund-suttacentral/11269)
- [Forming a 501(c)(3) for SuttaCentral (forum)](https://discourse.suttacentral.net/t/forming-a-501c3-in-the-usa-friends-of-suttacentral-development-trust/32233)
- [Plum Village app overview (aggregator, weak source)](https://subger.com/en/service/plum-village-app)
- [How Insight Timer monetizes (UX Collective)](https://uxdesign.cc/turning-meditation-into-money-how-insight-timer-monetizes-25m-users-c79cb29257be)
- [Insight Timer app review (Headway)](https://makeheadway.com/blog/insight-timer-app/)
- [Lion's Roar membership](https://lionsroar.com/membership/)
- [Tricycle customer support](https://tricycle.org/customer-support/)
- [Khyentse Foundation grants](https://khyentsefoundation.org/apply-for-support/)
- [Fiscal sponsorship (Charitable Allies)](https://charitableallies.org/fiscal-sponsorship/)
- [Charitable solicitation registration 101 (Charitable Allies)](https://charitableallies.org/charitable-solicitation-registration-101/)
- [Fundraising during incorporation (Candid)](https://learning.candid.org/nonprofit-fundraising-during-incorporation-process)

Legal:
- [Authors Guild v. Google, fair use summary (U.S. Copyright Office)](https://www.copyright.gov/fair-use/summaries/authorsguild-google-2dcir2015.pdf)
- [Washington My Health My Data Act (Sidley)](https://datamatters.sidley.com/?p=4971)
- [My Health My Data Act: are you ready (Loeb)](https://www.loeb.com/zh-hans/insights/publications/2024/02/washingtons-my-health-my-data-act-are-you-ready)
- [GDPR Article 9 summary (Presencis)](https://presencis.com/regulations/gdpr/article-9/)
- [Article 9 and inferred data (Jentis)](https://www.jentis.com/blog/what-is-article-9-of-the-gdpr-sensitive-data-explained-and-how-companies-remain-compliant)
- [Special category data examples (Bird & Bird)](https://www.twobirds.com/-/media/pdfs/expertise/data-protection/examples-and-analysis.pdf)
