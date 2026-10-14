//! `wt asr benchmark` (Spec 04, Transcription model and where it runs; Owner's hardware and the
//! decision rule): every available setup and model on one clip, scored against known text.
//!
//! Scoring follows `docs/asr-benchmark.md`: lowercase, accents and punctuation removed, hyphens
//! as spaces. Hypothesis words before the reference's first three words, when those appear in
//! its first quarter (a reading's preamble), are not scored, and reference words after the
//! hypothesis ends (a clip cut mid-text) cost nothing. A glossary term or a
//! teacher's name counts as wrong when any of its reference words is not matched exactly.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use unicode_normalization::UnicodeNormalization;
use wt_core::scratch::{ScratchDir, wav_duration_ms};
use wt_core::transcript::Word;

use crate::{AsrError, AsrHints, AsrSettings, WhisperCli, parse_cli_json};

/// One way of running whisper.cpp: a build and its environment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    /// Its name in reports, such as `vulkan-coopmat-off`.
    pub name: String,
    /// The `whisper-cli` executable.
    pub binary: PathBuf,
    /// Environment variables for the run, such as `GGML_VK_DISABLE_COOPMAT = "1"`.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Whether the GPU is used (`-ng` when false).
    #[serde(default = "yes")]
    pub use_gpu: bool,
    /// Extra `whisper-cli` arguments, such as `["-oved", "GPU"]` for the OpenVINO encoder.
    #[serde(default)]
    pub args: Vec<String>,
    /// Directories put in front of `PATH`, such as OpenVINO's libraries.
    #[serde(default)]
    pub path_prepend: Vec<PathBuf>,
}

/// `use_gpu`'s default: on.
fn yes() -> bool {
    true
}

/// A model to compare.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchModel {
    /// Its name in reports, such as `large-v3`.
    pub name: String,
    /// The ggml model file.
    pub path: PathBuf,
}

/// `config/asr_bench.toml`. Models are in order of preference: a later, faster model is chosen
/// only if it gets the glossary terms at least as right as every model before it (Spec 04,
/// rule 3).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchConfig {
    /// A run whose word error rate is over this, in percent, is unusable and left out of the
    /// decision. 5 for now; it has to come down as transcription improves.
    pub max_wer_percent: f64,
    /// The setups, in Spec 04's order.
    #[serde(rename = "setup")]
    pub setups: Vec<Setup>,
    /// The models, preferred first.
    #[serde(rename = "model")]
    pub models: Vec<BenchModel>,
}

impl BenchConfig {
    /// Reads the file; relative paths are taken from `base`.
    pub fn load(path: &Path, base: &Path) -> Result<Self, AsrError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| AsrError::Output(format!("reading {}: {e}", path.display())))?;
        let mut cfg: Self = toml::from_str(&text)
            .map_err(|e| AsrError::Output(format!("{}: {e}", path.display())))?;
        for s in &mut cfg.setups {
            if s.binary.is_relative() {
                s.binary = base.join(&s.binary);
            }
            for d in &mut s.path_prepend {
                if d.is_relative() {
                    *d = base.join(&*d);
                }
            }
        }
        for m in &mut cfg.models {
            if m.path.is_relative() {
                m.path = base.join(&m.path);
            }
        }
        Ok(cfg)
    }

    /// The setups whose binary exists, and the ones skipped with the reason.
    pub fn available(&self) -> (Vec<&Setup>, Vec<(String, String)>) {
        let mut ok = Vec::new();
        let mut skipped = Vec::new();
        for s in &self.setups {
            if s.binary.is_file() {
                ok.push(s);
            } else {
                skipped.push((s.name.clone(), format!("{} not found", s.binary.display())));
            }
        }
        (ok, skipped)
    }
}

/// What one run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineRun {
    /// The recognized words.
    pub words: Vec<Word>,
    /// Wall-clock time, model load included.
    pub wall: Duration,
    /// Highest sampled working set of the process, in bytes.
    pub peak_bytes: Option<u64>,
}

/// Runs one setup and model on a clip. The real engine is [`WhisperEngine`]; tests use a fake.
#[async_trait]
pub trait BenchEngine: Send + Sync {
    /// Transcribes `wav`, writing only inside `scratch`.
    async fn run(
        &self,
        setup: &Setup,
        model: &BenchModel,
        wav: &Path,
        scratch: &ScratchDir,
        hints: &AsrHints,
    ) -> Result<EngineRun, AsrError>;
}

/// `whisper-cli` as the worker runs it, with each setup's environment, sampling the process's
/// memory every 100 milliseconds.
#[derive(Debug, Clone)]
pub struct WhisperEngine {
    /// Threads and beam size, from `[asr]`.
    pub settings: AsrSettings,
}

#[async_trait]
impl BenchEngine for WhisperEngine {
    async fn run(
        &self,
        setup: &Setup,
        model: &BenchModel,
        wav: &Path,
        scratch: &ScratchDir,
        hints: &AsrHints,
    ) -> Result<EngineRun, AsrError> {
        let cli = WhisperCli {
            binary: setup.binary.clone(),
            model: model.path.clone(),
            threads: self.settings.threads,
            beam_size: self.settings.beam_size,
            use_gpu: setup.use_gpu,
            max_context: self.settings.max_context,
        };
        let stem = scratch.file(&format!("bench-{}-{}", setup.name, model.name))?;
        let mut cmd = cli.command(wav, &stem, hints);
        cmd.args(&setup.args);
        if !setup.path_prepend.is_empty() {
            let current = std::env::var_os("PATH").unwrap_or_default();
            let paths = setup
                .path_prepend
                .iter()
                .cloned()
                .chain(std::env::split_paths(&current));
            if let Ok(joined) = std::env::join_paths(paths) {
                cmd.env("PATH", joined);
            }
        }
        cmd.envs(&setup.env)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        let started = Instant::now();
        let mut child = cmd.spawn().map_err(|e| AsrError::Start {
            binary: setup.binary.display().to_string(),
            message: e.to_string(),
        })?;
        let pid = child.id().map(sysinfo::Pid::from_u32);
        let mut sys = sysinfo::System::new();
        let mut peak: Option<u64> = None;
        let status = loop {
            if let Some(pid) = pid {
                sys.refresh_processes_specifics(
                    sysinfo::ProcessesToUpdate::Some(&[pid]),
                    true,
                    sysinfo::ProcessRefreshKind::nothing().with_memory(),
                );
                if let Some(p) = sys.process(pid) {
                    peak = Some(peak.unwrap_or(0).max(p.memory()));
                }
            }
            tokio::select! {
                status = child.wait() => break status,
                () = tokio::time::sleep(Duration::from_millis(100)) => {}
            }
        }
        .map_err(|e| AsrError::Failed(e.to_string()))?;
        let wall = started.elapsed();
        if !status.success() {
            let mut stderr = String::new();
            if let Some(mut err) = child.stderr.take() {
                let _ = tokio::io::AsyncReadExt::read_to_string(&mut err, &mut stderr).await;
            }
            let line = stderr.lines().last().unwrap_or("no message").to_owned();
            return Err(AsrError::Failed(format!("{status}: {line}")));
        }
        let json_path = stem.with_extension("json");
        let json =
            std::fs::read_to_string(&json_path).map_err(|e| AsrError::Output(e.to_string()))?;
        Ok(EngineRun {
            words: parse_cli_json(&json)?,
            wall,
            peak_bytes: peak,
        })
    }
}

/// Text in scoring form: lowercase words with accents and punctuation removed and hyphens as
/// spaces.
pub fn normalize(text: &str) -> Vec<String> {
    let folded: String = text
        .nfd()
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
        .flat_map(char::to_lowercase)
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect();
    folded
        .split_whitespace()
        .map(|w| w.replace('\'', ""))
        .filter(|w| !w.is_empty())
        .collect()
}

/// Errors out of a total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rate {
    /// Wrong.
    pub errors: usize,
    /// Counted.
    pub total: usize,
}

impl Rate {
    /// As a percentage; none when nothing was counted.
    pub fn percent(self) -> Option<f64> {
        (self.total > 0).then(|| self.errors as f64 * 100.0 / self.total as f64)
    }

    /// An error rate as a table cell: the percentage with its counts, or `n/a` with nothing to
    /// count.
    fn cell(self) -> String {
        match self.percent() {
            Some(p) => format!("{p:.2}% ({}/{})", self.errors, self.total),
            None => "n/a".into(),
        }
    }
}

/// Spec 04's three error rates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ErrorRates {
    /// Word error rate: substitutions, deletions and insertions over reference words.
    pub overall: Rate,
    /// Glossary-term occurrences in the reference not recognized exactly.
    pub glossary: Rate,
    /// Teacher-name occurrences in the reference not recognized exactly.
    pub names: Rate,
}

/// For each reference word, whether it was matched exactly; and the edit count.
fn align(reference: &[String], hypothesis: &[String]) -> (Vec<bool>, usize) {
    let (n, m) = (reference.len(), hypothesis.len());
    // d[i][j]: cost of aligning the first i reference words with the first j hypothesis words.
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sub = d[i - 1][j - 1] + usize::from(reference[i - 1] != hypothesis[j - 1]);
            d[i][j] = sub.min(d[i - 1][j] + 1).min(d[i][j - 1] + 1);
        }
    }
    // Trailing reference words are free: end at the best row in the last column.
    let end = (0..=n)
        .min_by_key(|&i| (d[i][m], std::cmp::Reverse(i)))
        .unwrap_or(n);
    let mut matched = vec![false; n];
    let (mut i, mut j) = (end, m);
    while i > 0 && j > 0 {
        let same = reference[i - 1] == hypothesis[j - 1];
        if d[i][j] == d[i - 1][j - 1] + usize::from(!same) {
            matched[i - 1] = same;
            i -= 1;
            j -= 1;
        } else if d[i][j] == d[i - 1][j] + 1 {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    // Reference words past `end` are outside the clip: they are neither right nor wrong, and
    // the caller counts only the first `end`.
    matched.truncate(end);
    (matched, d[end][m])
}

/// Occurrences of `phrases` in `reference`, each as a word range.
fn occurrences(reference: &[String], phrases: &[Vec<String>]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for i in 0..reference.len() {
        for p in phrases {
            if !p.is_empty() && reference[i..].starts_with(p) {
                out.push((i, i + p.len()));
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Scores `hypothesis` against `reference`. `terms` and `names` are phrases in any spelling;
/// they are normalized here.
pub fn score(reference: &str, hypothesis: &str, terms: &[String], names: &[String]) -> ErrorRates {
    let r = normalize(reference);
    let mut h = normalize(hypothesis);
    // A preamble before the reference begins is not scored.
    if r.len() >= 3 {
        let limit = h.len() / 4;
        if let Some(start) = (1..=limit).find(|&j| h[j..].starts_with(&r[..3])) {
            h.drain(..start);
        }
    }
    let (matched, edits) = align(&r, &h);
    let scored = matched.len();
    let rate = |phrases: &[String]| {
        let phrases: Vec<Vec<String>> = phrases.iter().map(|p| normalize(p)).collect();
        let found: Vec<(usize, usize)> = occurrences(&r[..scored], &phrases)
            .into_iter()
            .filter(|&(_, b)| b <= scored)
            .collect();
        Rate {
            errors: found
                .iter()
                .filter(|&&(a, b)| !matched[a..b].iter().all(|&m| m))
                .count(),
            total: found.len(),
        }
    };
    ErrorRates {
        overall: Rate {
            errors: edits,
            total: scored,
        },
        glossary: rate(terms),
        names: rate(names),
    }
}

/// One setup and model on the clip.
#[derive(Debug, Clone, PartialEq)]
pub struct RunResult {
    /// The setup.
    pub setup: String,
    /// The model.
    pub model: String,
    /// Seconds of audio per second of wall-clock time; none when the run failed.
    pub rtf: Option<f64>,
    /// Wall-clock time.
    pub wall: Option<Duration>,
    /// Highest sampled working set, bytes.
    pub peak_bytes: Option<u64>,
    /// Words recognized.
    pub words: usize,
    /// Against the reference, when there is one.
    pub rates: Option<ErrorRates>,
    /// Why the run failed.
    pub error: Option<String>,
}

/// Spec 04's routes by real-time factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// 3 or more: the whole corpus on this PC.
    ThisPc,
    /// Between 1 and 3: the first 20 to 30 talks here for Gate A, a rented GPU for the rest.
    SplitWithRentedGpu,
    /// Below 1: a rented GPU for everything.
    RentedGpu,
}

impl Route {
    /// The route for a real-time factor.
    pub fn for_rtf(rtf: f64) -> Self {
        if rtf >= 3.0 {
            Route::ThisPc
        } else if rtf >= 1.0 {
            Route::SplitWithRentedGpu
        } else {
            Route::RentedGpu
        }
    }

    /// In words.
    pub fn describe(self) -> &'static str {
        match self {
            Route::ThisPc => "run the whole corpus on this PC",
            Route::SplitWithRentedGpu => {
                "run the first 20 to 30 talks on this PC for Gate A and rent a GPU machine for the rest"
            }
            Route::RentedGpu => "rent a GPU machine for everything",
        }
    }
}

/// What the rules pick.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// Runs left out because their output is unusable, with the reason.
    pub unusable: Vec<(String, String, String)>,
    /// The best real-time factor of any usable run, and its route.
    pub best: Option<(String, String, f64, Route)>,
    /// The model the vocabulary rule keeps, with the reason.
    pub model: Option<(String, String)>,
    /// That model's fastest setup, its real-time factor and route: the setup chosen.
    pub chosen: Option<(String, f64, Route)>,
}

/// Applies Spec 04's rules to the usable runs. `models` is in order of preference. A later model is acceptable
/// only when, on every setup where both ran, it gets no more glossary terms wrong than each
/// earlier model; of the acceptable models, the one with the fastest run is chosen.
pub fn decide(results: &[RunResult], models: &[String], max_wer_percent: f64) -> Decision {
    // A fast run that produced garbage or stopped early must not win on speed: a word error
    // rate over `max_wer_percent`, or under half the median word count of the completed runs,
    // makes a run unusable. The word count matters with a reference too, because reference
    // words after the hypothesis ends are free: a run that stops early is scored on less.
    let completed: Vec<&RunResult> = results.iter().filter(|r| r.rtf.is_some()).collect();
    let mut counts: Vec<usize> = completed.iter().map(|r| r.words).collect();
    counts.sort_unstable();
    let median = counts.get(counts.len() / 2).copied().unwrap_or(0);
    let mut unusable = Vec::new();
    let mut ok: Vec<&RunResult> = Vec::new();
    for r in completed {
        let why = match r.rates.and_then(|x| x.overall.percent()) {
            Some(wer) if wer > max_wer_percent => Some(format!(
                "word error rate {wer:.1}% is over {max_wer_percent}%"
            )),
            _ if r.words * 2 < median => Some(format!(
                "{} words, under half the median of {median}",
                r.words
            )),
            _ => None,
        };
        match why {
            Some(why) => unusable.push((r.setup.clone(), r.model.clone(), why)),
            None => ok.push(r),
        }
    }
    let rtf = |r: &RunResult| r.rtf.unwrap_or(0.0);
    let best = ok.iter().max_by(|a, b| rtf(a).total_cmp(&rtf(b))).map(|r| {
        (
            r.setup.clone(),
            r.model.clone(),
            rtf(r),
            Route::for_rtf(rtf(r)),
        )
    });

    let term_errors = |model: &str, setup: &str| {
        ok.iter()
            .find(|r| r.model == model && r.setup == setup)
            .and_then(|r| r.rates)
            .map(|x| x.glossary.errors)
    };
    let mut acceptable: Vec<(&String, String)> = Vec::new();
    for (k, m) in models.iter().enumerate() {
        let worse = models[..k].iter().find_map(|earlier| {
            ok.iter().filter(|r| &r.model == m).find_map(|r| {
                match (term_errors(m, &r.setup), term_errors(earlier, &r.setup)) {
                    (Some(a), Some(b)) if a > b => Some(format!(
                        "{m} got {a} glossary terms wrong on {} where {earlier} got {b}",
                        r.setup
                    )),
                    _ => None,
                }
            })
        });
        match worse {
            Some(reason) => acceptable.push((m, format!("rejected: {reason}"))),
            None if ok.iter().any(|r| &r.model == m) => acceptable.push((m, String::new())),
            None => {}
        }
    }
    let fastest = |m: &str| {
        ok.iter()
            .filter(|r| r.model == m)
            .max_by(|a, b| rtf(a).total_cmp(&rtf(b)))
            .map(|r| (r.setup.clone(), rtf(r)))
    };
    let chosen_model = acceptable
        .iter()
        .filter(|(_, why)| why.is_empty())
        .filter_map(|(m, _)| fastest(m).map(|f| ((*m).clone(), f)))
        .max_by(|a, b| a.1.1.total_cmp(&b.1.1));
    let rejected: Vec<String> = acceptable
        .iter()
        .filter(|(_, why)| !why.is_empty())
        .map(|(_, why)| why.clone())
        .collect();
    let model = chosen_model.as_ref().map(|(m, _)| {
        let why = if rejected.is_empty() {
            "fastest model that keeps the glossary terms".to_owned()
        } else {
            format!(
                "fastest model that keeps the glossary terms; {}",
                rejected.join("; ")
            )
        };
        (m.clone(), why)
    });
    let chosen = chosen_model.map(|(_, (setup, r))| (setup, r, Route::for_rtf(r)));
    Decision {
        unusable,
        best,
        model,
        chosen,
    }
}

/// Bytes as gigabytes, or `n/a` when not measured.
fn gib(bytes: Option<u64>) -> String {
    bytes.map_or("n/a".into(), |b| format!("{:.1} GB", b as f64 / 1e9))
}

/// The results as a Markdown table, then the decision.
pub fn render(results: &[RunResult], decision: &Decision, skipped: &[(String, String)]) -> String {
    let mut out = String::from(
        "| Setup | Model | RTF | Wall time | Peak RAM | Words | WER | Glossary terms wrong | Teacher names wrong |\n\
         | --- | --- | --- | --- | --- | --- | --- | --- | --- |\n",
    );
    for r in results {
        let rates = r.rates.unwrap_or_default();
        let (wer, terms, names) = if r.rates.is_some() {
            (
                rates.overall.cell(),
                rates.glossary.cell(),
                rates.names.cell(),
            )
        } else {
            ("n/a".into(), "n/a".into(), "n/a".into())
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.setup,
            r.model,
            r.rtf.map_or_else(
                || format!("failed: {}", r.error.as_deref().unwrap_or("?")),
                |x| format!("{x:.2}")
            ),
            r.wall
                .map_or("n/a".into(), |w| format!("{:.1} s", w.as_secs_f64())),
            gib(r.peak_bytes),
            r.words,
            wer,
            terms,
            names
        );
    }
    for (name, why) in skipped {
        let _ = writeln!(out, "| {name} | | skipped: {why} | | | | | | |");
    }
    out.push('\n');
    for (setup, model, why) in &decision.unusable {
        let _ = writeln!(
            out,
            "Left out: {model} on {setup}, unusable output ({why})."
        );
    }
    if let Some((setup, model, rtf, route)) = &decision.best {
        let _ = writeln!(
            out,
            "Best real-time factor: {rtf:.2}, {model} on {setup}. On speed alone the rule says: {}.",
            route.describe()
        );
    }
    if let Some((model, why)) = &decision.model {
        let _ = writeln!(out, "Model: {model} ({why}).");
    }
    match &decision.chosen {
        Some((setup, rtf, route)) => {
            let _ = writeln!(
                out,
                "Chosen setup: {setup}, at {rtf:.2} times real time, so {}.",
                route.describe()
            );
        }
        None => out.push_str("No setup completed, so nothing is chosen.\n"),
    }
    out
}

/// What a benchmark run is scored against: the clip's known text, and the glossary terms and
/// teacher names whose error rates are counted apart (Spec 04).
#[derive(Debug, Clone, Copy)]
pub struct Scoring<'a> {
    /// The known text.
    pub reference: &'a str,
    /// Glossary terms.
    pub terms: &'a [String],
    /// Teacher names and aliases from the registry.
    pub names: &'a [String],
}

/// Runs every setup with every model on `wav` and, given `scoring`, scores each run. Without a
/// known text the runs are timed only.
pub async fn run(
    engine: &dyn BenchEngine,
    setups: &[&Setup],
    models: &[BenchModel],
    wav: &Path,
    scratch: &ScratchDir,
    hints: &AsrHints,
    scoring: Option<Scoring<'_>>,
) -> Result<Vec<RunResult>, AsrError> {
    let audio_ms = wav_duration_ms(wav)?;
    let mut out = Vec::new();
    for setup in setups {
        for model in models {
            tracing::info!(setup = %setup.name, model = %model.name, "benchmark run");
            let result = if !model.path.is_file() {
                Err(AsrError::Output(format!(
                    "{} not found",
                    model.path.display()
                )))
            } else {
                engine.run(setup, model, wav, scratch, hints).await
            };
            out.push(match result {
                Ok(run) => {
                    let text = run
                        .words
                        .iter()
                        .map(|w| w.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ");
                    RunResult {
                        setup: setup.name.clone(),
                        model: model.name.clone(),
                        rtf: Some(audio_ms as f64 / run.wall.as_secs_f64().max(1e-3) / 1000.0),
                        wall: Some(run.wall),
                        peak_bytes: run.peak_bytes,
                        words: run.words.len(),
                        rates: scoring.map(|s| score(s.reference, &text, s.terms, s.names)),
                        error: None,
                    }
                }
                Err(e) => RunResult {
                    setup: setup.name.clone(),
                    model: model.name.clone(),
                    rtf: None,
                    wall: None,
                    peak_bytes: None,
                    words: 0,
                    rates: None,
                    error: Some(e.to_string()),
                },
            });
        }
    }
    Ok(out)
}

/// The machine, for the record: CPU, memory and operating system.
pub fn hardware() -> String {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_list(sysinfo::CpuRefreshKind::nothing());
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_owned())
        .unwrap_or_else(|| "unknown CPU".into());
    format!(
        "{cpu}, {} logical cores, {:.0} GB RAM, {}",
        sys.cpus().len(),
        sys.total_memory() as f64 / 1e9,
        sysinfo::System::long_os_version().unwrap_or_else(|| "unknown OS".into())
    )
}

/// The section `wt asr benchmark --record` appends to `docs/asr-benchmark.md`.
pub fn record_section(
    date: &str,
    hardware: &str,
    clip: &str,
    note: Option<&str>,
    table: &str,
) -> String {
    let mut s = format!(
        "\n## Run of {date}, `wt asr benchmark`\n\n- Hardware: {hardware}.\n- Clip: {clip}.\n"
    );
    if let Some(n) = note {
        let _ = writeln!(s, "- Note: {n}");
    }
    s.push('\n');
    s.push_str(table);
    s
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn normalizing_drops_case_accents_punctuation_and_hyphens() {
        assert_eq!(
            normalize("Dōgen's thorough-going, ZAZEN!"),
            ["dogens", "thorough", "going", "zazen"]
        );
    }

    #[test]
    fn a_preamble_and_a_cut_ending_cost_nothing() {
        let r = score(
            "to study the self is to forget the self and more words after the cut",
            "librivox recording to study the self is to forget the self",
            &[],
            &[],
        );
        assert_eq!((r.overall.errors, r.overall.total), (0, 9));
    }

    #[test]
    fn a_misheard_term_counts_once_per_occurrence() {
        let r = score(
            "the atman and the anatman and the atman",
            "the ottman and the anatman and the atman",
            &["Atman".into(), "Anatman".into()],
            &[],
        );
        assert_eq!((r.glossary.errors, r.glossary.total), (1, 3));
        assert_eq!(r.overall.errors, 1);
    }

    fn run(
        setup: &str,
        model: &str,
        rtf: f64,
        words: usize,
        wer: (usize, usize),
        terms: (usize, usize),
    ) -> RunResult {
        RunResult {
            setup: setup.into(),
            model: model.into(),
            rtf: Some(rtf),
            wall: Some(Duration::from_secs_f64(600.0 / rtf)),
            peak_bytes: None,
            words,
            rates: Some(ErrorRates {
                overall: Rate {
                    errors: wer.0,
                    total: wer.1,
                },
                glossary: Rate {
                    errors: terms.0,
                    total: terms.1,
                },
                names: Rate::default(),
            }),
            error: None,
        }
    }

    /// The run recorded on October 6, 2026 chose default Vulkan, whose 38 words of garbage were
    /// scored on 35 reference words with no glossary term among them.
    #[test]
    fn garbage_and_inaccurate_runs_are_left_out_of_the_decision() {
        let results = vec![
            run("vulkan", "large-v3", 4.21, 38, (42, 35), (0, 0)),
            run("vulkan", "distil-large-v3", 0.64, 1634, (1068, 597), (4, 4)),
            run(
                "vulkan-coopmat-off",
                "large-v3",
                1.99,
                1668,
                (150, 1701),
                (1, 4),
            ),
            run(
                "vulkan-coopmat-off",
                "distil-large-v3",
                9.16,
                1751,
                (51, 1701),
                (3, 4),
            ),
            run("openvino-gpu", "large-v3", 1.30, 1616, (1013, 1701), (1, 4)),
            run(
                "openvino-gpu",
                "distil-large-v3",
                10.34,
                1749,
                (46, 1701),
                (3, 4),
            ),
            run("openvino-npu", "large-v3", 0.64, 1846, (181, 1701), (1, 4)),
            run(
                "openvino-npu",
                "distil-large-v3",
                9.23,
                1750,
                (51, 1701),
                (4, 4),
            ),
            run("cpu", "large-v3", 0.34, 1746, (65, 1701), (1, 4)),
            run("cpu", "distil-large-v3", 1.02, 1749, (45, 1701), (3, 4)),
        ];
        let models = ["large-v3".to_owned(), "distil-large-v3".to_owned()];
        let d = decide(&results, &models, 5.0);

        let out: Vec<(&str, &str)> = d
            .unusable
            .iter()
            .map(|(s, m, _)| (s.as_str(), m.as_str()))
            .collect();
        assert_eq!(
            out,
            [
                ("vulkan", "large-v3"),
                ("vulkan", "distil-large-v3"),
                ("vulkan-coopmat-off", "large-v3"),
                ("openvino-gpu", "large-v3"),
                ("openvino-npu", "large-v3"),
            ]
        );
        assert_eq!(d.unusable[2].2, "word error rate 8.8% is over 5%");
        // Only the CPU run of large-v3 is accurate enough, and distil-large-v3 still misses
        // more glossary terms there, so large-v3 on the CPU is chosen and is too slow.
        assert_eq!(d.model.as_ref().map(|m| m.0.as_str()), Some("large-v3"));
        assert_eq!(
            d.chosen.map(|(s, _, route)| (s, route)),
            Some(("cpu".to_owned(), Route::RentedGpu))
        );

        // A short run is unusable even when the words it has are right.
        let short = vec![
            run("a", "large-v3", 5.0, 100, (0, 100), (0, 0)),
            run("b", "large-v3", 2.0, 1700, (20, 1701), (0, 4)),
            run("c", "large-v3", 1.5, 1690, (30, 1701), (0, 4)),
        ];
        let d = decide(&short, &models[..1], 5.0);
        assert_eq!(d.unusable[0].2, "100 words, under half the median of 1690");
        assert_eq!(d.chosen.map(|c| c.0), Some("b".to_owned()));
    }

    #[test]
    fn routes_follow_the_thresholds() {
        assert_eq!(Route::for_rtf(3.26), Route::ThisPc);
        assert_eq!(Route::for_rtf(1.06), Route::SplitWithRentedGpu);
        assert_eq!(Route::for_rtf(0.5), Route::RentedGpu);
    }
}
