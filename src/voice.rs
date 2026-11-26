//! Questions read aloud. When an agent asks something through AskUser, arakun says the
//! question and its options with an ElevenLabs voice, one question at a time, and stops
//! as soon as it is answered. The API key lives in the desktop keyring; the switch and
//! the voice in `voice.json` next to arakun's state.

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::Question;
use crate::platform::keyring;

const KEYRING_ACCOUNT: &str = "elevenlabs-api-key";
/// ElevenLabs' lowest-latency multilingual model.
const MODEL: &str = "eleven_flash_v2_5";
const SAMPLE_RATE: u32 = 24_000;
/// The most characters one questionnaire may spend; the rest is not read.
const MAX_CHARS: usize = 1_000;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub voice_id: String,
    #[serde(default)]
    pub tone: Tone,
}

/// How much the voice acts out what it reads; ElevenLabs calls the inverse stability.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Tone {
    Expressive,
    #[default]
    Balanced,
    Calm,
}

impl Tone {
    pub const ALL: [Tone; 3] = [Tone::Expressive, Tone::Balanced, Tone::Calm];

    pub fn label(self) -> &'static str {
        match self {
            Tone::Expressive => "Expressive",
            Tone::Balanced => "Balanced",
            Tone::Calm => "Calm",
        }
    }

    fn stability(self) -> f32 {
        match self {
            Tone::Expressive => 0.45,
            Tone::Balanced => 0.7,
            Tone::Calm => 0.95,
        }
    }
}

fn file() -> PathBuf {
    crate::paths::store("voice.json")
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn cached() -> &'static Mutex<Option<Settings>> {
    static SETTINGS: OnceLock<Mutex<Option<Settings>>> = OnceLock::new();
    SETTINGS.get_or_init(|| Mutex::new(None))
}

pub fn settings() -> Settings {
    lock(cached())
        .get_or_insert_with(|| crate::durable::load_json(file()).unwrap_or_default())
        .clone()
}

pub fn store(settings: &Settings) {
    *lock(cached()) = Some(settings.clone());
    let path = file();
    if let Err(e) = crate::durable::write(
        &path,
        &serde_json::to_vec_pretty(settings).unwrap_or_default(),
    ) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

pub fn api_key() -> Option<String> {
    keyring::lookup(KEYRING_ACCOUNT)
}

pub fn store_api_key(key: &str) -> Result<(), String> {
    keyring::store(KEYRING_ACCOUNT, "arakun: ElevenLabs API key", key.trim()).map_err(String::from)
}

pub fn clear_api_key() {
    keyring::clear(KEYRING_ACCOUNT);
}

fn last_error_slot() -> &'static Mutex<Option<String>> {
    static LAST: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Why the last question could not be read, until one is read again.
pub fn last_error() -> Option<String> {
    lock(last_error_slot()).clone()
}

// ---- what is said ---------------------------------------------------------------

/// Markdown and links out of `text`, so the voice says words, not symbols.
pub fn speakable(text: &str) -> String {
    let text = strip_links(text);
    let mut words = Vec::new();
    for w in text.split_whitespace() {
        if w.starts_with("http://") || w.starts_with("https://") {
            continue;
        }
        let w: String = w
            .chars()
            .filter(|c| !matches!(c, '`' | '*' | '#' | '>' | '~' | '|'))
            .map(|c| if c == '_' { ' ' } else { c })
            .collect();
        let w = w.trim();
        if !w.is_empty() {
            words.push(w.to_string());
        }
    }
    words.join(" ")
}

/// `[label](target)` becomes `label`.
fn strip_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find("](") else { break };
        let target = &after[close + 2..];
        let Some(end) = target.find(')') else { break };
        out.push_str(&rest[..open]);
        out.push_str(&after[..close]);
        rest = &target[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Small numbers as words: the fast ElevenLabs models read digits unreliably.
fn number(n: usize) -> String {
    const WORDS: [&str; 10] = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    match n {
        1..=10 => WORDS[n - 1].to_string(),
        _ => n.to_string(),
    }
}

fn sentence(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() || s.ends_with(['.', '?', '!', '…', ':']) {
        s.to_string()
    } else {
        format!("{s}.")
    }
}

/// What arakun says for a questionnaire from the session titled `session`.
pub fn question_text(session: &str, questions: &[Question]) -> String {
    let session = speakable(session);
    let n = questions.len();
    let mut out = match (n, session.is_empty()) {
        (1, true) => "I have a question.".to_string(),
        (1, false) => format!("I have a question in {session}."),
        (_, true) => format!("I have {} questions.", number(n)),
        (_, false) => format!("I have {} questions in {session}.", number(n)),
    };
    for (i, q) in questions.iter().enumerate() {
        if n > 1 {
            out.push_str(&format!(" Question {}.", number(i + 1)));
        }
        out.push(' ');
        out.push_str(&sentence(&speakable(&q.question)));
        if q.multi {
            out.push(' ');
            out.push_str("You can pick more than one.");
        }
        for (j, o) in q.options.iter().enumerate() {
            out.push_str(&format!(
                " Option {}: {}",
                number(j + 1),
                sentence(&speakable(o))
            ));
        }
    }
    truncate(out.trim(), MAX_CHARS)
}

fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_string(),
        Some((cut, _)) => {
            let head = &s[..cut];
            head[..head.rfind(' ').unwrap_or(cut)].to_string()
        }
    }
}

/// What the "Test voice" button says.
pub const SAMPLE: &str =
    "Hi. This is how Arakun reads questions to you. Option one: yes. Option two: no.";

// ---- ElevenLabs -----------------------------------------------------------------

/// ElevenLabs voice ids are letters and digits; anything else would change the URL.
pub fn valid_voice_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn temp_path(ext: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("arakun-voice-{}-{n}.{ext}", std::process::id()))
}

/// `text` spoken with `voice_id`, as a WAV file in the temp directory.
pub fn fetch(api_key: &str, voice_id: &str, tone: Tone, text: &str) -> Result<PathBuf, String> {
    let voice_id = voice_id.trim();
    if !valid_voice_id(voice_id) {
        return Err("Set the voice ID in Settings › Voice (the letters and digits ElevenLabs shows for a voice).".into());
    }
    let raw = temp_path("pcm");
    let url = format!(
        "https://api.elevenlabs.io/v1/text-to-speech/{voice_id}?output_format=pcm_{SAMPLE_RATE}"
    );
    // Without voice_settings ElevenLabs uses the voice's own, which for many library
    // voices are far more theatrical than a question needs.
    let body = json!({
        "text": text,
        "model_id": MODEL,
        // No language_code: the question is read in the language the agent wrote it in.
        "voice_settings": {
            "stability": tone.stability(),
            "similarity_boost": 0.75,
            "style": 0,
            "use_speaker_boost": true,
        },
    })
    .to_string();
    // The key travels in curl's config on stdin, never on a command line.
    let config = format!(
        "url = {}\nrequest = \"POST\"\nheader = {}\nheader = \"Content-Type: application/json\"\ndata-binary = {}\noutput = {}\n",
        quote(&url),
        quote(&format!("xi-api-key: {}", api_key.trim())),
        quote(&body),
        quote(&raw.to_string_lossy()),
    );
    let mut child = crate::platform::command("curl")
        .args(["-sS", "--max-time", "30", "-K", "-", "-w", "%{http_code}"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("curl: no stdin")?
        .write_all(config.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&raw).unwrap_or_default();
    let _ = std::fs::remove_file(&raw);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("Could not reach ElevenLabs ({}).", err.trim()));
    }
    let code: u16 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    if code != 200 {
        return Err(api_error(code, &bytes));
    }
    if bytes.is_empty() {
        return Err("ElevenLabs sent no audio.".into());
    }
    let wav_path = temp_path("wav");
    std::fs::write(&wav_path, wav(&bytes, SAMPLE_RATE)).map_err(|e| e.to_string())?;
    Ok(wav_path)
}

fn api_error(code: u16, body: &[u8]) -> String {
    let detail = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("detail").cloned());
    let msg = match &detail {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Object(o)) => o.get("message").and_then(Value::as_str).map(str::to_string),
        Some(Value::Array(a)) => a
            .first()
            .and_then(|e| e.get("msg"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    };
    let msg = msg
        .map(|m| m.trim().trim_end_matches('.').to_string())
        .filter(|m| !m.is_empty());
    match (code, msg) {
        (401, None) => "ElevenLabs did not accept the API key.".into(),
        (401, Some(m)) => format!("ElevenLabs did not accept the API key ({m})."),
        (_, Some(m)) => format!("ElevenLabs answered {code}: {m}."),
        (_, None) => format!("ElevenLabs answered {code}."),
    }
}

/// 16-bit mono PCM wrapped as a WAV file, which every player on both systems reads.
fn wav(pcm: &[u8], rate: u32) -> Vec<u8> {
    let len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

// ---- one voice at a time --------------------------------------------------------

enum Source {
    Text(String),
    File(PathBuf),
}

struct Job {
    key: String,
    source: Source,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// The job being fetched or played.
    current: Option<String>,
    player: Option<Child>,
    /// `cancel` hit the current job while it was still being fetched.
    dropped: bool,
}

fn queue() -> &'static (Mutex<Queue>, Condvar) {
    static QUEUE: OnceLock<(Mutex<Queue>, Condvar)> = OnceLock::new();
    QUEUE.get_or_init(|| {
        if let Err(e) = std::thread::Builder::new()
            .name("arakun-voice".into())
            .spawn(run)
        {
            eprintln!("arakun: voice: {e}");
        }
        (Mutex::new(Queue::default()), Condvar::new())
    })
}

/// Says `text` after whatever is being said; `key` is what [`cancel`] takes.
pub fn speak(key: impl Into<String>, text: String) {
    push(Job {
        key: key.into(),
        source: Source::Text(text),
    });
}

/// Plays a WAV file from [`fetch`] in turn, and deletes it afterwards.
pub fn play(key: impl Into<String>, path: PathBuf) {
    push(Job {
        key: key.into(),
        source: Source::File(path),
    });
}

fn push(job: Job) {
    let (m, cv) = queue();
    lock(m).jobs.push_back(job);
    cv.notify_one();
}

/// Stops saying `key`, or drops it if it has not started.
pub fn cancel(key: &str) {
    let (m, _) = queue();
    let mut q = lock(m);
    q.jobs.retain(|j| j.key != key);
    if q.current.as_deref() == Some(key) {
        q.dropped = true;
        if let Some(p) = q.player.as_mut() {
            let _ = p.kill();
        }
    }
}

/// Stops whatever is being said and drops everything waiting.
pub fn stop() {
    let (m, _) = queue();
    let mut q = lock(m);
    q.jobs.clear();
    if q.current.is_some() {
        q.dropped = true;
        if let Some(p) = q.player.as_mut() {
            let _ = p.kill();
        }
    }
}

fn run() {
    let (m, cv) = queue();
    loop {
        let job = {
            let mut q = lock(m);
            loop {
                if let Some(job) = q.jobs.pop_front() {
                    q.current = Some(job.key.clone());
                    q.dropped = false;
                    break job;
                }
                q = cv.wait(q).unwrap_or_else(|e| e.into_inner());
            }
        };
        let wav = match job.source {
            Source::File(path) => Ok(path),
            Source::Text(text) => synthesize(&text),
        };
        match wav {
            Ok(path) => {
                play_file(m, &path);
                let _ = std::fs::remove_file(&path);
            }
            Err(e) => {
                eprintln!("arakun: voice: {e}");
                *lock(last_error_slot()) = Some(e);
            }
        }
        let mut q = lock(m);
        q.current = None;
        q.player = None;
    }
}

fn synthesize(text: &str) -> Result<PathBuf, String> {
    let key = api_key().ok_or("Add your ElevenLabs API key in Settings › Voice.")?;
    let settings = settings();
    let path = fetch(&key, &settings.voice_id, settings.tone, text)?;
    *lock(last_error_slot()) = None;
    Ok(path)
}

fn play_file(m: &Mutex<Queue>, path: &Path) {
    {
        let mut q = lock(m);
        if q.dropped {
            return;
        }
        match crate::platform::play_sound(path) {
            Ok(child) => q.player = Some(child),
            Err(e) => {
                eprintln!("arakun: voice: {e}");
                *lock(last_error_slot()) = Some(format!("Could not play the audio: {e}"));
                return;
            }
        }
    }
    // Polled rather than waited on, so `cancel` can take the lock and kill the player.
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let mut q = lock(m);
        match q.player.as_mut().map(Child::try_wait) {
            Some(Ok(None)) => {}
            _ => break,
        }
    }
}

/// A value inside a double-quoted curl config string.
pub(crate) fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(question: &str, options: &[&str], multi: bool) -> Question {
        Question {
            index: 1,
            topic: String::new(),
            question: question.into(),
            options: options.iter().map(|o| o.to_string()).collect(),
            multi,
            secret: false,
        }
    }

    #[test]
    fn markdown_and_links_are_not_read() {
        assert_eq!(
            speakable("Use **`date-fns`** or [dayjs](https://day.js.org)?"),
            "Use date-fns or dayjs?"
        );
        assert_eq!(speakable("See https://example.com now"), "See now");
        assert_eq!(
            speakable("## pending_question  in  `src/voice.rs`"),
            "pending question in src/voice.rs"
        );
    }

    #[test]
    fn one_question_reads_its_options_in_order() {
        let text = question_text(
            "Add voice",
            &[q(
                "Which library do we use for dates?",
                &["date-fns", "dayjs"],
                false,
            )],
        );
        assert_eq!(
            text,
            "I have a question in Add voice. Which library do we use for dates? Option one: date-fns. Option two: dayjs."
        );
    }

    #[test]
    fn several_questions_are_numbered() {
        let text = question_text(
            "",
            &[
                q("Which features do you want?", &["Auth", "Login"], true),
                q("Which database?", &["Postgres"], false),
            ],
        );
        assert_eq!(
            text,
            "I have two questions. Question one. Which features do you want? You can pick more than one. \
             Option one: Auth. Option two: Login. Question two. Which database? Option one: Postgres."
        );
    }

    #[test]
    fn long_questionnaires_stop_at_a_word() {
        let long = "word ".repeat(400);
        let text = question_text("x", &[q(&long, &[], false)]);
        assert!(text.chars().count() <= MAX_CHARS);
        assert!(text.ends_with("word"));
    }

    #[test]
    fn wav_header_describes_the_samples() {
        let w = wav(&[1, 2, 3, 4], 24_000);
        assert_eq!(w.len(), 48);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 40);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 24_000);
        assert_eq!(u32::from_le_bytes(w[40..44].try_into().unwrap()), 4);
    }

    #[test]
    fn api_errors_read_like_sentences() {
        let body = br#"{"detail":{"status":"invalid_api_key","message":"Invalid API key"}}"#;
        assert_eq!(
            api_error(401, body),
            "ElevenLabs did not accept the API key (Invalid API key)."
        );
        assert_eq!(
            api_error(429, br#"{"detail":"Too many requests."}"#),
            "ElevenLabs answered 429: Too many requests."
        );
        assert_eq!(api_error(500, b"oops"), "ElevenLabs answered 500.");
    }

    #[test]
    fn older_settings_files_read_as_balanced() {
        let s: Settings = serde_json::from_str(r#"{"enabled":true,"voice_id":"abc"}"#).unwrap();
        assert_eq!(s.tone, Tone::Balanced);
        let s: Settings =
            serde_json::from_str(r#"{"enabled":true,"voice_id":"abc","tone":"calm"}"#).unwrap();
        assert_eq!(s.tone, Tone::Calm);
    }

    #[test]
    fn voice_ids_cannot_change_the_url() {
        assert!(valid_voice_id("21m00Tcm4TlvDq8ikWAM"));
        assert!(!valid_voice_id(""));
        assert!(!valid_voice_id("abc/../x"));
        assert!(!valid_voice_id("abc?x=1"));
    }
}
