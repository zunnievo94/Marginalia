//! Translating and editing chapters with an AI model, using the reader's
//! own API key (Claude, or any OpenAI-compatible service).
//!
//! Two separate steps, each saved per chapter so it is paid for once:
//! - **translate**: the book's text into the reader's language;
//! - **edit**: polish a (machine-)translated text, above all making the
//!   names of characters, places, items and skills consistent.
//!
//! Every request carries the book's glossary (the names chosen so far) and
//! returns the new names it met, so that names stay the same from chapter
//! to chapter. Paragraphs map one to one, so notes, bookmarks and reading
//! positions keep pointing at the same places.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use marginalia_core::{Block, Book, Chapter};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `anthropic` or `openai` (any OpenAI-compatible service).
    pub provider: String,
    pub api_key: String,
    /// For OpenAI-compatible services, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub model: String,
    /// The language to translate into and edit in.
    pub language: String,
    /// Extra instructions from the reader, added to every request.
    pub instructions: String,
    /// Show translated and edited chapters instead of the book's text.
    pub use_ai_text: bool,
    /// Work on the chapter after the one being read: `""` (off),
    /// `translate`, `edit` or `both`.
    pub auto: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: "anthropic".into(),
            api_key: String::new(),
            base_url: "https://api.openai.com/v1".into(),
            model: "claude-opus-5-5".into(),
            language: "Vietnamese".into(),
            instructions: String::new(),
            use_ai_text: true,
            auto: String::new(),
        }
    }
}

/// Price in dollars per million input and output tokens, when known.
pub fn price(provider: &str, model: &str) -> Option<(f64, f64)> {
    if provider != "anthropic" {
        return None;
    }
    match model {
        "claude-opus-5-5" => Some((4.0, 20.0)),
        "claude-sonnet-5-5" => Some((2.0, 10.0)),
        "claude-haiku-5-5" => Some((0.10, 0.50)),
        _ => None,
    }
}

fn dir() -> Result<PathBuf> {
    Ok(marginalia_tts::paths::data_dir()?.join("ai"))
}

fn book_dir(id: &str) -> Result<PathBuf> {
    // Book ids are hex hashes, but never trust them as path components.
    let safe: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    Ok(dir()?.join(safe))
}

fn read_json<T: for<'de> Deserialize<'de> + Default>(path: &std::path::Path) -> T {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(tmp, path)?;
    Ok(())
}

pub fn load_config() -> Config {
    dir()
        .map(|d| read_json(&d.join("config.json")))
        .unwrap_or_default()
}

pub fn save_config(config: &Config) -> Result<()> {
    write_json(&dir()?.join("config.json"), config)
}

// ------------------------------------------------------------- glossary

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Term {
    /// The form to use.
    pub term: String,
    /// `character`, `place`, `organization`, `item`, `skill`, `title`
    /// or `other`.
    pub kind: String,
    /// Other spellings met in the book (and the original-language form).
    #[serde(default)]
    pub variants: Vec<String>,
}

pub fn load_glossary(id: &str) -> Vec<Term> {
    book_dir(id)
        .map(|d| read_json(&d.join("glossary.json")))
        .unwrap_or_default()
}

pub fn save_glossary(id: &str, terms: &[Term]) -> Result<()> {
    write_json(&book_dir(id)?.join("glossary.json"), &terms)
}

fn key(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Add the names a model returned: new ones are appended, variants of
/// known ones are added to them. A name the reader already has as a
/// variant stays a variant. Returns how many terms are new.
pub fn merge_terms(glossary: &mut Vec<Term>, new: Vec<Term>) -> usize {
    let mut added = 0;
    for t in new {
        let name = t.term.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let k = key(&name);
        let found = glossary
            .iter_mut()
            .find(|g| key(&g.term) == k || g.variants.iter().any(|v| key(v) == k));
        let entry = match found {
            Some(g) => g,
            None => {
                glossary.push(Term {
                    term: name.clone(),
                    kind: t.kind.clone(),
                    variants: Vec::new(),
                });
                added += 1;
                glossary.last_mut().unwrap()
            }
        };
        for v in t.variants {
            let v = v.trim();
            let vk = key(v);
            if !v.is_empty()
                && vk != key(&entry.term)
                && !entry.variants.iter().any(|x| key(x) == vk)
            {
                entry.variants.push(v.to_string());
            }
        }
    }
    added
}

// ---------------------------------------------------------- saved steps

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Step {
    Translate,
    Edit,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Step::Translate => "translate",
            Step::Edit => "edit",
        }
    }
}

/// A chapter's text after one step.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Saved {
    /// Hash of the text this step was made from; when that text changes
    /// (a new translation, another edition of the book), it is not used.
    source: u64,
    title: String,
    paragraphs: Vec<String>,
}

fn saved_path(id: &str, chapter: usize, step: Step) -> Result<PathBuf> {
    Ok(book_dir(id)?.join(format!("{chapter}.{}.json", step.name())))
}

fn load_saved(id: &str, chapter: usize, step: Step) -> Option<Saved> {
    let path = saved_path(id, chapter, step).ok()?;
    let s = fs::read_to_string(path).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn clear(id: &str, chapter: usize, step: Step) -> Result<()> {
    let path = saved_path(id, chapter, step)?;
    if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// FNV-1a: stable across builds, unlike the standard library's hasher.
fn hash(title: &str, paragraphs: &[String]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    feed(title.as_bytes());
    for p in paragraphs {
        feed(&[0]);
        feed(p.as_bytes());
    }
    h
}

fn paragraphs(chapter: &Chapter) -> Vec<String> {
    chapter.paragraphs().map(str::to_string).collect()
}

/// Put `title` and `paragraphs` (one per text block) into a chapter.
/// Inline styling is dropped from the paragraphs that changed.
fn set_text(chapter: &mut Chapter, title: &str, new: &[String]) {
    chapter.title = title.to_string();
    let mut i = 0;
    for block in &mut chapter.blocks {
        if let Block::Text(t) = block {
            if let Some(p) = new.get(i) {
                if t != p {
                    *t = p.clone();
                    if let Some(style) = chapter.styles.get_mut(i) {
                        style.spans.clear();
                    }
                }
            }
            i += 1;
        }
    }
    chapter.text = chapter
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
}

/// The chapter's text before `step` (the book's, or its translation),
/// with the hash a saved `step` must have been made from.
fn input(id: &str, index: usize, chapter: &Chapter, step: Step) -> (String, Vec<String>, u64) {
    let mut title = chapter.title.clone();
    let mut paras = paragraphs(chapter);
    if step == Step::Edit {
        let source = hash(&title, &paras);
        if let Some(t) = load_saved(id, index, Step::Translate) {
            if t.source == source && t.paragraphs.len() == paras.len() {
                title = t.title;
                paras = t.paragraphs;
            }
        }
    }
    let h = hash(&title, &paras);
    (title, paras, h)
}

/// Which steps are saved and current for each chapter of a book (read
/// before the reader's replacement rules are applied).
pub fn status(id: &str, book: &Book) -> Vec<[bool; 2]> {
    book.chapters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            [Step::Translate, Step::Edit].map(|step| {
                let (_, paras, h) = input(id, i, c, step);
                load_saved(id, i, step)
                    .is_some_and(|s| s.source == h && s.paragraphs.len() == paras.len())
            })
        })
        .collect()
}

/// Show the saved translations and edits in place of the book's text.
pub fn apply(id: &str, book: &mut Book) {
    for (i, chapter) in book.chapters.iter_mut().enumerate() {
        // Each step applies only to the text it was made from: the edit of
        // a translation, or of the book's own text.
        for step in [Step::Translate, Step::Edit] {
            let current = paragraphs(chapter);
            let source = hash(&chapter.title, &current);
            if let Some(s) = load_saved(id, i, step) {
                if s.source == source && s.paragraphs.len() == current.len() {
                    set_text(chapter, &s.title, &s.paragraphs);
                }
            }
        }
    }
}

// ------------------------------------------------------------- requests

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Dollars, when the model's price is known.
    pub cost: Option<f64>,
}

impl Usage {
    fn add(&mut self, o: Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cost = match (self.cost, o.cost) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        };
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Spending {
    pub cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

pub fn spending() -> Spending {
    dir()
        .map(|d| read_json(&d.join("spending.json")))
        .unwrap_or_default()
}

fn record(u: Usage) {
    let mut s = spending();
    s.cost += u.cost.unwrap_or(0.0);
    s.input_tokens += u.input_tokens;
    s.output_tokens += u.output_tokens;
    if let Ok(d) = dir() {
        let _ = write_json(&d.join("spending.json"), &s);
    }
}

fn system_prompt(config: &Config, step: Step) -> String {
    let lang = if config.language.trim().is_empty() {
        "Vietnamese"
    } else {
        config.language.trim()
    };
    let task = match step {
        Step::Translate => format!(
            "You translate novels into {lang}. Translate each paragraph completely and \
             faithfully, in natural {lang} prose that keeps the author's tone, dialogue \
             style and level of detail. Never summarise, skip, censor or add content.\n\
             Proper names (people, places, sects and organizations, items, techniques \
             and skills, titles): use the glossary's form whenever the name is in it. \
             For Chinese names translated into Vietnamese, use the usual Sino-Vietnamese \
             (Hán Việt) reading, as Vietnamese translations of Chinese novels do."
        ),
        Step::Edit => format!(
            "You are the editor of a {lang} novel that was machine-translated (often from \
             Chinese). Edit each paragraph so it reads as natural, correct {lang}:\n\
             - Make proper names consistent: every person, place, sect or organization, \
             item, technique or skill, and title must use one form, the glossary's \
             form when it is in the glossary. Machine translations often write the same \
             name several ways (a translated meaning in one place, a transliteration or \
             Sino-Vietnamese reading in another); use one.\n\
             - Fix grammar, word order, pronouns and forms of address, and awkward \
             machine-translated phrasing.\n\
             - Keep the meaning, events, dialogue and tone. Do not summarise, skip, \
             censor or add content; change no more than needed."
        ),
    };
    let mut prompt = format!(
        "{task}\n\n\
         The input is a JSON object: `title` (the chapter title) and `paragraphs` (a list \
         of paragraphs). Return the same structure: `title`, and `paragraphs` with \
         exactly one output paragraph for each input paragraph, in the same order. Never \
         merge or split paragraphs, even short ones.\n\
         Also return `terms`: the proper names in this text that are not in the glossary \
         yet, and glossary names that appeared here written another way. For each, \
         `term` is the form you used, `kind` is one of character, place, organization, \
         item, skill, title, other, and `variants` lists the other forms found in the \
         input (for a translation, the original-language form). Leave common words out."
    );
    if config.provider != "anthropic" {
        prompt.push_str(
            "\n\nAnswer with the JSON object only: {\"title\": string, \"paragraphs\": \
             [string], \"terms\": [{\"term\": string, \"kind\": string, \"variants\": \
             [string]}]}.",
        );
    }
    if !config.instructions.trim().is_empty() {
        prompt.push_str("\n\nInstructions from the reader:\n");
        prompt.push_str(config.instructions.trim());
    }
    prompt
}

fn glossary_text(glossary: &[Term]) -> String {
    if glossary.is_empty() {
        return "Glossary: empty so far.".into();
    }
    let mut s = String::from("Glossary (form to use: kind; other forms seen):\n");
    for t in glossary {
        s.push_str(&format!("{}: {}", t.term, t.kind));
        if !t.variants.is_empty() {
            s.push_str("; ");
            s.push_str(&t.variants.join(", "));
        }
        s.push('\n');
    }
    s
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "paragraphs": {"type": "array", "items": {"type": "string"}},
            "terms": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "term": {"type": "string"},
                    "kind": {"type": "string", "enum": [
                        "character", "place", "organization", "item", "skill", "title", "other"
                    ]},
                    "variants": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["term", "kind", "variants"],
                "additionalProperties": false
            }}
        },
        "required": ["title", "paragraphs", "terms"],
        "additionalProperties": false
    })
}

#[derive(Debug, Deserialize)]
struct Answer {
    #[serde(default)]
    title: String,
    paragraphs: Vec<String>,
    #[serde(default)]
    terms: Vec<Term>,
}

fn agent() -> ureq::Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(Duration::from_secs(600)))
        .http_status_as_error(false)
        .user_agent(concat!("marginalia/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The error message an API returned.
fn api_error(status: u16, body: &Value) -> anyhow::Error {
    let msg = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("no details");
    match status {
        401 | 403 => anyhow!("The API key was refused ({status}): {msg}"),
        429 => anyhow!("Too many requests or no credit left (429): {msg}"),
        _ => anyhow!("The AI service returned {status}: {msg}"),
    }
}

fn post(
    agent: &ureq::Agent,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> Result<(u16, Value)> {
    let mut req = agent.post(url).header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let mut resp = req
        .send(serde_json::to_vec(body)?)
        .with_context(|| format!("could not reach {url}"))?;
    let status = resp.status().as_u16();
    let text = resp
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .unwrap_or_default();
    let value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok((status, value))
}

fn ask_claude(
    config: &Config,
    system: &str,
    glossary: &str,
    user: &str,
) -> Result<(String, Usage)> {
    let model = config.model.as_str();
    let mut body = json!({
        "model": model,
        "max_tokens": 32000,
        "system": [
            {"type": "text", "text": system},
            {"type": "text", "text": glossary, "cache_control": {"type": "ephemeral"}}
        ],
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": schema()}
        },
        "messages": [{"role": "user", "content": user}]
    });
    let mut headers = vec![
        ("x-api-key", config.api_key.trim().to_string()),
        ("anthropic-version", "2026-10-21".to_string()),
    ];
    // A refusal is retried on another model by the API itself.
    if matches!(model, "claude-opus-5-5" | "claude-sonnet-5-5") {
        body["fallbacks"] = json!("default");
        headers.push(("anthropic-beta", "server-side-fallback-2026-07-01".into()));
    }
    let url = "https://api.anthropic.com/v1/messages";
    let agent = agent();
    let (mut status, mut v) = post(&agent, url, &headers, &body)?;
    let message = v
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    if status == 400 && message.contains("fallback") {
        // Not offered with these settings: ask again without it.
        body.as_object_mut().unwrap().remove("fallbacks");
        headers.retain(|(k, _)| *k != "anthropic-beta");
        (status, v) = post(&agent, url, &headers, &body)?;
    }
    if status != 200 {
        return Err(api_error(status, &v));
    }
    let input = v
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + v.pointer("/usage/cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0)
        + v.pointer("/usage/cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
    let output = v
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cost = price(&config.provider, model).map(|(pi, po)| {
        let read = v
            .pointer("/usage/cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let write = v
            .pointer("/usage/cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let plain = input - read - write;
        (plain as f64 + read as f64 * 0.1 + write as f64 * 1.25) * pi / 1e6
            + output as f64 * po / 1e6
    });
    let usage = Usage {
        input_tokens: input,
        output_tokens: output,
        cost,
    };
    match v.get("stop_reason").and_then(Value::as_str) {
        Some("refusal") => bail!("the model declined this part"),
        Some("max_tokens") => bail!("the answer was cut off"),
        _ => {}
    }
    let text: String = v
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    Ok((text, usage))
}

fn ask_openai(
    config: &Config,
    system: &str,
    glossary: &str,
    user: &str,
) -> Result<(String, Usage)> {
    let url = format!(
        "{}/chat/completions",
        config.base_url.trim().trim_end_matches('/')
    );
    let mut body = json!({
        "model": config.model,
        "messages": [
            {"role": "system", "content": format!("{system}\n\n{glossary}")},
            {"role": "user", "content": user}
        ],
        "response_format": {"type": "json_object"}
    });
    let headers = [("Authorization", format!("Bearer {}", config.api_key.trim()))];
    let agent = agent();
    let (mut status, mut v) = post(&agent, &url, &headers, &body)?;
    if status == 400 || status == 422 {
        // Not every compatible service knows `response_format`.
        body.as_object_mut().unwrap().remove("response_format");
        (status, v) = post(&agent, &url, &headers, &body)?;
    }
    if status != 200 {
        return Err(api_error(status, &v));
    }
    let usage = Usage {
        input_tokens: v
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: v
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cost: None,
    };
    if v.pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        == Some("length")
    {
        bail!("the answer was cut off");
    }
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok((text, usage))
}

/// The JSON object in a model's answer, ignoring code fences around it.
fn parse_answer(text: &str) -> Result<Answer> {
    let start = text
        .find('{')
        .ok_or_else(|| anyhow!("the answer had no JSON"))?;
    let end = text
        .rfind('}')
        .ok_or_else(|| anyhow!("the answer had no JSON"))?;
    serde_json::from_str(&text[start..=end]).context("the answer was not the expected JSON")
}

/// Paragraphs per request: about this many characters each.
const CHUNK_CHARS: usize = 6000;

fn chunks(paras: &[String]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (i, p) in paras.iter().enumerate() {
        if size > 0 && size + p.len() > CHUNK_CHARS {
            out.push(start..i);
            start = i;
            size = 0;
        }
        size += p.len();
    }
    if start < paras.len() || out.is_empty() {
        out.push(start..paras.len());
    }
    out
}

pub struct Job<'a> {
    pub config: &'a Config,
    pub step: Step,
    pub glossary: Vec<Term>,
    pub usage: Usage,
    pub new_terms: usize,
    /// Paragraphs kept as they were because the model's answer did not
    /// match them.
    pub kept: usize,
    /// The chapter title from the latest answer.
    title: Option<String>,
}

impl<'a> Job<'a> {
    pub fn new(config: &'a Config, step: Step, glossary: Vec<Term>) -> Self {
        Job {
            config,
            step,
            glossary,
            usage: Usage::default(),
            new_terms: 0,
            kept: 0,
            title: None,
        }
    }
}

impl Job<'_> {
    fn ask(&mut self, title: &str, paras: &[String]) -> Result<Answer> {
        let system = system_prompt(self.config, self.step);
        let glossary = glossary_text(&self.glossary);
        let user = json!({"title": title, "paragraphs": paras}).to_string();
        let (text, usage) = if self.config.provider == "anthropic" {
            ask_claude(self.config, &system, &glossary, &user)?
        } else {
            ask_openai(self.config, &system, &glossary, &user)?
        };
        self.usage.add(usage);
        record(usage);
        let answer = parse_answer(&text)?;
        self.new_terms += merge_terms(&mut self.glossary, answer.terms.clone());
        if !answer.title.trim().is_empty() {
            self.title = Some(answer.title.trim().to_string());
        }
        Ok(answer)
    }

    /// Paragraphs from the model, one for each of `paras`. A part whose
    /// answer has the wrong number of paragraphs is asked again in halves;
    /// a single paragraph that still fails is kept as it was.
    fn part(&mut self, title: &str, paras: &[String]) -> Result<Vec<String>> {
        match self.ask(title, paras) {
            Ok(a) if a.paragraphs.len() == paras.len() => Ok(a.paragraphs),
            Err(e) if is_fatal(&e) => Err(e),
            _ if paras.len() == 1 => {
                self.kept += 1;
                Ok(paras.to_vec())
            }
            _ => {
                let mid = paras.len() / 2;
                let mut out = self.part(title, &paras[..mid])?;
                out.extend(self.part(title, &paras[mid..])?);
                Ok(out)
            }
        }
    }
}

/// Errors not worth retrying in smaller parts.
fn is_fatal(e: &anyhow::Error) -> bool {
    let s = format!("{e:#}");
    s.contains("refused")
        || s.contains("could not reach")
        || s.contains("(429)")
        || s.contains("returned 4")
}

/// Translate or edit one chapter and save it. `book` is the book as
/// parsed, before any step or replacement.
pub fn run(
    id: &str,
    book: &Book,
    index: usize,
    job: &mut Job,
    replacer: &marginalia_core::replace::Replacer,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<()> {
    let chapter = book
        .chapters
        .get(index)
        .ok_or_else(|| anyhow!("no chapter {index}"))?;
    let (title, paras, source) = input(id, index, chapter, job.step);
    // The model sees the text with the reader's own replacements.
    let shown: Vec<String> = paras.iter().map(|p| replacer.apply(p)).collect();
    let shown_title = replacer.apply(&title);
    let parts = chunks(&shown);
    let mut out = Vec::with_capacity(paras.len());
    let mut new_title = shown_title.clone();
    for (n, range) in parts.iter().enumerate() {
        progress(n, parts.len());
        let answer = job.part(&shown_title, &shown[range.clone()])?;
        out.extend(answer);
        if n == 0 {
            if let Some(t) = job.title.take() {
                new_title = t;
            }
        }
    }
    progress(parts.len(), parts.len());
    save_glossary(id, &job.glossary)?;
    write_json(
        &saved_path(id, index, job.step)?,
        &Saved {
            source,
            title: new_title,
            paragraphs: out,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(t: &str, variants: &[&str]) -> Term {
        Term {
            term: t.into(),
            kind: "character".into(),
            variants: variants.iter().map(|v| v.to_string()).collect(),
        }
    }

    #[test]
    fn glossary_grows_without_duplicates() {
        let mut g = vec![term("Trương Cao Thiên", &["Trương Cao Trời"])];
        let added = merge_terms(
            &mut g,
            vec![
                term("trương cao thiên", &["Trương Cao Thiên", "张高天"]),
                term("Trương Cao Trời", &[]),
                term("Thanh Vân Môn", &["Thanh Mây Cửa"]),
                term(" ", &[]),
            ],
        );
        assert_eq!(added, 1);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].variants, ["Trương Cao Trời", "张高天"]);
        assert_eq!(g[1].term, "Thanh Vân Môn");
    }

    #[test]
    fn parts_and_answers() {
        let paras: Vec<String> = (0..10).map(|_| "x".repeat(2500)).collect();
        let parts = chunks(&paras);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0], 0..2);
        assert_eq!(chunks(&[]), vec![0..0]);
        let a = parse_answer(
            "```json\n{\"title\":\"T\",\"paragraphs\":[\"a\",\"b\"],\"terms\":[]}\n```",
        )
        .unwrap();
        assert_eq!(a.paragraphs, ["a", "b"]);
        assert!(parse_answer("sorry").is_err());
    }

    #[test]
    fn saved_steps_replace_the_text_they_were_made_from() {
        let dir = std::env::temp_dir().join(format!("marginalia-ai-test-{}", std::process::id()));
        std::env::set_var("MARGINALIA_DATA_DIR", &dir);
        let mut book = Book {
            format: marginalia_core::Format::Txt,
            metadata: Default::default(),
            chapters: vec![Chapter::from_text("第一章".into(), "他说。\n\n好。".into())],
            images: Vec::new(),
            cover: None,
        };
        let raw = paragraphs(&book.chapters[0]);
        let save = |step, source, title: &str, paras: &[&str]| {
            let saved = Saved {
                source,
                title: title.into(),
                paragraphs: paras.iter().map(|p| p.to_string()).collect(),
            };
            write_json(&saved_path("b1", 0, step).unwrap(), &saved).unwrap();
        };
        let h0 = hash("第一章", &raw);
        save(Step::Translate, h0, "Chương 1", &["Hắn nói.", "Được."]);
        let translated = vec!["Hắn nói.".to_string(), "Được.".to_string()];
        save(
            Step::Edit,
            hash("Chương 1", &translated),
            "Chương 1",
            &["Hắn bảo.", "Được."],
        );
        assert_eq!(status("b1", &book), [[true, true]]);

        let mut shown = book.clone();
        apply("b1", &mut shown);
        assert_eq!(shown.chapters[0].title, "Chương 1");
        assert_eq!(paragraphs(&shown.chapters[0]), ["Hắn bảo.", "Được."]);
        assert_eq!(shown.chapters[0].text, "Hắn bảo.\n\nĐược.");

        // A new translation makes the old edit stale.
        save(Step::Translate, h0, "Chương 1", &["Anh ấy nói.", "Được."]);
        assert_eq!(status("b1", &book), [[true, false]]);
        // Another edition of the book makes both stale.
        book.chapters[0] = Chapter::from_text("第一章".into(), "她说。\n\n好。".into());
        assert_eq!(status("b1", &book), [[false, false]]);
        let _ = fs::remove_dir_all(dir);
    }

    /// A local OpenAI-compatible server: the first answer has one paragraph
    /// too few, so the part is asked again in halves.
    #[test]
    fn asks_an_openai_compatible_service() {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for n in 0..3 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let user: Value = serde_json::from_str(
                    body.pointer("/messages/1/content")
                        .unwrap()
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                let paras: Vec<String> =
                    serde_json::from_value(user["paragraphs"].clone()).unwrap();
                let out: Vec<String> = if n == 0 {
                    vec!["merged".into()]
                } else {
                    paras.iter().map(|p| p.to_uppercase()).collect()
                };
                let content = json!({"title": "T", "paragraphs": out,
                    "terms": [{"term": "Lâm Phong", "kind": "character", "variants": ["Rừng Gió"]}]})
                .to_string();
                let answer = json!({
                    "choices": [{"message": {"content": format!("```json\n{content}\n```")}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                })
                .to_string();
                let mut stream = reader.into_inner();
                write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}", answer.len()).unwrap();
                bodies.push(body);
            }
            bodies
        });
        let config = Config {
            provider: "openai".into(),
            api_key: "k".into(),
            base_url: format!("http://127.0.0.1:{port}/v1/"),
            model: "m".into(),
            ..Config::default()
        };
        let mut job = Job::new(&config, Step::Edit, Vec::new());
        let out = job.part("t", &["a".into(), "b".into()]).unwrap();
        assert_eq!(out, ["A", "B"]);
        assert_eq!(job.usage.input_tokens, 30);
        assert_eq!(job.glossary.len(), 1);
        assert_eq!(job.glossary[0].variants, ["Rừng Gió"]);
        let bodies = server.join().unwrap();
        assert_eq!(bodies[0]["model"], "m");
        assert!(bodies[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Glossary: empty"));
        assert!(bodies[2]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Lâm Phong: character; Rừng Gió"));
    }
}
