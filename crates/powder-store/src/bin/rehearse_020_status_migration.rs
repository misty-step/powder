use std::{env, fs, path::PathBuf, process};

use powder_store::status_model_020::{clone_and_rehearse, markdown_report};

fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => process::exit(2),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let args = Args::parse()?;
    let report = clone_and_rehearse(&args.source, &args.work)?;
    if let Some(path) = args.json {
        fs::write(path, serde_json::to_string_pretty(&report)?)?;
    }
    let markdown = markdown_report(&report);
    if let Some(path) = args.markdown {
        fs::write(path, &markdown)?;
    }
    print!("{markdown}");
    Ok(report.passed())
}

struct Args {
    source: PathBuf,
    work: PathBuf,
    json: Option<PathBuf>,
    markdown: Option<PathBuf>,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut source = None;
        let mut work = None;
        let mut json = None;
        let mut markdown = None;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--source" => source = Some(next_path(&mut args, "--source")?),
                "--work" => work = Some(next_path(&mut args, "--work")?),
                "--json" => json = Some(next_path(&mut args, "--json")?),
                "--markdown" => markdown = Some(next_path(&mut args, "--markdown")?),
                "--help" | "-h" => return Err(usage()),
                other => return Err(format!("unknown argument: {other}\n\n{}", usage())),
            }
        }
        Ok(Self {
            source: source.ok_or_else(usage)?,
            work: work.ok_or_else(usage)?,
            json,
            markdown,
        })
    }
}

fn next_path(
    args: &mut impl Iterator<Item = String>,
    flag: &'static str,
) -> Result<PathBuf, String> {
    args.next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("{flag} requires a path"))
}

fn usage() -> String {
    "usage: rehearse_020_status_migration --source <snapshot.db> --work <rehearsal.db> [--json <report.json>] [--markdown <report.md>]".to_string()
}
