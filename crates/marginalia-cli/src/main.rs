//! `marginalia` — read EPUB / MOBI / PRC books in the terminal and listen to
//! them with open-source text-to-speech.

mod speech;

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use marginalia_tts::catalog::{self, InstalledVoice};

#[derive(Parser)]
#[command(
    name = "marginalia",
    version,
    about = "Open-source EPUB/MOBI/PRC reader with text-to-speech"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show a book's metadata.
    Info { book: PathBuf },
    /// List a book's chapters.
    Toc { book: PathBuf },
    /// Print a book (or one chapter) as plain text.
    Read {
        book: PathBuf,
        /// Chapter number (see `toc`); prints the whole book if omitted.
        #[arg(short, long)]
        chapter: Option<usize>,
    },
    /// Read a book aloud, starting at a chapter.
    Speak {
        book: PathBuf,
        /// Chapter to start from (1-based).
        #[arg(short, long, default_value_t = 1)]
        chapter: usize,
        /// Stop after this many chapters.
        #[arg(long)]
        chapters: Option<usize>,
        #[command(flatten)]
        tts: TtsArgs,
    },
    /// Speak a piece of text (handy for trying voices).
    Say {
        text: String,
        /// Language used to pick a default voice.
        #[arg(long, default_value = "en")]
        lang: String,
        #[command(flatten)]
        tts: TtsArgs,
    },
    /// Browse, download and remove Piper voice models.
    Voices {
        #[command(subcommand)]
        command: VoicesCommand,
    },
    /// Manage the Piper speech engine.
    Engine {
        #[command(subcommand)]
        command: EngineCommand,
    },
}

#[derive(Args, Clone)]
pub struct TtsArgs {
    /// Speech engine.
    #[arg(long, value_enum, default_value_t = EngineKind::Auto)]
    pub engine: EngineKind,
    /// Piper voice key (e.g. en_US-lessac-medium) or eSpeak voice (e.g. en-us).
    #[arg(long)]
    pub voice: Option<String>,
    /// Speaking speed; 1.0 is normal.
    #[arg(long, default_value_t = 1.0)]
    pub speed: f32,
    /// Speaker id for multi-speaker Piper voices.
    #[arg(long)]
    pub speaker: Option<u32>,
    /// Write audio to this WAV file instead of playing it.
    #[arg(short, long)]
    pub out: Option<PathBuf>,
    /// Download missing voices/engine without asking.
    #[arg(short, long)]
    pub yes: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EngineKind {
    /// Piper if possible, otherwise eSpeak NG.
    Auto,
    /// Piper neural voices (downloaded on demand).
    Piper,
    /// eSpeak NG installed on the system.
    Espeak,
}

#[derive(Subcommand)]
enum VoicesCommand {
    /// List voices from the Piper catalog.
    List {
        /// Only voices for this language, e.g. `en`, `en_GB`, `fr`.
        #[arg(long)]
        lang: Option<String>,
        /// Only voices already downloaded.
        #[arg(long)]
        installed: bool,
        /// Re-download the catalog.
        #[arg(long)]
        refresh: bool,
    },
    /// Download a voice model.
    Download { key: String },
    /// Delete a downloaded voice model.
    Remove { key: String },
}

#[derive(Subcommand)]
enum EngineCommand {
    /// Download the Piper runtime for this platform.
    Install,
    /// Show which engines are available and where data is stored.
    Status,
}

fn main() {
    if let Err(e) = run(Cli::parse()) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Info { book } => {
            let b = open(&book)?;
            println!("Title:    {}", b.metadata.title);
            println!("Authors:  {}", b.metadata.authors.join(", "));
            println!(
                "Language: {}",
                b.metadata.language.as_deref().unwrap_or("unknown")
            );
            println!("Format:   {:?}", b.format);
            println!("Chapters: {}", b.chapters.len());
            println!("Words:    {}", b.word_count());
        }
        Command::Toc { book } => {
            let b = open(&book)?;
            for (i, c) in b.chapters.iter().enumerate() {
                let words = marginalia_core::text::word_count(&c.text);
                println!("{:>4}  {}  ({words} words)", i + 1, c.title);
            }
        }
        Command::Read { book, chapter } => {
            let b = open(&book)?;
            let mut out = std::io::stdout().lock();
            let chapters = match chapter {
                Some(n) => &b.chapters[chapter_index(&b, n)?..=chapter_index(&b, n)?],
                None => &b.chapters[..],
            };
            for c in chapters {
                writeln!(out, "## {}\n\n{}\n", c.title, c.text)?;
            }
        }
        Command::Speak {
            book,
            chapter,
            chapters,
            tts,
        } => {
            let b = open(&book)?;
            let start = chapter_index(&b, chapter)?;
            let end = chapters.map_or(b.chapters.len(), |n| (start + n).min(b.chapters.len()));
            let lang = b.metadata.language.clone().unwrap_or_else(|| "en".into());
            let parts: Vec<speech::Part> = b.chapters[start..end]
                .iter()
                .enumerate()
                .map(|(i, c)| speech::Part {
                    label: format!("{} {}", start + i + 1, c.title),
                    text: c.text.clone(),
                })
                .collect();
            speech::speak(parts, &lang, &tts)?;
        }
        Command::Say { text, lang, tts } => {
            let part = speech::Part {
                label: String::new(),
                text,
            };
            speech::speak(vec![part], &lang, &tts)?;
        }
        Command::Voices { command } => voices(command)?,
        Command::Engine { command } => match command {
            EngineCommand::Install => {
                let path = marginalia_tts::piper::install(&mut progress_bar("Piper runtime"))?;
                eprintln!();
                println!("Installed Piper to {}", path.display());
            }
            EngineCommand::Status => {
                let data = marginalia_tts::paths::data_dir()?;
                println!("Data directory: {}", data.display());
                match marginalia_tts::piper::find_executable() {
                    Some(p) => println!("Piper:          {}", p.display()),
                    None => println!("Piper:          not installed (`marginalia engine install`)"),
                }
                match marginalia_tts::espeak::find_executable() {
                    Some(p) => println!("eSpeak NG:      {}", p.display()),
                    None => println!("eSpeak NG:      not found"),
                }
                let installed = InstalledVoice::list()?;
                println!("Voices:         {}", installed.len());
                for v in installed {
                    println!("  {}", v.key);
                }
                println!(
                    "Playback:       {}",
                    if cfg!(feature = "playback") {
                        "enabled"
                    } else {
                        "disabled (use --out FILE.wav)"
                    }
                );
            }
        },
    }
    Ok(())
}

fn voices(command: VoicesCommand) -> Result<()> {
    match command {
        VoicesCommand::List {
            lang,
            installed,
            refresh,
        } => {
            if installed && !refresh {
                for v in InstalledVoice::list()? {
                    println!("{}", v.key);
                }
                return Ok(());
            }
            let voices = catalog::load(refresh)?;
            let lang = lang.map(|l| l.replace('-', "_").to_lowercase());
            for v in &voices {
                if let Some(l) = &lang {
                    let code = v.language.code.to_lowercase();
                    if &code != l && v.language.family.to_lowercase() != *l {
                        continue;
                    }
                }
                let is_installed = v.is_installed();
                if installed && !is_installed {
                    continue;
                }
                println!(
                    "{:<36} {:<28} {:<7} {:>4} MB {}",
                    v.key,
                    format!(
                        "{} ({})",
                        v.language.name_english, v.language.country_english
                    ),
                    v.quality,
                    v.download_size() / 1_000_000,
                    if is_installed { "[installed]" } else { "" }
                );
            }
        }
        VoicesCommand::Download { key } => {
            let voices = catalog::load(false)?;
            let voice = catalog::find(&voices, &key)?;
            let v = voice.download(&mut progress_bar(&key))?;
            eprintln!();
            println!("Downloaded {} to {}", v.key, v.model.display());
        }
        VoicesCommand::Remove { key } => match InstalledVoice::find(&key)? {
            Some(v) => {
                v.remove()?;
                println!("Removed {key}");
            }
            None => bail!("voice {key:?} is not installed"),
        },
    }
    Ok(())
}

fn open(path: &PathBuf) -> Result<marginalia_core::Book> {
    marginalia_core::open(path).with_context(|| format!("opening {}", path.display()))
}

fn chapter_index(book: &marginalia_core::Book, n: usize) -> Result<usize> {
    if n == 0 || n > book.chapters.len() {
        bail!(
            "chapter {n} is out of range (the book has {} chapters)",
            book.chapters.len()
        );
    }
    Ok(n - 1)
}

/// Ask a yes/no question on the terminal; `default_yes` when non-interactive.
pub fn confirm(question: &str, assume_yes: bool) -> Result<bool> {
    if assume_yes {
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() {
        bail!("{question} Re-run with --yes to allow downloads.");
    }
    eprint!("{question} [Y/n] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    let a = answer.trim().to_lowercase();
    Ok(a.is_empty() || a == "y" || a == "yes")
}

/// A minimal single-line download progress indicator on stderr.
pub fn progress_bar(label: &str) -> impl FnMut(u64, Option<u64>) + '_ {
    let mut last_pct = u64::MAX;
    move |done, total| {
        let mb = done as f64 / 1e6;
        match total {
            Some(t) if t > 0 => {
                let pct = done * 100 / t;
                if pct != last_pct {
                    last_pct = pct;
                    eprint!(
                        "\rDownloading {label}: {pct:>3}% ({mb:.1}/{:.1} MB)",
                        t as f64 / 1e6
                    );
                }
            }
            _ => eprint!("\rDownloading {label}: {mb:.1} MB"),
        }
    }
}
