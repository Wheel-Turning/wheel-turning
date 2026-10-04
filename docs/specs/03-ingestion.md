# Spec 03: Discovery and ingestion

Implements M2 (registry, consent, gate) and M3 (discovery and ingestion). It takes a video from a source listing to a talk in state `cleared`, with speaker, sangha, series, part and type resolved, and it defines the job queue every later stage runs on. Fetching media, transcribing and everything after `cleared` belongs to Spec 04. Table definitions are in Spec 02 and are not repeated here.

## Talk state machine

Every talk has one state in `talk.state`. The pipeline states are the eight stages between `cleared` and `ready` and are listed in Spec 02.

Discovered moves to Awaiting consent after classification. Awaiting consent moves to Cleared when a rights basis is recorded (consent, a verified license or the owner's mandate) and the rights holder is known. Cleared moves through fetch, transcribe, diarize, segment, correct, extract, resolve and index to Ready. Needs review holds a talk whose speaker or media is unclear and returns it to Awaiting consent or sends it to Excluded. Excluded covers duplicates, non-talks, declines and held-out talks. Quarantined takes a talk out of serving when a check fails, until it is repaired and rechecked. Withdrawn follows a takedown, and a stub row blocks re-ingest.

Only the accented arrow lets a talk into the pipeline, and each pipeline job rechecks the gate before it runs. Quarantine holds a talk out of serving without deleting anything, and a talk returns to the pipeline after repair and a passing re-check. Takedown is the only route to `withdrawn`.

## Source adapters

All sources implement one trait in `wt-ingest`. Discovery returns metadata only. The two fetch methods take a `Talk<Consented>`, so media and captions cannot be requested for a talk that has not passed the gate.

```rust
#[async_trait]
pub trait SourceAdapter: Send + Sync {
    fn platform(&self) -> SourcePlatform;
    async fn discover(&self, since: Option<OffsetDateTime>) -> Result<Vec<DiscoveredVideo>, IngestError>;
    async fn fetch_media(&self, talk: &Talk<Consented>, dest: &ScratchDir) -> Result<FetchedMedia, IngestError>;
    async fn fetch_captions(&self, talk: &Talk<Consented>) -> Result<Option<CaptionTrack>, IngestError>;
}

pub struct DiscoveredVideo {
    pub external_id: String, pub url: String, pub title: String, pub description: Option<String>,
    pub published_at: Option<OffsetDateTime>, pub duration_s: Option<i32>,
    pub channel_ref: Option<String>, pub raw: serde_json::Value,
}
pub struct FetchedMedia { pub path: PathBuf, pub kind: MediaKind, pub bytes: u64 }   // Audio | Video
pub struct CaptionTrack { pub language: String, pub format: CaptionFormat, pub body: String, pub is_auto_generated: bool }
```

Sources are listed in `config/sources.toml`.

```toml
[[source]]
name = "ssz-youtube"
platform = "youtube"
channel_id = "UC_REPLACE_ME"        # or playlist_ids = ["PL..."]
default_sangha = "Seattle Soto Zen"
fetch_mode = "owner_api"            # owner_api | third_party

[[source]]
name = "ssz-storage"
platform = "storage"
root = "s3://REPLACE_ME/ssz-video/" # or a local directory
extensions = ["mp4", "m4a", "mp3", "wav"]
```

The YouTube adapter has these behaviors:

1. Discovery uses the official YouTube Data API v3 with `YOUTUBE_API_KEY`. It reads the channel's uploads playlist id from `channels.list` (`part=contentDetails`), pages through `playlistItems.list` at 50 per page, then calls `videos.list` (`part=snippet,contentDetails`) for batches of 50 ids to get title, description, publish time and ISO 8601 duration. Each list call costs one quota unit against the default daily allowance of 10,000, so a few hundred videos need well under 20 calls. The raw API item is stored in `source_video.raw`.
2. The source URL stored for each talk is the canonical watch URL. Timestamped links append `&t=<seconds>s`.
3. In `owner_api` mode, captions come from `captions.list` and `captions.download`, which need OAuth consent from the owner of the channel. Credentials are a refresh token in Secrets Manager (`YOUTUBE_OAUTH_REFRESH_TOKEN`). If the channel is not controlled by the founder, `fetch_captions` returns `None` and the pipeline transcribes from audio.
4. The Data API offers no media download, so `fetch_media` in `owner_api` mode returns `IngestError::MediaNotAvailable` and the worker falls back to the storage adapter, which holds the SSZ video.
5. In `third_party` mode the adapter shells out to `yt-dlp` for subtitles and audio. This mode exists only behind `WT_ALLOW_THIRD_PARTY_FETCH=true`. The default is false, startup logs a warning when it is true, and the adapter returns `IngestError::ThirdPartyFetchDisabled` otherwise. The PRD lists YouTube terms and caption ingestion at scale as a counsel question, so nobody enables this mode before that review.

The storage adapter lists files under its root with name, size, modified time and duration from `ffprobe`. It reads a file only inside `fetch_media`. `wt ingest match-storage` pairs storage files to YouTube records by normalized title, date and duration, writes a review table of proposed matches, and on confirmation sets `source_video.raw.storage_key` on the YouTube record. A talk with no storage match stays at `needs_review` with reason `no_media`.

## Deduplication and classification

Discovery creates a `source_video` row and a `talk` row in state `discovered` for every new video, then a `classify` job runs these rules in order. Tunable values live in `config/pipeline.toml` and the defaults below are starting points to adjust against the corpus review.

1. Normalize the title: Unicode NFKC, lowercase, strip punctuation, collapse spaces.
2. Non-talk filter. A video shorter than `min_talk_seconds` (default 300) or matching any pattern in `exclude_title_patterns` (such as `quote`, `clip`, `trailer`) becomes `talk_type = non_talk`, state `excluded`, reason `non_talk`. The 11-second quote clip in the SSZ archive is the test case. A separate `exclude_session_patterns` list holds the Antiracist 12 Steps session, which the PRD excludes regardless of consent.
3. Duplicate detection. Two talks are duplicates when their dates are within one day, their durations differ by no more than the larger of 30 seconds and 2 percent, and the `pg_trgm` similarity of their normalized titles is at least 0.6. Identical normalized titles with durations within two minutes also match. The kept record is the one with a captions track, otherwise the earliest published. The other gets `duplicate_of`, state `excluded` and reason `duplicate`.
4. Series and part. `config/series.toml` lists each known series with a regular expression that has a named capture `part`. It starts with Genjokoan, Uji, Bendowa, Blue Cliff Record and Song of the Grass Hut. A match sets `series` and `part`.
5. Talk type. Title patterns map to `class`, `retreat_talk` and `service`. An unmatched video of 900 seconds or longer becomes `dharma_talk`. Anything else becomes `other` and goes to `needs_review`.
6. Speaker. Aho-Corasick over every alias in the registry runs against the title and then the description, on word boundaries, case-insensitive. Exactly one matching teacher sets `teacher_id`, `sangha_id` from the teacher, and `speaker_guess` to the matched alias. Zero or several matches set `needs_review` with reason `speaker_unresolved`.

A talk that survives every rule moves to `awaiting_consent`. Every merge, exclusion and reclassification writes a row to `correction_log`.

`wt ingest report --out report.html` writes the review page: duplicate clusters with the kept record marked, excluded items with reasons, talks in `needs_review` with their reasons, series coverage by part number so a gap such as missing Genjokoan paragraphs 5 and 6 is visible, a month-by-month count that shows a gap like April 2022 to February 2023, and any donation-looking links found in descriptions for a human to verify. The report sets nothing automatically.

## The consent gate in the pipeline

The gate has a Rust half and a database half, and both must agree. The database half is the `past_gate_requires_consent` constraint in Spec 02. The Rust half is a typestate in `wt-core`.

```rust
pub struct Pending;
pub struct Consented;
pub struct Talk<S> { pub id: TalkId, pub teacher_id: Option<TeacherId>, /* ... */ _s: PhantomData<S> }

pub struct RightsHolder(String);          // constructor rejects blank and "unknown" in any case

#[derive(Debug, thiserror::Error)]
pub enum GateError { NoSpeaker, ConsentMissing, ConsentDeclined, ConsentWithdrawn, RightsUnknown }

impl Talk<Pending> {
    pub fn authorize(self, consent: &CurrentConsent, rights: Option<&RightsHolder>)
        -> Result<Talk<Consented>, GateError>;
}
```

Rules:

1. Only `authorize` builds a `Talk<Consented>`, and its constructor is private to `wt-core`. Every function that fetches media, writes a transcript, segments, extracts, embeds or indexes takes a `&Talk<Consented>`.
2. Every job handler loads its talk and calls `authorize` again at the start, so a consent change between stages stops the pipeline at the next job.
3. State changes go through `TalkRepo::advance(&Talk<Consented>, next_state)`, which exists only for consented talks. The only way into `cleared` is `TalkRepo::clear(Talk<Consented>)`.
4. `wt consent set --teacher <name> --status <status> --evidence <ref>` appends a `speaker_consent` row and then applies the change to every talk of that teacher in one transaction. With `granted`, talks whose rights holder is known and whose state is `awaiting_consent` move to `cleared`, and the rest stay with reason `rights_unknown`. With `declined`, talks move to `excluded` with reason `declined`. With `withdrawn`, the command stops and asks for explicit confirmation, then runs the takedown routine in Spec 07 for the teacher.
5. `wt talk set-rights --talk <id> | --teacher <name> --holder <text>` sets the rights holder and logs the change. `--holder speaker` writes the teacher's display name. A talk may also be excluded by hand with `wt talk exclude --talk <id> --reason <text>`, which is how a speaker's limits are applied in v1.
6. A teacher with no consent row is treated as `unknown`. The default is never permission.

## Job queue

The queue is the `job` table from Spec 02 with a small worker in `wt-cli`. Claiming uses `SKIP LOCKED` so any number of workers can run without double-claiming.

```sql
with next as (
  select id from job
  where state = 'queued' and run_at <= now() and kind = any($1)
  order by run_at
  for update skip locked
  limit 1)
update job set state = 'running', locked_by = $2, locked_at = now(), attempts = attempts + 1
from next where job.id = next.id
returning job.*;
```

Behavior:

1. Job kinds are `classify`, `audio`, `correct`, `extract`, `resolve` and `index`. The `audio` kind runs the fetch, transcribe, diarize and segment steps in sequence on one machine, as Spec 04 explains. Each handler does its work, writes its output, advances the talk state and enqueues the next kind in the same transaction. A talk's jobs therefore run strictly in sequence.
2. The idempotency key is `{kind}:{talk_id}:{input_hash}`, where the input hash covers the stage's inputs (for example the current transcript hash and the glossary version). Enqueue uses `on conflict do nothing`, so re-enqueueing unchanged work is a no-op and changed inputs produce a new job.
3. On failure with attempts left, the job returns to `queued` with `run_at` pushed back by an exponential delay of 30 seconds doubled per attempt and capped at one hour, and `last_error` is stored. After `max_attempts` the job becomes `dead` and the talk keeps its stage state with `state_reason` set to the error summary.
4. A reaper returns `running` jobs whose `locked_at` is older than `job_lock_timeout_s` (default 7200) to `queued`. Long jobs such as ASR refresh `locked_at` every minute.
5. `wt worker --kinds audio --concurrency 1` runs only the named kinds, so the GPU host takes the heavy stages and any other machine takes the rest.
6. `wt pipeline retry --talk <id>` re-enqueues the failed stage. `wt pipeline advance` enqueues `audio` for every talk in `cleared`.

## Commands, configuration and acceptance

Commands added by this spec:

- `wt registry import --file registry.toml` loads teachers, aliases and sanghas.
- `wt consent set | list`, `wt talk set-rights`, `wt talk exclude`.
- `wt ingest discover [--source <name>]`, `wt ingest match-storage`, `wt ingest report --out <path>`.
- `wt pipeline advance`, `wt pipeline retry --talk <id>`, `wt worker --kinds <list> --concurrency <n>`.

Ingest keys in `config/pipeline.toml`: `min_talk_seconds = 300`, `dedupe_title_similarity = 0.6`, `dedupe_duration_tolerance_pct = 2`, `dedupe_duration_tolerance_min_s = 30`, `dedupe_title_identical_tolerance_s = 120`, `long_talk_seconds = 900`, `exclude_title_patterns`, `exclude_session_patterns`, `job_lock_timeout_s = 7200`.

Acceptance criteria for M2:

- [ ] `Talk<Pending>` cannot be passed to any fetch, transcript, index or advance function. A compile-fail test (`trybuild`) proves it.
- [ ] `authorize` returns the correct `GateError` for each of: no speaker, no consent row, declined, withdrawn, rights unknown, rights `Unknown` in mixed case.
- [ ] Consent propagation: granting moves eligible talks to `cleared`, declining moves them to `excluded`, and withdrawing requires confirmation and triggers takedown.
- [ ] `speaker_consent` history is append-only through the CLI and the database.
- [ ] The alias seed loads the PRD's variants, and ambiguous or missing speakers land in `needs_review`.

Acceptance criteria for M3:

- [ ] YouTube discovery against a recorded fixture of the Data API (served by `wiremock`) yields the expected `source_video` rows, parses durations correctly, and pages past 50 items.
- [ ] A synthetic corpus with planted duplicates, an 11-second clip, a title-variant duplicate and a differently dated near-duplicate produces the expected exclusions and no false merges.
- [ ] Series extraction handles the five named series and records part numbers. The report lists missing parts.
- [ ] Eight concurrent workers never claim the same job. Retry backoff, `dead` after max attempts, and the stale-lock reaper each have a test.
- [ ] With `WT_ALLOW_THIRD_PARTY_FETCH` unset, any `third_party` fetch returns `ThirdPartyFetchDisabled` and no subprocess starts.
- [ ] `wt ingest report` renders every section listed above from the fixture corpus.

## Licensed sources

This section amends the PRD, which lists open-license material and other sanghas as out of scope for v1. Talks that a Soto sangha itself publishes under a license we may rely on can enter the v1 corpus beside consented SSZ talks, which gives the evaluation more voices. Speaker consent remains the basis for SSZ talks. A licensed talk is cleared by license evidence instead. The demo stays private.

What counts as evidence. A publisher's statement of a named license, on a page, in a feed or in a video's license flag. Being downloadable, or having no terms of service, is not a license, because copyright applies by default and silence is not permission. The Everyday Zen Foundation is already an open item in the PRD. The first license read found the opposite of a license: the site footer reserves all rights, so that source stays gated until the foundation or Norman Fischer grants permission in writing. The same check found that the San Francisco Zen Center archive says its talks are published under a Creative Commons license without naming the variant in what was visible, and that Dharma Seed states Creative Commons Attribution-NonCommercial-NoDerivatives 4.0 across its site. The capture step below records exactly what each page says.

Allowed licenses are set in `config/pipeline.toml`:

```toml
[licenses]
allowed_license_codes = ["public-domain", "permission-excerpt", "operator-mandate", "cc0", "cc-by-4.0", "cc-by-3.0", "cc-by-sa-4.0", "cc-by-sa-3.0"]
```

Noncommercial and no-derivatives variants are off by default because both raise open questions. A no-derivatives license may treat transcription and excerpting as adaptation, and a noncommercial license may reach a site that carries a donation line. Adding one to the list needs counsel's answer, recorded in `correction_log`. Many archives use those variants, so this default may exclude a large share of what exists.

Mechanics:

1. `wt license capture --sangha <name> --url <page> --code <license code> --scope "<text>"` fetches the page with plain HTTP and respects `robots.txt`, saves its text to S3 under `license-evidence/`, records the SHA-256, and inserts a `license_evidence` row with `allowed = false`.
2. `wt license approve --id <evidence id> --reviewer <name>` is a person's step after reading the snapshot. It sets `allowed = true` only if the license code is in `allowed_license_codes`, and refuses otherwise.
3. `wt license apply --source <name> --evidence <id> [--limit <n>] [--since <date>]` sets `rights_basis = license`, `rights_holder` to the sangha name or the teacher where known, and `license_evidence_id` on matching talks, then moves them to `cleared`. For YouTube sources it applies only to videos whose `raw.status.license` is `creativeCommon`, so discovery now requests `part=snippet,contentDetails,status` from `videos.list`. Other videos stay gated.
4. The gate's `authorize` takes a rights basis, either `Consent(&CurrentConsent)` or `License(&LicenseEvidence)`. `GateError` gains `LicenseNotAllowed` and `LicenseUnverified`. Every stage re-authorizes as before. Setting `allowed = false` later stops that sangha's talks at their next job and removes them from the serving view at once.
5. A new `RssAdapter` (`platform = "rss"`) reads podcast feeds. It takes title, publish date, duration, description and the enclosure URL from each item, and `fetch_media` downloads the enclosure, which is the file the publisher distributes for listening. It sends a `User-Agent` naming the project and a contact address, waits at least one second between requests, and returns `None` for captions.
6. A sangha's licensed talks bring new teachers. Import them with `wt registry import`. They need no `speaker_consent` rows, and an unresolved speaker still lands in `needs_review`.
7. Start with a capped batch chosen to cover the 50-question topics, and measure teacher breadth before ingesting more. Transcribing a large archive costs real GPU time.
8. Everything else the PRD promises still holds: attribution, the sangha's own donation link, which stays unverified until someone verifies it, and a removal path. `wt takedown --sangha <name>` expands into per-talk takedowns, each logged with scope `talk`. Notify each sangha before the corpus widens beyond the private demo, and honor any removal request even where the license would allow use.

```toml
[[source]]
name = "sfzc-podcast"
platform = "rss"
feed_url = "https://REPLACE_ME/feed.xml"
default_sangha = "San Francisco Zen Center"
```

Acceptance criteria for licensed sources:

- [ ] `capture` stores the snapshot and hash and inserts evidence with `allowed = false`.
- [ ] `approve` refuses any code outside `allowed_license_codes`, including every noncommercial and no-derivatives variant by default.
- [ ] A licensed talk cannot reach `cleared` without evidence, and cannot appear in `servable_passage` unless that evidence is `allowed`. Both are database tests.
- [ ] `apply` on a YouTube source skips videos that are not flagged `creativeCommon`.
- [ ] Setting evidence to `allowed = false` stops the talk's next job and removes it from the serving view.
- [ ] `RssAdapter` passes feed fixtures, sends the identifying `User-Agent`, honors the delay, and never fetches a path that `robots.txt` disallows.
- [ ] `wt takedown --sangha <name>` removes every talk of a sangha and logs each one.

## Noncommercial conditions

Purpose. Wheel Turning is not a commercial venture. It exists to help people find what teachers have said and to carry dana to the sanghas that gave the talks. The rules below turn that intent into checks the build enforces, so a noncommercial license is relied on only while they hold. They do not settle whether an NC or ND license permits transcription and excerpting. That question stays with counsel. Forming a nonprofit is a legal step outside the code, and the configuration flag below is an assertion the build checks, not a status.

Rules:

1. Free to use. No ads, no paid tiers, no payment for access, no sponsorship placements, and no sale or licensing of the corpus or of data derived from it.
2. Dana is never a condition. The site's own dana line is optional and separate. No route, feature or rate limit depends on whether anyone has given.
3. Nothing flows to the project from a sangha's gift. The product handles no money and takes no cut, as the PRD says.
4. Any grant or donation to the project funds operating costs only. The runbook keeps a log of funding sources, so the question "was this use commercial" can be answered later with facts.
5. Every passage from a licensed talk shows its speaker, its sangha, the license name with a link to the license text, and a marker that it is an excerpt. The answer assembler in Spec 05 adds a `license` field to `PassageView` for this.
6. Material from a ShareAlike source carries its license code on every exported line. An export marked for sharing outside the project is refused for those sources until counsel confirms the terms.

Enforcement:

1. `config/pipeline.toml` gains `[deployment] commercial_use = false`. Startup refuses `commercial_use = true` while any talk that is not withdrawn is cleared by evidence whose license code contains `-nc`. Turning the flag on therefore requires removing or re-clearing those talks first.
2. A noncommercial code is not in `allowed_license_codes` by default. Counsel adds one, and the loader rejects an allow-list with an NC code unless `commercial_use` is `false`. An ND code needs its own counsel decision, recorded in `correction_log`.
3. A template test fails if any template loads a third-party script, image or frame, apart from outbound donation links. The content security policy already blocks other hosts at runtime.
4. A behavior test shows a tester with no `dana_event` rows has full access to every route, and a grep test fails if any access check reads `dana_event`.
5. `cargo deny` also bans advertising and analytics crates.

Acceptance criteria:

- [ ] Startup fails when `commercial_use = true` and an NC-cleared talk exists, and passes once that talk is withdrawn.
- [ ] The loader rejects an allow-list containing an NC code when `commercial_use = true`.
- [ ] The template test fails on a planted external script tag and passes on the real templates.
- [ ] A tester with no dana events can use every route.
- [ ] A passage from a licensed talk renders speaker, sangha, license name, license link and an excerpt marker.
- [ ] An export flagged for sharing is refused for a ShareAlike source and carries license codes per line otherwise.
- [ ] The runbook has a funding-source log entry template.

## First license read, October 2, 2026

Read from public pages by Claude, not yet captured with `wt license capture` and not approved by a person. The status column says what each source needs next.

| Source | What the pages say | License code | Allowed by default | Status |
| --- | --- | --- | --- | --- |
| Everyday Zen Foundation | The site footer reserves all rights. The podcast feed carries a foundation copyright notice for 2022 to 2023. A 2001 talk page carries a copyright notice in Norman Fischer's own name. The site says talks can be downloaded for a suggested donation. | None | No | Not usable. Ask the foundation and Norman Fischer for written permission. The PRD's rights-holder question has a first answer: the foundation for the site and feed, Norman personally for at least some talks. |
| San Francisco Zen Center archive | The archive says talks are published under a Creative Commons license and does not name the variant. The current archive host adds that talks are shared with SFZC's permission. The archive includes guest teachers. | Unknown | Unknown | Ask SFZC's audio contact for the exact license and whether it covers guest speakers. |
| Dharma Seed | A talk page states Creative Commons Attribution-NonCommercial-NoDerivatives 4.0 for the whole site. | `cc-by-nc-nd-4.0` | No | Blocked until counsel rules on noncommercial and no-derivatives use. |
| AudioDharma | Talk posts by Gil Fronsdal state Creative Commons Attribution-NonCommercial-No Derivative Works 4.0. His talks also appear in the SFZC archive. | `cc-by-nc-nd-4.0` | No | Same block. It also shows that an archive's blanket statement may not cover every speaker in it. |

## Mandated talks

Decision of October 3, 2026. Recorded talks from teachers who have not replied are used by default on the project owner's mandate, and a teacher can leave at any time. This amends the PRD, whose rule is that no talk is indexed without its speaker's consent, and it turns the consent invitation into a notice that also invites explicit permission. Kanshin's view is being sought and may change the decision, so the behavior sits behind one switch that can be turned back with a single command.

What does not change. Student speech is never stored, indexed or shown, whatever the basis. A talk still needs a resolved speaker, a known rights holder and a registered sangha. A speaker who has declined or withdrawn is never used. Honest labeling holds: the page never says a mandated talk is used with the teacher's permission.

The switch. A `rights_setting` row `talks_basis` is `consent` or `mandate`. The migration default is `consent`, so a fresh database never serves a mandated talk until the owner decides. `wt rights set-talks-basis mandate --by "<name>"` turns the policy on, `wt rights set-talks-basis consent --by "<name>"` turns it off, and each change writes a `rights_event`. While it is `consent`, the serving view returns no mandated talks, though nothing is deleted.

How a talk is cleared under the mandate:

1. A talk source, such as a sangha's YouTube channel, is added with `wt source add --kind talks --key <key> --publisher "<name>" --prefix <url> --by "<name>" --rationale "<text>" --display-notice "<line>"`. It creates mandate evidence and a `source_policy` row of kind `talks`.
2. Discovery and classification run as before. When a talk's speaker resolves to a registered teacher who has not opted out and has no active exclusion, and the source is enabled, the talk takes `rights_basis = 'mandate'`, the source's evidence, and state `cleared`, with no wait for a reply. An unresolved speaker still goes to `needs_review`.
3. Every pipeline job re-authorizes the talk, so an opt-out stops the next job at once.
4. In the gate, `authorize` gains `RightsBasis::Mandate(&MandateEvidence)`. `GateError` gains `MandateOff`, `TeacherOptedOut` and `Excluded`.
5. A teacher's answer changes the basis. `wt consent set --status granted` moves that teacher's talks to `speaker_consent` and the page begins to say they are used with permission. `declined` or `withdrawn` runs the teacher takedown and creates exclusions, replacing the earlier behavior of only excluding talks.

Teacher controls and notice:

- `wt teacher opt-out --teacher <name> --reason "<text>" --by "<name>"` sets `opted_out_at`, which removes that teacher's passages from the serving view immediately, then runs the takedown and adds permanent exclusions for the person and the content hashes of the removed transcripts. `--suspend-only` stops after the first step, for the period while a request is verified.
- Teachers, like anyone, can use the Rights and removal page, and every passage, talk or text, carries a "Report this excerpt" link. `PassageView` gains a `report_link`.
- `wt rights notice-draft --teacher <name>` fills `templates/teacher_notice.md` with the facts a teacher should have: what the tool does, that talks are used on the project's own decision, what an excerpt looks like, how to leave with one message, and an invitation to give explicit permission. The notice is drafted and recorded by the CLI and sent by hand. Nothing is sent automatically. Recording the send, the follow-up date and the reply uses the same `permission_request` and ledger commands as for texts, now keyed by teacher.
- On a page, a mandated passage says "Talk by \<teacher>, \<sangha>" with the source link and the attribution line stored with the mandate.

Migrations. `0015_mandate_basis.sql` holds only `alter type rights_basis add value 'mandate';`, because a new enum value cannot be used in the same transaction. `0016_mandated_talks.sql`:

```sql
create table rights_setting (
  key text primary key,
  value text not null,
  updated_by text,
  updated_at timestamptz not null default now()
);
insert into rights_setting (key, value, updated_by) values ('talks_basis', 'consent', 'migration');

alter table source_policy add column kind text not null default 'texts'
  check (kind in ('texts','talks'));
alter table talk add column source_key text references source_policy(key);
alter table teacher add column opted_out_at timestamptz;
alter table teacher add column opted_out_reason text;

alter table rights_event alter column source_key drop not null;
alter table rights_event add column teacher_id uuid references teacher(id);
alter table rights_event add constraint rights_event_target
  check (source_key is not null or teacher_id is not null);
alter table permission_request alter column source_key drop not null;
alter table permission_request add column teacher_id uuid references teacher(id);
alter table permission_request add constraint permission_request_target
  check (source_key is not null or teacher_id is not null);

alter table talk drop constraint past_gate_requires_consent;
alter table talk add constraint past_gate_requires_consent check (
  state not in ('cleared','fetching','transcribing','diarizing','segmenting',
                'correcting','extracting','resolving','indexing','ready')
  or (teacher_id is not null and sangha_id is not null
      and rights_holder is not null and lower(rights_holder) <> 'unknown'
      and ((rights_basis = 'speaker_consent' and consent_status = 'granted')
        or (rights_basis in ('license','mandate') and license_evidence_id is not null))));

create or replace view servable_passage with (security_barrier = true) as
select p.id as passage_id, p.text, p.start_ms, p.end_ms, p.ordinal, p.segment_kind,
       p.embedding, p.tsv,
       t.id as talk_id, t.title as talk_title, t.talk_date, t.series, t.part,
       sv.platform, sv.external_id, sv.url as source_url,
       te.id as teacher_id, te.display_name as teacher_name,
       s.id as sangha_id, s.name as sangha_name, s.listed,
       s.donation_url, s.donation_url_template, s.donation_verified,
       t.rights_basis, le.display_notice
from passage p
join talk t on t.id = p.talk_id
join source_video sv on sv.id = t.source_video_id
join teacher te on te.id = t.teacher_id
join sangha s on s.id = t.sangha_id
left join license_evidence le on le.id = t.license_evidence_id
where t.state = 'ready'
  and t.rights_holder is not null and lower(t.rights_holder) <> 'unknown'
  and te.opted_out_at is null
  and (
    (t.rights_basis = 'speaker_consent' and t.consent_status = 'granted')
    or (t.rights_basis = 'license' and le.allowed)
    or (t.rights_basis = 'mandate' and le.allowed
        and (select value from rights_setting where key = 'talks_basis') = 'mandate'
        and (t.source_key is null
             or exists (select 1 from source_policy sp where sp.key = t.source_key and sp.enabled)))
  );
```

The grants for the new objects follow the pattern above: `dg_web` selects `rights_setting` and nothing writes it except `dg_pipeline`.

Audit additions: a servable passage whose teacher has `opted_out_at` set, or that matches an active exclusion, fails. A mandated talk served while `talks_basis` is `consent` fails. A teacher with mandated talks and no recorded notice after 14 days warns, and so does an overdue follow-up.

Acceptance criteria for mandated talks:

- [ ] With `talks_basis = 'consent'`, a mandated, ready talk returns nothing from `servable_passage`. After `set-talks-basis mandate` it serves, and each switch writes a `rights_event`.
- [ ] A talk from a mandated talk source with a resolved, non-opted-out teacher reaches `cleared` without a consent row, and an unresolved speaker lands in `needs_review`.
- [ ] The constraint rejects a mandated talk with no evidence, and the typestate cannot build a `Talk<Consented>` for an opted-out teacher or an excluded one.
- [ ] `wt teacher opt-out` removes the teacher's passages from the view at once, and the full form deletes passages, embeddings, segments and stored transcripts and adds person and content-hash exclusions. A later discovery run clears none of that teacher's talks.
- [ ] `--suspend-only` removes the teacher from serving and deletes nothing.
- [ ] `consent set --status granted` switches the teacher's talks to `speaker_consent` and the page wording changes to permission. `declined` and `withdrawn` run the teacher takedown.
- [ ] A rendered mandated passage reads "Talk by" the teacher and sangha with a source link and a report link, and a test asserts that the words permission and licensed are absent. Student speech is still absent from every store.
- [ ] `wt rights notice-draft` produces a file from the template and sends nothing.

## Seattle Soto Zen sources

Decision of October 3, 2026, revised the same day. SSZ publishes every dharma talk on Vimeo, and only some of them on Apple Podcasts and Spotify. Vimeo is therefore the complete list and the listening link, and the podcast feed is a partial source of audio. The v1 route is:

1. Vimeo is the canonical list. A `VimeoAdapter` in `wt-ingest` (platform `vimeo`) discovers talks in two ways. With `WT_VIMEO_ACCESS_TOKEN` it lists the account's videos through the Vimeo API and reads name, description, duration, creation time, link, privacy and license. Without a token it reads the video links from the public talks page at `seattlesotozen.org/talks-page/`, obeying `robots.txt`, and takes each video's title, duration and upload date from Vimeo's public oEmbed endpoint. Either way each video becomes a `source_video` of platform `vimeo`, with its metadata in `raw`.
2. Media comes from the first route that works, in this order. (a) A matching podcast episode. `wt source resolve-feed --apple-id 1472964846` finds the feed address, the owner confirms it, and the importer matches episodes to Vimeo talks by normalized title, date and duration. A matched episode's audio enclosure is used and nothing is downloaded from Vimeo. The feed source is configured with `role = "audio_only"`, so it never creates talk records and only attaches audio to talks that Vimeo already lists. (b) A Vimeo download link, which exists only when the account owner has enabled downloads for a video, read through the API with a token that has the `private` and `video_files` scopes. (c) A file from SSZ's own storage through the storage adapter. (d) Third-party fetching, which stays off behind `WT_ALLOW_THIRD_PARTY_FETCH` and needs counsel's answer and SSZ's agreement first, because Vimeo's terms limit downloading to what an owner allows.
3. The owner asks SSZ for route (b) or (c). Ask whoever administers the SSZ Vimeo account either to create a personal access token for the project with the `public`, `private` and `video_files` scopes and enable downloads on the talks, or to share a bulk export. This is the same conversation as the SSZ permission note in the PRD.
4. Link style. `link_style = "vimeo"` renders the page address with `#t=<seconds>s`. A talk with only a podcast match uses `none`, and the page shows "starts at h:mm:ss". This supersedes rule 2 of Answer assembly in Spec 05.
5. Captions. Vimeo can hold text tracks. When the API returns one, it is saved in scratch as a reference for measuring accuracy and is never used as the transcript.
6. A completeness check compares the Vimeo list with the talks page and the feed and reports talks missing from either, so the PRD's figure of about 225 distinct talks is checked against Vimeo.

```toml
[[source]]
name = "ssz-vimeo"
platform = "vimeo"
talks_page = "https://seattlesotozen.org/talks-page/"
default_sangha = "Seattle Soto Zen"
link_style = "vimeo"

[[source]]
name = "ssz-podcast"
platform = "rss"
feed_url = "RESOLVED_BY_wt_source_resolve_feed"
default_sangha = "Seattle Soto Zen"
link_style = "none"
role = "audio_only"
```

Acceptance criteria for the SSZ sources:

- [ ] Vimeo discovery returns the same talk set from a recorded API fixture and from a recorded talks-page fixture with oEmbed responses.
- [ ] Talks-page discovery obeys `robots.txt` and makes no request to a disallowed path.
- [ ] Feed episodes match Vimeo talks by title, date and duration, an `audio_only` source never creates a talk, and a matched talk uses the enclosure with no Vimeo download.
- [ ] With no usable media route a talk stays at `needs_review` with reason `no_media` and nothing is fetched.
- [ ] `resolve-feed` prints the `feedUrl` from a fixture lookup response and saves it only after confirmation.
- [ ] Each `link_style` renders the right link, and `none` renders the "starts at" text.
- [ ] The completeness report lists Vimeo talks missing from the feed, feed episodes with no Vimeo match, and talks-page items missing from Vimeo.
