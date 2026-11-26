use anyhow::{bail, ensure, Context, Result};
use std::{path::Path, process::exit, time::Instant};
use zoflib_archive::{group::Level, pack, restore::Archive};

const HELP: &str = "zofa pack FOLDER ARCHIVE [--level fast|balanced|max] [--resume] [--no-pass]
zofa list ARCHIVE
zofa verify ARCHIVE
zofa extract ARCHIVE NEW_FOLDER [PATH_PREFIX]
The passphrase comes from ZOFA_PASS, or is asked for (not shown while typing).
--no-pass: no passphrase; the archive is still checked for damage, but anyone can read it.";

fn pass(new: bool) -> Result<String> {
    let p = match std::env::var("ZOFA_PASS") {
        Ok(p) => p,
        Err(_) => {
            let p = rpassword::prompt_password("Passphrase: ")?;
            if new {
                ensure!(rpassword::prompt_password("Same passphrase again: ")? == p, "the two passphrases differ");
            }
            p
        }
    };
    ensure!(!p.is_empty(), "empty passphrase (use --no-pass for an archive without one)");
    Ok(p)
}

fn run() -> Result<bool> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| a.iter().any(|x| x == f);
    let val = |f: &str| a.iter().position(|x| x == f).and_then(|i| a.get(i + 1)).cloned();
    let arg = |i: usize| a.get(i).context(HELP);
    let t = Instant::now();
    match a.first().map(|s| s.as_str()) {
        Some("pack") => {
            let level = match val("--level").as_deref() {
                None | Some("balanced") => Level::Balanced,
                Some("fast") => Level::Fast,
                Some("max") => Level::Max,
                Some(x) => bail!("unknown level {x}"),
            };
            let resume = flag("--resume");
            let dst = Path::new(arg(2)?);
            let p = if flag("--no-pass") {
                String::new()
            } else if resume && !zoflib_archive::container::has_passphrase(dst)? {
                bail!("this archive has no passphrase; resume it with --no-pass")
            } else {
                pass(!resume)?
            };
            let o = pack::Options { level, resume, ..Default::default() };
            let r = pack::pack(Path::new(arg(1)?), dst, &p, o)?;
            for (n, why) in &r.skipped {
                eprintln!("left out: {n}: {why}");
            }
            println!(
                "{} files ({} from before), {:.1} MB in, {:.1} MB archive, {:.1} MB duplicates, {:.1} s, {} left out",
                r.files, r.resumed_files, r.bytes as f64 / 1e6, r.archive_bytes as f64 / 1e6, r.duplicate_bytes as f64 / 1e6,
                t.elapsed().as_secs_f64(), r.skipped.len()
            );
            Ok(r.complete && r.skipped.is_empty())
        }
        Some(c @ ("list" | "verify" | "extract")) => {
            let ap = Path::new(arg(1)?);
            let p = if zoflib_archive::container::has_passphrase(ap)? { pass(false)? } else { String::new() };
            let mut z = Archive::open(ap, &p)?;
            if !z.complete {
                eprintln!("warning: this archive is incomplete or damaged; only indexed files can be read");
            }
            for d in &z.damage {
                eprintln!("damage: {d}");
            }
            if c == "list" {
                for f in &z.files {
                    println!("{:>14}  {}", f.size, f.path);
                }
                return Ok(z.complete);
            }
            let out = if c == "extract" { Some(arg(2)?.clone()) } else { None };
            if let Some(o) = &out {
                let p = Path::new(o);
                ensure!(!p.exists() || p.read_dir()?.next().is_none(), "the output folder must be new or empty");
            }
            let prefix = if c == "extract" { a.get(3).cloned().unwrap_or_default() } else { String::new() };
            let r = z.restore(&|n| n.starts_with(&prefix), out.as_deref().map(Path::new))?;
            for (n, why) in &r.failed {
                eprintln!("failed: {n}: {why}");
            }
            println!("{} files good, {:.1} MB, {} failed, {:.1} s", r.files, r.bytes as f64 / 1e6, r.failed.len(), t.elapsed().as_secs_f64());
            Ok(r.failed.is_empty() && z.complete)
        }
        _ => bail!(HELP),
    }
}

fn main() {
    // parts of damaged or unusual files can make the recoders panic; that is caught and handled
    std::panic::set_hook(Box::new(|_| {}));
    match run() {
        Ok(true) => {}
        Ok(false) => exit(2),
        Err(e) => {
            eprintln!("error: {e:#}");
            exit(1)
        }
    }
}
