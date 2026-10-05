# Spec 08: Texts and reference assets

Implements milestone M4b, which runs after M4 and in parallel with Gate A, and is finished before the texts section of the answer page in M5. It adds two kinds of non-talk material to the system with one importer: foundational texts that people can search, and reference assets that feed the pipeline. The schema builds on Spec 02 and the license rules on Spec 03.

| Role | What it is | Examples | Stored in | Shown as a result | Counts as a teacher | In the dana split |
| --- | --- | --- | --- | --- | --- | --- |
| Corpus text | A specific edition of a work, split into locatable units | Platform Sutra, Dhammapada, a Genjokoan translation | `text_unit` | Yes, in the Texts section only | No | No |
| Reference asset | Input that improves the pipeline | Glossaries, alias lists, lineage charts, locator lists, chant metadata | `reference_asset`, plus the rows it produces in `glossary_term`, `work_locator`, `person` and `lineage_edge` | Never | No | No |

Rules:

1. A text is never a teacher. The `teacher` registry holds people whose talks are in the corpus. An author such as Huineng or Dogen is a `person`, and a work with an unknown or collective author is attributed to the work itself. Single-author attribution is displayed but never counts toward the three-teacher rule and never earns a give button.
2. Rights attach to the edition, not the work. A public-domain text can have a copyrighted translation, so each edition needs its own license evidence, with the translator named.
3. The gate is the same as for talks. An edition is indexed only when its license evidence is approved by a person and its license code is on the allowed list. Otherwise the work stays a link-out in the `work` table, which keeps citation resolution working without storing text.
4. Texts and talks never mix in retrieval. They live in separate tables behind separate serving views and are searched separately. The answer assembler combines them only as two distinct sections, so a text cannot change which talks are returned or in what order.
5. Reference assets are never embedded and never displayed. They exist only to produce rows that other stages read.
6. A third-party reference asset needs license evidence like any other source. Facts the founder compiles personally, such as an alias list, do not.
7. The noncommercial conditions in Spec 03 apply to texts as well, including license display with each shown unit.

## Schema

Two new migrations: `0009_texts_reference.sql` for objects and `0010_texts_grants.sql` for grants. Everything here follows the conventions in Spec 02.

```sql
create type text_state as enum ('imported','ready','quarantined','withdrawn');
create type reference_kind as enum ('glossary','alias_list','lineage','locator_list','chant_metadata','other');

create table person (
  id uuid primary key default gen_random_uuid(),
  display_name text not null unique,
  kind text not null check (kind in ('author','translator','compiler','figure','teacher')),
  tradition text,
  era_note text,                         -- free text, for example '13th century'
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);
create table person_alias (
  id uuid primary key default gen_random_uuid(),
  person_id uuid not null references person(id) on delete cascade,
  alias text not null
);
create unique index person_alias_lower_uq on person_alias (lower(alias));

alter table teacher add column person_id uuid references person(id);

-- license evidence now also covers publishers of texts and reference assets
alter table license_evidence alter column sangha_id drop not null;
alter table license_evidence add column publisher text;
alter table license_evidence add constraint license_evidence_owner
  check (sangha_id is not null or publisher is not null);

-- work.excerptable from Spec 02 stays unused; the edition-level flag below governs
alter table work add column author_person_id uuid references person(id);
alter table work add column tradition text;

create table text_edition (
  id uuid primary key default gen_random_uuid(),
  work_id uuid not null references work(id),
  edition_label text not null,           -- for example 'Muller 1881'
  translator_person_id uuid references person(id),
  language text not null default 'en',
  source_url text,
  rights_holder text,
  license_evidence_id uuid references license_evidence(id),
  excerptable boolean not null default false,
  state text_state not null default 'imported',
  content_hash text,
  imported_at timestamptz not null default now(),
  unique (work_id, edition_label),
  constraint ready_requires_license check (
    state <> 'ready'
    or (excerptable and license_evidence_id is not null and rights_holder is not null))
);

create table text_unit (
  id uuid primary key default gen_random_uuid(),
  edition_id uuid not null references text_edition(id) on delete cascade,
  ordinal integer not null,
  locator text not null,                 -- canonical, for example 'dhammapada:1'
  label text not null,                   -- display, for example 'Dhammapada, verse 1'
  heading text,
  text text not null check (char_length(text) between 10 and 2500),
  word_count integer not null,
  content_hash text not null,
  tsv tsvector generated always as (to_tsvector('english', text)) stored,
  embedding vector(1024) not null,
  embedding_model text not null,
  unique (edition_id, ordinal),
  unique (edition_id, locator)
);
create index text_unit_tsv_gin on text_unit using gin (tsv);
create index text_unit_embedding_hnsw on text_unit
  using hnsw (embedding vector_cosine_ops) with (m = 16, ef_construction = 64);

create table reference_asset (
  id uuid primary key default gen_random_uuid(),
  kind reference_kind not null,
  title text not null,
  source_url text,
  license_evidence_id uuid references license_evidence(id),
  s3_key text not null,
  sha256 text not null unique,
  row_counts jsonb not null default '{}'::jsonb,
  imported_by text not null,
  imported_at timestamptz not null default now()
);
alter table glossary_term add column source_asset_id uuid references reference_asset(id);
alter table work_locator add column source_asset_id uuid references reference_asset(id);

create table lineage_edge (
  id uuid primary key default gen_random_uuid(),
  parent_person_id uuid not null references person(id),
  child_person_id uuid not null references person(id),
  kind text not null check (kind in ('teacher_student','transmission','founder')),
  source_asset_id uuid references reference_asset(id),
  note text,
  check (parent_person_id <> child_person_id),
  unique (parent_person_id, child_person_id, kind)
);

create table import_run (                 -- audit of every importer run
  id uuid primary key default gen_random_uuid(),
  manifest_path text not null,
  role text not null check (role in ('corpus','reference')),
  dry_run boolean not null,
  counts jsonb not null,
  actor text not null,
  created_at timestamptz not null default now()
);

alter table citation add column text_unit_id uuid references text_unit(id) on delete set null;

create view servable_text_unit with (security_barrier = true) as
select u.id as text_unit_id, u.text, u.locator, u.label, u.ordinal, u.embedding, u.tsv,
       e.id as edition_id, e.edition_label, e.language,
       w.id as work_id, w.title as work_title, w.link_out_url as work_url,
       ap.id as author_id, ap.display_name as author_name,
       tp.display_name as translator_name,
       le.license_code, le.license_url
from text_unit u
join text_edition e on e.id = u.edition_id
join work w on w.id = e.work_id
left join person ap on ap.id = w.author_person_id
left join person tp on tp.id = e.translator_person_id
join license_evidence le on le.id = e.license_evidence_id
where e.state = 'ready' and e.excerptable and le.allowed;

create or replace view servable_citation with (security_barrier = true) as
select c.id as citation_id, c.passage_id, c.span_start, c.span_end, c.raw_text, c.confidence,
       wl.locator, wl.label, wl.link_out_url as locator_url,
       w.id as work_id, w.title as work_title, w.excerptable,
       stu.text_unit_id
from citation c
join servable_passage sp on sp.passage_id = c.passage_id
left join work_locator wl on wl.id = c.work_locator_id
left join work w on w.id = wl.work_id
left join servable_text_unit stu on stu.text_unit_id = c.text_unit_id;
```

Grants in `0010`: `dg_pipeline` gets read and write on every new table. `dg_web` gets select on `servable_text_unit`, `person` and `person_alias` and nothing on `text_unit`, `text_edition` or `reference_asset`. `dg_analyst` gets no access to the new tables.

Because `license_evidence` now serves texts, the default `allowed_license_codes` in Spec 03 also includes `public-domain`. For a public-domain edition, the evidence is the page that supports the claim, such as the translator's death date or the publisher's public-domain statement, captured with `wt license capture` and approved by a person like any other evidence.

## Import pipeline

Every import is driven by a manifest, a small TOML file that sits beside the source files. Two commands read manifests, and both run as a dry run unless given `--apply`.

- `wt text import --manifest <path> [--apply]` for corpus texts.
- `wt reference import --manifest <path> [--apply]` for reference assets.

A corpus text manifest:

```toml
role = "corpus"

[work]
title = "Dhammapada"
author = "REPLACE_ME"              # a person display name, or omit for an anonymous work
tradition = "theravada"

[edition]
label = "REPLACE_ME 1881"
translator = "REPLACE_ME"
language = "en"
source_url = "https://REPLACE_ME"
rights_holder = "public domain"    # or the translator or publisher
license_evidence_id = "REPLACE_ME" # from wt license capture and approve

[source]
files = ["dhammapada.epub"]
format = "epub"                    # epub | html | txt | md | pdf
include_sections = ["Chapter*"]    # glob on section titles

[units]
scheme = "numbered"                # verse | numbered | paragraph | heading
locator_prefix = "dhammapada"
number_pattern = '^\s*(\d+)\.\s'
label_template = "{work}, verse {n}"
max_words = 220
```

A reference asset manifest:

```toml
role = "reference"

[asset]
kind = "glossary"                  # glossary | alias_list | lineage | locator_list | chant_metadata
title = "REPLACE_ME"
source_url = "https://REPLACE_ME"
license_evidence_id = "REPLACE_ME" # omit for material the founder compiled personally

[source]
files = ["terms.csv"]
format = "csv"
columns = { canonical = "term", variants = "mishearings", kind = "type" }
```

Steps for a corpus text:

1. Validate the manifest. Refuse if the license evidence is missing or not `allowed`, if its code is not on the allowed list, or if `rights_holder` is blank. The dry run then prints a report of sections kept and excluded, unit count, the first and last three units, length statistics, locator collisions and warnings.
2. Extract. Read epub through the `epub` crate in spine order, html through `scraper`, and txt or md directly. Strip markup, footnote markers, page numbers and running headers. Normalize to Unicode NFKC and tidy whitespace and quotes, keeping diacritics.
3. Filter sections. By default exclude sections whose titles match introduction, preface, foreword, translator, notes, commentary, glossary, index, bibliography, appendix or about. Translator front matter often carries its own copyright even when the text itself is public domain, so `include_sections` is the only way back in.
4. Cut units by scheme: `verse` splits on blank-line stanzas, `numbered` on the number pattern, `paragraph` on blank lines, `heading` takes each heading's body. A unit longer than `max_words` splits at sentence boundaries into `locator.a`, `locator.b` and so on. A fragment under 5 words merges into its neighbor.
5. Assign locators as `<prefix>:<n>`, or `<prefix>:<chapter>:<n>` when the scheme has chapters. A duplicate locator fails the import.
6. In one transaction, upsert `person`, `work` and `text_edition`, replace the edition's units with new rows and embeddings from the same `Embedder` that indexes talks, set `excerptable` from the evidence, and mark the edition `ready`. Upload the original source files to S3 under `texts-source/<work>/<edition>/` with their SHA-256 so the import can be repeated. Write an `import_run` row.
7. Sync locators. Upsert a `work_locator` for every unit, with aliases from an optional `[aliases]` table in the manifest, so the citation resolver can match them. Then `wt reindex --citations` reruns resolution over talks and sets `citation.text_unit_id` where a resolved locator maps to a unit in a ready, excerptable edition.

An import is idempotent. The key is the SHA-256 of the source files plus the manifest hash, and an unchanged key makes the run a no-op.

Reference assets feed the pipeline and nothing else:

- A glossary file becomes `glossary_term` rows with `source_asset_id`. New variants merge into an existing canonical term. A conflict in canonical form or kind is reported and never overwritten silently. The glossary version hash then changes and the existing mechanism requeues the affected talks.
- An alias list becomes `person` and `person_alias` rows. These are not teacher aliases, which come only from `wt registry import`.
- A lineage file becomes `person` rows and `lineage_edge` rows. Teachers link to a person only when the display names match exactly or `--link-teachers` is given.
- A locator list becomes `work` and `work_locator` rows, the hand-listed citation locators of Spec 04.
- A chant metadata file is stored in S3 and recorded in `reference_asset` with no downstream table in v1, since the PRD puts chant tooling in the backlog.

What these assets change: the glossary prompt for ASR and the glossary correction pass read the glossary rows, the resolver reads locator aliases, and the extraction prompt receives person names to recognize. Lineage edges are used only to validate the registry and for future context cards. Lineage never enters ranking.

## PDFs and OCR

PDFs with a text layer are read directly. Scanned PDFs are run through OCR and then proofread by a person before the edition can be served, because an OCR error in a text unit shows up as a misquotation of the work. Set `format = "pdf"` in the manifest and add a `[pdf]` table.

```toml
[pdf]
ocr = "auto"             # auto | always | never
languages = ["eng"]
dpi = 300
columns = "auto"         # auto | 1 | 2
skip_pages = ["1-6"]     # front matter, plates, blank pages
strip_running_heads = true
vision_pass = "flagged"  # off | flagged
min_layer_chars = 200
min_unit_confidence = 0.80
min_dictionary_share = 0.85
max_suspect_share = 0.10
```

The importer calls `pdftotext` and `pdftoppm` from poppler-utils and Tesseract 5 with its `eng` data as subprocesses. They are installed in the batch host image and through the local package manager (apt in WSL 2, or winget or Scoop on Windows). No Python is needed.

Steps for a PDF edition:

1. Detect per page. Run `pdftotext` on each page. A page with fewer than `min_layer_chars` characters, or a high share of non-letter symbols, goes to OCR. In `auto` mode a mixed PDF is handled page by page.
2. Render and OCR. Render the page to PNG at `dpi` in scratch, then run Tesseract with `--oem 1` and automatic page segmentation, asking for TSV output so each word has a confidence and each block has geometry. Deskew when the detected angle is over 0.5 degrees.
3. Rebuild the text. Join lines into paragraphs, remove hyphenation at line breaks, drop page numbers and running heads (lines repeated at the same position on many pages), fold ligatures and the long s, and keep diacritics in NFC. Blocks in clearly smaller type at the foot of a page are treated as footnotes and excluded. The dry run lists every excluded block so you can check the heuristic.
4. Flag suspect units. A unit is `suspect` when its mean word confidence is under `min_unit_confidence`, or when fewer than `min_dictionary_share` of its tokens appear in the English word list plus the glossary. If more than `max_suspect_share` of the units are suspect, the import stops with a report, since the scan probably needs better settings or a better copy.
5. Optional vision pass. With `vision_pass = "flagged"`, each page holding a suspect unit is sent as an image to the Claude API under a strict prompt: transcribe exactly, do not correct or modernize, keep diacritics, mark illegible text as `[?]`. This runs under a new `DataClass::PublicText`, which requires approved license evidence for the edition and does not need `WT_LLM_REAL_DATA_OK`, since the text is openly licensed and public. The vision text replaces the OCR text for a page only when the two agree at a character-level similarity of at least 0.75, which guards against a model inventing text. Otherwise the page keeps its OCR text and goes to the proofread.
6. Record the origin. `text_edition.extraction` becomes `text_layer`, `ocr` or `ocr_vision`, with the engine and mean confidence. Each unit keeps its `source_page` and `confidence`, and the page shows the page number when it is known.

The proofread gate:

1. `wt text proof --edition <id>` writes a standalone HTML proof sheet to `data-inbox/_proof/`. For each sampled unit it shows the extracted text beside a crop of the page image, with controls for correct or error and a note field. The sample is 24 units: 12 chosen at random across the book, with at least one from each tenth of the pages where the book is long enough, and the 12 with the lowest OCR confidence. A native or text-layer edition uses 10 random units. Reading a unit against its page crop takes a few seconds, so a normal edition takes about ten minutes.
2. `wt text proof-import --edition <id> --file proof.json --reviewer <name>` records a `proof_check` row. An error is any wrong, missing or extra word in a unit. The edition passes when the random 12 hold at most `proof_random_max_errors` errors (default 1) and the 12 lowest-confidence units hold at most `proof_worst_max_errors` (default 3). The counts in `proof_check` are the number sampled and the number of errors in each group.
3. Until a passing check exists the edition stays `imported` and does not serve. The serving view enforces this.
4. After a failure, the report lists the error patterns. The options are to change the OCR settings, run the vision pass, or correct units with `wt text fix --unit <id> --text "<text>"`, which writes to `correction_log`. Then run the proofread again.

Page images are working files in scratch and on the proof sheet only, and are never stored in the database. Migration `0011_ocr_proof.sql`:

```sql
alter table text_edition add column extraction text not null default 'native'
  check (extraction in ('native','text_layer','ocr','ocr_vision'));
alter table text_edition add column ocr_engine text;
alter table text_edition add column mean_confidence real;
alter table text_unit add column source_page integer;
alter table text_unit add column confidence real;

create table proof_check (
  id uuid primary key default gen_random_uuid(),
  edition_id uuid not null references text_edition(id) on delete cascade,
  sample_size integer not null,
  errors integer not null,
  reviewer text not null,
  passed boolean not null,
  notes text,
  created_at timestamptz not null default now()
);

create or replace view servable_text_unit with (security_barrier = true) as
select u.id as text_unit_id, u.text, u.locator, u.label, u.ordinal, u.embedding, u.tsv,
       e.id as edition_id, e.edition_label, e.language,
       w.id as work_id, w.title as work_title, w.link_out_url as work_url,
       ap.id as author_id, ap.display_name as author_name,
       tp.display_name as translator_name,
       le.license_code, le.license_url,
       u.source_page
from text_unit u
join text_edition e on e.id = u.edition_id
join work w on w.id = e.work_id
left join person ap on ap.id = w.author_person_id
left join person tp on tp.id = e.translator_person_id
join license_evidence le on le.id = e.license_evidence_id
where e.state = 'ready' and e.excerptable and le.allowed
  and exists (select 1 from proof_check pc where pc.edition_id = e.id and pc.passed);
```

The grants in `0010` extend to `proof_check` for `dg_pipeline` only.

## Large documents

A 2,500-page scan is a batch job, not a single import. These rules keep it resumable, bounded and checkable.

1. Look for existing text first. Before OCR, check whether the work already has a born-digital edition, and let `wt intake` record the answer. Internet Archive items usually carry their own OCR text, and many open texts exist as epub or html. An existing text layer or archive OCR text can be imported as `text_layer`, and it goes through the same quality checks and proofreading. OCR the scan yourself only when nothing usable exists.
2. Chunked jobs. The importer splits the PDF into page ranges of `pdf_chunk_pages` (default 50) and queues one `ocr_chunk` job per range on the same queue as talks, with idempotency key `ocr:<edition>:<pdf sha256>:<first>-<last>`. Each job renders and OCRs its pages one at a time, writes the results (text, word confidences and block geometry, never images) to S3 under `texts-work/<edition>/pages/`, and deletes its images. A final `text_assemble` job runs when every chunk is done. A crash or restart reruns only the unfinished chunks, and workers on several machines can share the queue. Run `wt worker --kinds ocr_chunk --concurrency <cores>`.
3. Estimate before committing. `wt text import --estimate` samples the first pages and prints total pages, pages needing OCR, measured pages per minute, projected wall time at the chosen concurrency, and the worst-case number of vision calls. Nothing is queued until you add `--apply`.
4. Bound the vision pass. `vision_max_pages` (default 200) caps how many pages go to the vision pass. Flagged pages beyond the cap are ordered by lowest confidence, and the ones past the cap go straight to proofreading.
5. Split compilations. A manifest can list several parts with page ranges, such as separate volumes or texts bound together. Each part becomes its own work or edition, with its own locators, license evidence line and proofread.
6. Check the whole book automatically. Scan quality often drifts across a long book, so the importer measures each tenth of the pages. A range whose mean OCR confidence is below `range_min_confidence` (default 0.85) is re-read once at 400 dpi, and if it is still below, you spot-check 6 units from that range only. `wt text reocr --edition <id> --pages <a-b> --dpi 400` also redoes a range by hand.
7. Memory and disk. Pages are processed one at a time and the document is never loaded whole. Scratch needs room for one chunk of page images only.
8. Hardware. OCR is CPU work and needs no GPU. Wall time scales with pages divided by cores, so use the estimate instead of guessing.

Acceptance criteria for large documents:

- [ ] A synthetic 300-page PDF split at 50 pages per chunk queues 6 jobs, completes with 3 workers, and assembles to text identical to a single-pass run.
- [ ] Killing a worker mid-chunk and restarting reruns only the unfinished chunks.
- [ ] `--estimate` queues nothing and prints every figure listed above.
- [ ] Flagged pages beyond `vision_max_pages` skip the vision pass and go to proofreading, lowest confidence first.
- [ ] A manifest with two parts creates two editions with separate locators and separate proofread requirements.
- [ ] A synthetic scan with one badly degraded page range fails the per-range check and names it, and `reocr` on that range then passes.
- [ ] Peak scratch usage during a run stays within one chunk of images.

## Retrieval and answer changes

Texts get their own retrieval path and their own section on the page. The talk path in Spec 05 is untouched.

1. Text search. `wt-retrieve` gains a `TextRetriever` that runs the same hybrid pipeline (full-text, vector, fusion, rerank) over `servable_text_unit` and nothing else. Its settings sit under `[texts]` in `config/retrieval.toml`: `fts_top_k = 20`, `vector_top_k = 20`, `fuse_top_k = 15`, `rerank_min_score = 0.40` (calibrate on the evaluation set), `max_units = 4`, `max_works = 3`, `per_work_cap = 2`.
2. When it runs. It runs in standard mode in parallel with talk retrieval, and in named-author mode. It does not run in named-teacher mode, where the page is about one teacher.
3. Named-author mode. A second Aho-Corasick automaton covers `person_alias` for people who are authors of at least one ready work. A query that matches exactly one such author and no teacher alias restricts text search to that author's works. Talk retrieval still runs in standard mode with the author's name removed from the query, so "what does Dogen say about time" returns Dogen's text units plus teachers who speak to time. A teacher match always takes precedence, and the usual alias length and common-word rules apply.
4. Logging. Add to migration `0009`: `alter table query_log add column named_author_id uuid references person(id); alter table query_log add column text_count integer not null default 0;`.
5. No crossover. Text results never enter teacher selection, the teacher count, the source ceiling or the dana split. They are merged only at assembly, as a separate section.

```rust
pub struct TextHit {
    pub unit_id: TextUnitId, pub text: String, pub label: String,
    pub work: WorkRef, pub author: Option<PersonRef>, pub translator: Option<String>,
    pub edition: String, pub license: LicenseNotice,   // name and link
    pub link: String,                                  // /text/<unit_id>
}
```

`AnswerView` gains `texts: Vec<TextHit>`. The page shows them in a section titled "From the texts" after the teacher groups, each as a blockquote with work, locator, author, translator, edition and license notice. A text hit has no give button. The result line counts them apart: "Passages from 3 teachers, and 2 text excerpts".

Footnotes. When a citation has `text_unit_id`, its footnote links to `/text/<unit_id>` as "Read in the text". Otherwise it renders as before, with a link-out when the locator has a URL.

New route: `GET /text/{unit_id}` (session required) shows the unit with at most two neighbors on each side, read through `servable_text_unit`, each with its license notice. There is no whole-work view, in keeping with the PRD's short-passages rule.

Texts are not passed to the synthesizer in v1, because themes describe how teachers approach a question. Evaluation exports list text hits in their own labeled block so the reviewer can rate them separately.

## Guided loading

All files you collect go into one gitignored folder at the repository root, `data-inbox/`, with one subfolder per edition or asset.

```text
data-inbox/
  texts/<work-slug>/         source files and manifest.toml for one edition
  reference/<asset-slug>/    source files and manifest.toml for one asset
  license-evidence/          optional saved copies of license pages, named by source
  _done/                     applied folders are moved here
```

After `--apply`, the importer uploads the originals to S3 (`texts-source/` or `reference-assets/`) with their hashes and moves the folder to `_done/`, so the local copy can be deleted. Nothing in `data-inbox/` is ever committed. `.gitignore` excludes it and a guard test fails if git tracks any file under it.

`wt intake` is an interactive walkthrough that builds the manifests so you never write TOML by hand.

1. It scans `data-inbox/` for folders with no manifest or an incomplete one, and lists each file with its size and the first lines of its extracted text.
2. For each folder it asks in order whether the role is corpus or reference. For a corpus text it asks for the work title, author, tradition, edition label, translator and language. For a reference asset it asks for the kind and title.
3. It asks where the license statement lives. It runs the capture step from Spec 03, shows you the saved page, and records your approval only when you type your name as reviewer. For a public-domain claim it asks for the basis, such as the translator's death year or the publisher's own statement. With no evidence the folder is marked blocked, the walkthrough says exactly what is missing, and nothing proceeds.
4. It proposes a unit scheme by inspecting the text (numbered verses, paragraphs or headings), shows five sample units, and asks you to confirm or adjust.
5. It writes `manifest.toml`, runs the dry run, shows the report, and asks whether to apply.
6. After applying, it prints a summary: units loaded, locators synced, citations relinked and where the originals went.

`wt intake --manifest-only` writes manifests with `REPLACE_ME` fields for you to fill later. The walkthrough never makes a legal judgment. It records evidence, requires a person to approve it, and defaults to blocked.

What may be loaded through this path:

- Only editions and assets with approved evidence. A restricted translation is not loaded. `wt text link-out --work <title> --url <url> [--locators <file>]` creates the `work` and its locators with no text, so talks can still cite it and the footnote links out.
- No talks. Audio or transcripts of talks go through the ingestion path in Spec 03, because they carry the consent and licensing rules for teachers.
- Nothing from a paywalled source, and nothing whose terms forbid compiling or redistributing it.

## Ingesting from URLs

You can hand the importer web addresses instead of files. It fetches each document, converts it to text, cuts units, embeds them and loads them, using the same steps as a manifest import.

Commands:

- `wt text fetch --url <url> [--url <url>]` or `wt text fetch --urls-file urls.txt`, with one URL per line and `#` comments allowed.
- `wt text fetch --crawl <index-url> --depth 1 --follow-pattern '<regex>' --max-pages 50` for an index page that lists documents.
- `wt text refresh --edition <id>` to refetch and detect changes.
- `wt license record-permission` to record a permission (below).

Permission as evidence. Evidence for a text can be a license the source states, which is preferred, or a recorded permission from an organization.

`wt license record-permission --grantor "<organization>" --scope-urls <prefix> [--scope-urls <prefix>] --covers "excerpting with attribution" --statement-file permission.txt --given-on <date> --recorded-by <name>`

This saves the statement to S3 (an email pasted into a text file is enough) and creates a `license_evidence` row of kind `permission`, code `permission-excerpt`, listing the URL prefixes it covers. A person then approves it with `wt license approve`, as for any evidence. A permission is only as broad as what the grantor can authorize. It covers works the grantor owns or republishes with rights, and it does not cover a third party's translation just because the grantor links to it. The dry run therefore shows the host, the grantor and the scope the evidence claims, and each edition keeps its own `rights_holder`.

The gate. A URL must fall under the prefixes of approved evidence before the importer downloads its body. For any other URL the importer sends only a `HEAD` request, reports the content type and size, says that no evidence covers the URL, and stops. The importer also looks for license signs on the page, such as `rel="license"` links, Creative Commons footer links and PDF rights metadata, and lists them as hints in the report. Hints never approve anything.

Fetching rules:

1. Politeness. The `User-Agent` names the project and a contact address. There is at least `fetch_delay_ms` (default 1000) between requests to one host, with one connection per host. The fetcher obeys `robots.txt`. A disallowed URL is skipped and reported, and there is no override flag. If a site owner wants it fetched, the owner changes the file.
2. Safety. Only `http` and `https`. Addresses are checked after DNS resolution and again on every redirect, and private, loopback and link-local ranges are blocked. At most 5 redirects, a 30-second timeout, and `max_download_mb` (default 200). Requests are anonymous with no cookies, logins or paywalled pages. JavaScript is not run in v1, and an extraction that comes back nearly empty is reported as such.
3. Documents only. Audio and video content types are refused, since talks go through the path in Spec 03.
4. Raw copy. The downloaded bytes are saved to S3 under `texts-source/<host>/<sha256>` with a sidecar holding the URL, final URL, fetch time, status, content type, ETag, Last-Modified and hash. Refetches use conditional requests.

Conversion, chosen by content type and file signature rather than extension:

- HTML. A readability-style extractor picks the main content from `article`, `main` or the densest text block, strips navigation, scripts, comments and footers, and keeps headings and paragraph breaks.
- PDF. The PDF and OCR path above, with chunked jobs for large files.
- DOCX. Unzip and read `word/document.xml`, keeping paragraphs and heading styles. Footnotes are excluded by default.
- DOC, RTF and ODT. Converted to DOCX with LibreOffice in headless mode, on the batch host or locally, and then handled as DOCX.
- EPUB, TXT and MD. As in the manifest path.
- Anything else is reported and skipped.

The manifest draft. The importer drafts the manifest from what it finds. Title comes from `og:title`, `<title>` or PDF metadata. Author comes from `author` or `citation_author` meta tags or PDF metadata. The edition label comes from the publication date or the host, `source_url` from the final URL, and the rights holder from the evidence's grantor unless the page says otherwise. The translator is never guessed, so it is asked. The unit scheme is sniffed and shown with five sample units, and `wt intake --url <url>` walks you through confirming it. With `--yes`, a born-digital HTML page that has a clear title uses the paragraph scheme with no questions. Anything that needs OCR, or whose extraction looks suspect, still stops for confirmation, and OCR editions still need the proofread.

Source links. Each unit stores a `source_anchor`, and the page shows a link back to the original beside the license or permission notice. For HTML the link is the page URL with a text fragment (`#:~:text=`) built from the unit's opening words, so a browser highlights the excerpt. For a PDF it is `#page=N`. For a permission, the notice reads "Used with permission of \<grantor>" instead of a license link.

Refreshing. `wt text refresh --edition <id>` makes a conditional request. An unchanged document is a no-op. A changed one is imported as a new version with a diff report of units added, removed and changed, and serving continues with the current edition until you approve the replacement with `--apply`, with a new proofread if it needed OCR. The nightly audit gains a warning for any source URL that is unreachable or whose content changed.

Schema, migration `0012_url_ingest.sql`:

```sql
alter table license_evidence add column kind text not null default 'license'
  check (kind in ('license','permission'));
alter table license_evidence add column grantor text;
alter table license_evidence add column url_prefixes text[] not null default '{}';
alter table license_evidence alter column license_url drop not null;
alter table license_evidence alter column evidence_url drop not null;
alter table license_evidence add constraint permission_has_scope
  check (kind <> 'permission' or (grantor is not null and cardinality(url_prefixes) > 0));

alter table text_unit add column source_anchor text;
alter table text_edition add column source_sha256 text;
alter table text_edition add column fetched_at timestamptz;
alter table text_edition add column source_etag text;
```

Acceptance criteria for URL ingestion:

- [ ] A URL outside every approved prefix gets a `HEAD` request only, with a report and no body downloaded or stored. A URL inside a prefix is downloaded, stored in S3 with its sidecar and converted.
- [ ] A URL disallowed by `robots.txt` is skipped and reported, and no flag overrides it.
- [ ] Loopback, private and link-local addresses are blocked, including through a redirect.
- [ ] Audio and video content types are refused.
- [ ] A PDF served under an `.html` name is detected as a PDF by its signature.
- [ ] Fixtures convert correctly: an HTML page loses its navigation and footer, a DOCX keeps headings and paragraphs, and an EPUB works. DOC and RTF conversion through LibreOffice runs nightly where it is installed.
- [ ] A crawl of an index fixture lists links, applies the pattern and the prefix scope, honors `--max-pages`, and requires a selection before importing.
- [ ] The manifest draft takes title and author from meta tags, never guesses a translator, and `--yes` applies only to born-digital HTML.
- [ ] An HTML unit's source link carries a text fragment, and a PDF unit's carries a page number.
- [ ] A permission record without a grantor or URL prefixes violates the database constraint, needs a person's approval before use, and renders "Used with permission of" on the page.
- [ ] Refresh is a no-op for an unchanged document. A changed one produces a diff and does not replace the served edition until approved.
- [ ] A local test server confirms the `User-Agent`, the per-host delay and the connection limit.

## Source policy for URL ingestion

Evidence follows rights, not hosts. Free to download and the absence of takedown notices are not evidence. The first says a host distributes a file. The second says nobody who noticed has objected. Neither says the rights holder permits indexing and excerpting. Two rules refine the URL gate.

1. Aggregator hosts. A site that republishes works by many rights holders is an aggregator. `config/pipeline.toml` lists them under `[url_policy] aggregator_hosts`, starting with `terebess.hu`. Evidence cannot cover an aggregator with a host-wide prefix, except through a mandate (see Mandated sources). Each document needs its own evidence naming its rights holder, and the host serves as a discovery index only.
2. Notice scan. Before conversion the importer scans the first and last pages and the document metadata for copyright notices, such as "Copyright", the copyright sign or "All rights reserved". If a notice names a rights holder that the evidence does not cover, the document is blocked as `third_party_notice` and listed in the report. A grantor's evidence never overrides a notice naming someone else. Under a mandate, what happens to a noticed document is set by the source's notice\_action setting.

Source assessments, read October 2, 2026:

| Source | What the pages say | Status | Next step |
| --- | --- | --- | --- |
| BDK America, https://www.bdkamerica.org/tripitaka-list/ | The homepage banner says most publications are free as PDFs. Product pages offer a free PDF download, for example Shōbōgenzō Volume I. The footer carries a 2026 BDK America copyright line, and no license statement appeared on the homepage, the Tripiṭaka list or the Shōbōgenzō page. The Shōbōgenzō page says BDK reprints the Nishijima and Cross translation first published by Windbell Publications in 1996 to 1999. The list includes Shōbōgenzō (four volumes), Blue Cliff Record, Platform Sutra, Baizhang Zen Monastic Regulations, Three Chan Classics and Zen Texts. | Needs written permission. Free PDFs are distribution, not a license. Rights may sit with BDK, the translators or the original publisher. | Ask BDK America through its contact page for permission to excerpt named volumes with attribution, and ask who holds rights in the Shōbōgenzō translation. |
| Terebess, https://terebess.hu/zen/ | The pages read carry no site-wide rights statement. Individual pages carry notices of many holders: a full Thomas Cleary book PDF marked with his 1994 copyright and a no-reproduction line, a Gregory Wonderwheel translation marked 2007 to 2008, and Gábor Terebess's own pages marked with his name. | Aggregator. Mixed rights, so per-document evidence only. | Use it to find texts, then verify each document's rights holder. Public-domain translations and works the site owner wrote can be cleared individually, and Gábor Terebess can grant permission for his own pages. |

Acceptance criteria for the source policy:

- [ ] Recording evidence with a host-wide prefix on an aggregator host is rejected.
- [ ] A fixture PDF whose first page carries a copyright notice naming another party is blocked as `third_party_notice`, and evidence naming that holder unblocks it.
- [ ] The notice scan covers HTML, PDF and DOCX, reading metadata and the first and last pages.

## Mandated sources

The project owner has decided to use BDK America and Terebess in full unless a rights holder says otherwise, with removal made as easy as possible. A mandate is that decision, recorded as what it is: the owner's choice to rely on a source, not a license and not a permission. Two rules follow from that, and the second is not configurable.

1. Reversible in seconds. Every mandated source has an `enabled` flag, and turning it off removes the source from every answer immediately.
2. Honest labeling. The product never says or implies that a mandated source granted permission or a license. An excerpt shows the work, translator, publisher and a link back to the source, plus the attribution line stored with the mandate. The words permission and licensed never appear on a mandated excerpt.

How it works:

- `wt source mandate --key <key> --publisher "<name>" --prefix <url> [--prefix <url>] --by "<your name>" --rationale "<text>" --display-notice "<line>"` records the owner's decision. It creates a `license_evidence` row of kind `mandate`, code `operator-mandate`, approved by the named person, and a `source_policy` row. The display notice may use `{title}`, `{translator}`, `{publisher}` and `{url}`, for example "Excerpted with attribution from {title}, {publisher}. Free to download at the source: {url}". The two starting keys are `bdkamerica` (prefixes `https://www.bdkamerica.org/` and `https://bdkamerica.org/`) and `terebess` (prefix `https://terebess.hu/`).
- A URL under a mandated prefix needs no per-URL evidence. The importer attaches the mandate and clears the edition. Everything else in the pipeline is unchanged, including `robots.txt` obedience, the polite fetch rules, OCR proofreading and the large-document jobs.
- Only English is loaded in v1. Language is detected per document and a non-English one is skipped and reported, which matters for a multilingual site like Terebess.
- To collect a mandated source, crawl its indexes in batches, for example `wt text fetch --crawl https://www.bdkamerica.org/tripitaka-list/ --depth 2 --max-pages 50` and `wt text fetch --crawl https://terebess.hu/zen/textindex.html --depth 2 --max-pages 50`. BDK's download links redirect to the PDF, and the fetcher follows them within the prefix.
- The notice scan from the source policy applies. Each source has `notice_action` of `flag`, `hold` or `block`, and under the mandate it starts as `flag`. A flagged document loads and appears in `wt source report` with its notice text, and `wt text quarantine --edition <id> --reason "<text>"` pulls any one document. `hold` quarantines noticed documents on arrival until released one at a time. `block` refuses them. Setting `hold` for explicit no-reproduction notices is the safer choice.
- `[deployment] exposure = "private"` is the default. With `exposure = "public"` and any mandated source enabled, startup logs a warning and the nightly audit warns, because the exposure of a mandated source grows once the tool is public. Nothing blocks.

Easy removal:

1. Switch off a source: `wt source disable <key> --reason "<text>"`, and `wt source enable <key>` to restore it. The serving view checks the flag, so the effect is immediate.
2. Delete a source, edition or URL prefix: `wt takedown --source <key>`, `--edition <id>` or `--url <prefix>`. This removes the text units, embeddings, S3 raw copies and work files, keeps a stub so a refetch does not recreate the document, and writes `takedown_log`.
3. A public "Rights and removal" page. `GET /rights` needs no session and says: "If you hold rights in any text shown here and want it removed, tell us and we will remove it." It has one short form, `POST /rights/request`, rate limited to 5 per hour per address, with a honeypot field and no CAPTCHA, that writes a `takedown_request` row and is read through the CLI. Email notification is in the backlog, under the name `WT_RIGHTS_NOTIFY_EMAIL`. Every text hit on the answer page carries a "Report this excerpt" link to the form with the excerpt prefilled. These are the only routes reachable without a session.
4. Handling. `wt takedown requests list|show|suspend|remove|decline`. The stated practice is to suspend the named source or document within one business day without argument, before verifying who is asking, and to restore it only if the requester turns out not to hold the rights. A suspension is reversible. A removal is permanent.
5. If BDK America or Terebess answers no to anything, disable and take down that scope at once. If either answers yes, record it with `wt license record-permission` and switch the source's `basis` from `mandate` to `permission`, so the page can then truthfully say so. The BDK request already drafted is the way to make that happen.

Migration `0013_mandate_rights.sql`:

```sql
alter table license_evidence drop constraint license_evidence_kind_check;
alter table license_evidence add constraint license_evidence_kind_check
  check (kind in ('license','permission','mandate'));
alter table license_evidence add column mandated_by text;
alter table license_evidence add column rationale text;
alter table license_evidence add column display_notice text;

create table source_policy (
  key text primary key,
  publisher text not null,
  url_prefixes text[] not null,
  basis text not null check (basis in ('license','permission','mandate')),
  license_evidence_id uuid references license_evidence(id),
  enabled boolean not null default true,
  notice_action text not null default 'flag' check (notice_action in ('flag','hold','block')),
  disabled_reason text,
  disabled_at timestamptz,
  updated_at timestamptz not null default now()
);
alter table text_edition add column source_key text references source_policy(key);

alter table takedown_log drop constraint takedown_log_scope_check;
alter table takedown_log add constraint takedown_log_scope_check
  check (scope in ('teacher','talk','edition','source'));
alter table takedown_log add column edition_id uuid;
alter table takedown_log add column source_key text;

create table takedown_request (
  id uuid primary key default gen_random_uuid(),
  received_at timestamptz not null default now(),
  requester_name text,
  requester_email text,
  organization text,
  target text not null,                  -- a URL, a title or an excerpt id
  message text,
  text_unit_id uuid,
  status text not null default 'open' check (status in ('open','suspended','removed','declined')),
  handled_by text,
  handled_at timestamptz,
  action_taken text
);

create or replace view servable_text_unit with (security_barrier = true) as
select u.id as text_unit_id, u.text, u.locator, u.label, u.ordinal, u.embedding, u.tsv,
       e.id as edition_id, e.edition_label, e.language,
       w.id as work_id, w.title as work_title, w.link_out_url as work_url,
       ap.id as author_id, ap.display_name as author_name,
       tp.display_name as translator_name,
       le.license_code, le.license_url,
       u.source_page,
       le.display_notice, le.kind as evidence_kind, u.source_anchor, e.source_url
from text_unit u
join text_edition e on e.id = u.edition_id
join work w on w.id = e.work_id
left join person ap on ap.id = w.author_person_id
left join person tp on tp.id = e.translator_person_id
join license_evidence le on le.id = e.license_evidence_id
where e.state = 'ready' and e.excerptable and le.allowed
  and exists (select 1 from proof_check pc where pc.edition_id = e.id and pc.passed)
  and (e.source_key is null
       or exists (select 1 from source_policy sp where sp.key = e.source_key and sp.enabled));
```

Grants: `dg_web` may insert into `takedown_request` and select `source_policy`, nothing more on the new tables. `dg_pipeline` has full access.

Acceptance criteria for mandated sources:

- [ ] `wt source mandate` creates the evidence and `source_policy` rows, and a URL under a mandated prefix is fetched and cleared with no per-URL evidence.
- [ ] `wt source disable` removes every unit of that source from `servable_text_unit` immediately, tested within one session before and after. `enable` restores them.
- [ ] `wt takedown --source` deletes units, embeddings, S3 raw copies and work files, writes `takedown_log`, and the `StoreRegistry` check finds nothing left.
- [ ] `GET /rights` works with no session, `POST /rights/request` stores a row, rate limits at 5 per hour per address and rejects a filled honeypot, and no other route is reachable without a session.
- [ ] Each text hit carries a "Report this excerpt" link whose form arrives prefilled.
- [ ] A rendered mandated excerpt shows the publisher, a source link and the stored attribution line, and a test asserts that the words permission and licensed are absent.
- [ ] A noticed document is flagged and listed under `flag`, quarantined on arrival under `hold`, and refused under `block`.
- [ ] A non-English document is skipped and reported.
- [ ] With `exposure = "public"` and a mandated source enabled, startup logs a warning and the audit warns. A disabled source with servable rows fails the audit, and a takedown request open for more than one business day warns.

## Rights lifecycle

The project's purpose is to spread the teachings. Its rule for sources is therefore to use them by default, to ask the rights holder for explicit permission, and to let any rights holder withdraw at any time. Every step is recorded, and a withdrawal is permanent. This section generalizes Mandated sources into a lifecycle that every source of texts follows.

The three steps:

1. Use by default. A new source is added with `wt source add --key <key> --publisher "<name>" --prefix <url> --by "<name>" --rationale "<text>" --display-notice "<line>"`, which is the mandate command with a general name and creates the same records. `[rights] default_basis_for_new_sources = "mandate"` is the setting. A source with a license or permission in hand is added with that basis instead.
2. Ask. Each mandated source gets a permission request, tracked in the ledger below. The request is the way an opt-out becomes an explicit record.
3. Record the answer. A grant is logged and the page begins to say so truthfully. A refusal or a takedown is logged and the source is excluded permanently. No reply is logged as no reply and is never treated as permission, so the source stays on its mandate and is labeled as such.

The ledger. Every step writes an append-only `rights_event`, so the history of each source can be read in order.

- `wt rights request --source <key> --to <address or contact URL> --channel email|form|other --draft-file <path>` records a drafted request. `wt rights sent --source <key> --on <date>` records that it went out and schedules a follow-up 14 days later.
- `wt rights grant --source <key> --statement-file <path> --from "<name>" --given-on <date> --scope "<text>"` stores the reply, creates permission evidence for the named prefixes, and switches the source's `basis` to `permission`. From then on the page says "Used with permission of \<publisher>".
- `wt rights decline --source <key> --statement-file <path>` logs the refusal, runs the takedown for the source, and adds the exclusions below.
- `wt rights ledger [--source <key>]` prints the timeline, and `wt rights due` lists requests that are unsent or past their follow-up date.

Never use it again. A takedown, a decline or a rights holder's removal request creates active rows in `source_exclusion` covering the source prefix, the work, and the SHA-256 of each removed source file and text unit. Every path that could bring text in checks them first: the fetcher, the crawler, `wt source add`, `wt text import`, `wt intake` and the unit writer. A URL, work, edition, person or content hash that matches an active exclusion is refused with the exclusion cited, including an identical document found at a different URL. In code, the fetch functions accept only a `FetchTarget`, a type that can be built only by `Exclusions::clear(url)`, so an unchecked URL cannot be fetched. An exclusion is lifted only by `wt exclusion lift --id <id> --by "<name>" --evidence <statement file>`, which requires the rights holder's own written grant and is itself logged.

Transparency. `GET /rights` also lists every source with its status, in plain words: used on the project's own decision with permission requested on a date, used with permission recorded on a date, or removed at the rights holder's request. It never uses permission language for a mandate.

This section covers texts. Talks follow Spec 03's Mandated talks section: they are used on the owner's mandate unless the teacher opts out, and each teacher receives a notice that asks for formal permission.

Migration `0014_rights_ledger.sql`:

```sql
create table rights_event (                 -- append-only
  id uuid primary key default gen_random_uuid(),
  source_key text not null references source_policy(key),
  event text not null check (event in (
    'mandated','request_drafted','request_sent','follow_up_sent','permission_granted',
    'permission_declined','no_response_noted','takedown_requested','suspended',
    'takedown_completed','exclusion_added','exclusion_lifted','enabled','disabled')),
  actor text not null,
  detail text,
  evidence_ref text,                        -- S3 key of a request, reply or statement
  created_at timestamptz not null default now()
);

create table permission_request (
  id uuid primary key default gen_random_uuid(),
  source_key text not null references source_policy(key),
  recipient text not null,
  channel text not null check (channel in ('email','form','other')),
  draft_ref text,
  sent_on date,
  follow_up_on date,
  status text not null default 'draft'
    check (status in ('draft','sent','replied','granted','declined','no_response')),
  reply_ref text,
  created_at timestamptz not null default now()
);

create table source_exclusion (
  id uuid primary key default gen_random_uuid(),
  scope text not null check (scope in ('host','prefix','work','edition','person','content')),
  pattern text not null,                    -- URL prefix, work title, edition id, person name or a SHA-256
  reason text not null,
  source_key text references source_policy(key),
  created_by text not null,
  created_at timestamptz not null default now(),
  lifted_at timestamptz,
  lifted_by text,
  lift_evidence_ref text
);
create index source_exclusion_active_idx on source_exclusion (scope, pattern) where lifted_at is null;

create view v_source_rights as
select sp.key, sp.publisher, sp.basis, sp.enabled,
       (select count(*) from source_exclusion x
         where x.source_key = sp.key and x.lifted_at is null) as active_exclusions,
       (select pr.status from permission_request pr
         where pr.source_key = sp.key order by pr.created_at desc limit 1) as latest_request_status,
       (select pr.sent_on from permission_request pr
         where pr.source_key = sp.key order by pr.created_at desc limit 1) as latest_request_sent_on
from source_policy sp;
```

A trigger makes `rights_event` append-only, as for `speaker_consent`. Grants: `dg_web` may select `v_source_rights` and nothing else here, and `dg_pipeline` has full access.

Audit additions: a source with `basis = 'mandate'` and no recorded request after 14 days warns, a request past its follow-up date warns, any servable text unit that matches an active exclusion fails the audit, and each refused attempt against an exclusion is logged for review.

Acceptance criteria for the rights lifecycle:

- [ ] `wt source add` with the default basis creates a mandate, the `source_policy` row and a `mandated` event.
- [ ] `wt rights request`, `sent`, `grant` and `decline` each write the right event and update `permission_request`, and `wt rights ledger` prints them in order.
- [ ] `grant` creates permission evidence, switches the basis, and the rendered excerpt then reads "Used with permission of" the publisher. Before the grant, a test confirms permission language is absent.
- [ ] `decline` and a takedown remove the units and raw copies and add exclusions for the prefix, the work and each content hash.
- [ ] After a takedown, `wt source add`, a direct fetch, a crawl and an import of the same prefix, the same work, and the same file served from a different URL are each refused with the exclusion cited.
- [ ] A compile-fail test shows the fetch functions cannot be called with a raw URL.
- [ ] `wt exclusion lift` without evidence fails, and with evidence logs the lift and allows the source again.
- [ ] A silent source stays a mandate, labeled as one, and `wt rights due` lists its follow-up.
- [ ] `GET /rights` lists each source with the correct plain-language status and uses no permission language for a mandate.
- [ ] The audit warns on a mandate with no request after 14 days, and fails on a servable unit that matches an active exclusion.

## Acceptance criteria

Tests use synthetic fixtures: a fake epub, a fake html page and a fake csv glossary written by `scripts/make_fixtures.py`. No real work is needed in the repository.

- [ ] Migrations `0009` and `0010` apply cleanly after `0008`, and the existing Spec 02 tests still pass.
- [ ] A `ready` edition without license evidence, without `excerptable`, or without a rights holder fails the `ready_requires_license` check.
- [ ] `servable_text_unit` returns nothing for an edition that is not `ready`, whose evidence is not `allowed`, or that is not excerptable.
- [ ] `dg_web` can select `servable_text_unit`, `person` and `person_alias`, and is denied on `text_unit`, `text_edition` and `reference_asset`.
- [ ] A dry run changes nothing in the database. An `--apply` run with unchanged files and manifest is a no-op.
- [ ] Section filtering excludes introduction, notes and translator material by default, and `include_sections` can restore a section.
- [ ] Each unit scheme (verse, numbered, paragraph, heading) produces the expected units and locators on its fixture. Over-long units split into `.a` and `.b`, and a duplicate locator fails the import.
- [ ] An import with evidence that is missing, not approved, or outside `allowed_license_codes` is refused, and a public-domain edition with approved evidence is accepted.
- [ ] A glossary import merges variants, reports a conflicting canonical form without overwriting it, and changes the glossary version so affected talks requeue.
- [ ] An alias list creates `person` rows and never creates teacher aliases. A lineage file creates edges, and `--link-teachers` links only exact name matches.
- [ ] After a locator sync and `wt reindex --citations`, a fixture talk citing a hand-listed locator gets `text_unit_id` set, and its footnote renders "Read in the text".
- [ ] Text hits never appear in the teacher count, the source ceiling or the dana split. The ranking invariance test passes with texts present.
- [ ] Named-author mode filters text search to that author, runs talk retrieval with the name removed, and a teacher match takes precedence. Named-teacher mode runs no text search.
- [ ] The page shows each text hit with work, locator, author, translator, edition and license notice, with no give button. `GET /text/{unit_id}` shows at most two neighbors on each side and never a whole work.
- [ ] `wt intake` on a folder with no evidence marks it blocked and applies nothing. With evidence and a typed reviewer name it completes the walkthrough end to end.
- [ ] `wt text link-out` creates a `work` and locators with no text rows.
- [ ] A guard test fails if git tracks any file under `data-inbox/`.

Acceptance criteria for PDFs and OCR, using a synthetic text-layer PDF, a synthetic scanned PDF rendered from known text with planted noise, and a mixed one:

- [ ] A page with a good text layer is read directly, and a page with no text layer goes to OCR, including inside one mixed PDF.
- [ ] On the synthetic scan, OCR output matches the known source text at a character error rate under 2 percent, measured in the test.
- [ ] Hyphenation at line breaks is removed, running heads and page numbers are dropped, and small-type foot blocks are excluded and listed in the dry run.
- [ ] Units under `min_unit_confidence` or under `min_dictionary_share` are flagged suspect, and an import with more than `max_suspect_share` suspect units stops with a report.
- [ ] The vision pass, tested with a fake client, replaces a page only at 0.75 similarity or higher, and a fake hallucinated page is rejected and left for proofreading.
- [ ] `DataClass::PublicText` is refused without approved license evidence for the edition.
- [ ] The proof sheet shows each sampled unit beside its page crop, the sample is 12 random and 12 lowest-confidence units, and `proof-import` computes pass or fail correctly from at most 1 error in the random 12 and at most 3 in the worst 12.
- [ ] An edition with no passing `proof_check` returns nothing from `servable_text_unit`, and one with a passing check serves.
- [ ] `wt text fix` changes the unit, writes `correction_log` and requires a new proofread before the edition serves again.
- [ ] Units from a PDF edition show their page number, and no page image is stored in the database.

## Source copies and legal pages

Decision of October 3, 2026. The index keeps the whole text of each work, cut into units, because search needs all of it. What changes is what happens to the original files and to the legal pages.

1. Legal pages are kept apart. The importer detects copyright pages, imprints, license and permission notices and colophons from section titles and notice patterns such as "Copyright", the copyright sign, "All rights reserved", ISBN lines and license names. It leaves them out of the units and stores them whole in `text_edition.legal_text`, with their page references in `legal_pages`. They feed the rights holder field, the notice scan and the credit line. They are never embedded, searched or shown, except that a notice may be quoted in an internal report.
2. Originals are not kept for mandated sources. `source_policy.source_retention` is `keep` or `delete_after_proof`. It defaults to `delete_after_proof` for a mandate and `keep` for a license or permission. When an edition's proofread passes, or at once for a born-digital edition whose sample check passed, the importer deletes the raw files from S3 and records `source_deleted_at`. It keeps the SHA-256 of each file, the source URL, the fetch time and the legal text. A re-import means fetching the document again.
3. Takedown deletes the units, embeddings, legal text and any remaining raw copies, and keeps only the stub with its hashes.

Migration `0018_retention_legal.sql`:

```sql
alter table text_edition add column legal_text text;
alter table text_edition add column legal_pages jsonb not null default '[]'::jsonb;
alter table text_edition add column source_deleted_at timestamptz;
alter table source_policy add column source_retention text not null default 'keep'
  check (source_retention in ('keep','delete_after_proof'));
update source_policy set source_retention = 'delete_after_proof' where basis = 'mandate';
```

## Excerpt limits

No word count is safe by rule. The U.S. Copyright Office says there is no specific number of words, lines or notes that may safely be taken without permission, and that crediting the source does not replace permission. Courts weigh four factors: the purpose and character of the use, the nature of the work, the amount and substance of the portion used, and the effect on the market for the work. A noncommercial use that helps people find and study works weighs in the project's favor, and even a short excerpt can weigh against it if it is the heart of the work. The limits below are therefore set to be defensible and easy to tune, and counsel reviews them.

Settings under `[excerpt]` in `config/retrieval.toml`:

```toml
[excerpt]
talk_passage_max_words = 250       # spoken teaching often circles before it lands
text_unit_max_words = 250
passages_per_talk_per_answer = 2
passages_per_teacher_per_answer = 3
neighbors_open_basis = 2           # permission and license editions
neighbors_mandate_basis = 1
text_page_max_words = 600          # words on one text page, neighbors included, for a mandate edition
text_views_per_hour = 30           # per tester
```

These values supersede the chunker defaults in Spec 04: `target_words` stays 140, `max_words` becomes 250 and `merge_cap_words` becomes 280. The 1,800-character database check still holds.

Rules:

- A talk passage may run to 250 words so that a point that arrives indirectly can arrive whole. The chunker still prefers to end at a sentence break near 140.
- Talks offer no "more of this talk" control. The source link leads to the talk itself. Texts offer context only through neighbors on the text page, at most 2 on each side for a permission or license edition and 1 for a mandate edition, capped at `text_page_max_words`.
- A rights holder can ask to withhold particular passages through the removal form without removing the whole work. Those requests become unit-level exclusions by content hash, so a request that a heart-of-the-work passage be left out is honored the same way as a full removal.
- `/text/{unit_id}` is limited to `text_views_per_hour` per tester, and the nightly audit warns when one tester views more than 60 distinct units of one work in a day.

## Alerts to the founder

The removal promise is one business day, so something must tell the owner a request has arrived. Terraform adds an SNS topic named `<project>-alerts`, an email subscription to the `alert_email` variable, and an `sns:Publish` permission on that topic alone for the web service and the batch host. The setting `WT_ALERT_TOPIC_ARN` points the code at it, and an empty value turns alerts off.

Messages go out for:

- a new `takedown_request` or `accessibility_report`
- a removal request still open after one business day, found by an hourly `alerts` job
- a failed nightly audit
- language-model spend at 80 percent of the cap, and again at the cap
- a dead job

A message holds a subject and one line with the id and a short target. It never holds contact details or question text. `wt alerts test` publishes a test message. This is an internal alert to the owner only. Letters to teachers, sanghas and publishers stay manual, and domain mailboxes are in the backlog.
