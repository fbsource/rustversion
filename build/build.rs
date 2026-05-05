#![allow(
    clippy::elidable_lifetime_names,
    clippy::enum_glob_use,
    clippy::must_use_candidate,
    clippy::single_match_else
)]

mod rustc;

use std::env;
use std::ffi::OsString;
use std::fmt::{self, Debug, Display};
use std::fs;
use std::iter;
use std::path::Path;
use std::process::{self, Command};

fn main() {
    println!("cargo:rerun-if-changed=build/build.rs");

    let rustc = env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let rustc_wrapper = env::var_os("RUSTC_WRAPPER").filter(|wrapper| !wrapper.is_empty());
    let wrapped_rustc = rustc_wrapper.iter().chain(iter::once(&rustc));

    let mut is_clippy_driver = false;
    let mut is_mirai = false;
    let version = loop {
        let mut command;
        if is_mirai {
            command = Command::new(&rustc);
        } else {
            let mut wrapped_rustc = wrapped_rustc.clone();
            command = Command::new(wrapped_rustc.next().unwrap());
            command.args(wrapped_rustc);
        }
        if is_clippy_driver {
            command.arg("--rustc");
        }
        command.arg("--version");

        let output = match command.output() {
            Ok(output) => output,
            Err(e) => {
                let rustc = rustc.to_string_lossy();
                eprintln!("Error: failed to run `{} --version`: {}", rustc, e);
                process::exit(1);
            }
        };

        let string = match String::from_utf8(output.stdout) {
            Ok(string) => string,
            Err(e) => {
                let rustc = rustc.to_string_lossy();
                eprintln!(
                    "Error: failed to parse output of `{} --version`: {}",
                    rustc, e,
                );
                process::exit(1);
            }
        };

        // Meta-local overlay: RUSTC_BOOTSTRAP=1 unlocks nightly features on a
        // stable rustc. The rest of the build will treat this rustc as a
        // capable-of-nightly compiler, so promote a `Stable` parse to `Nightly`
        // (using the rustc release date already embedded in the version line)
        // before the parser runs. This makes date-based gates like
        // #[rustversion::since(YYYY-MM-DD)] match the rustc's release date.
        let string = if env::var_os("RUSTC_BOOTSTRAP").as_deref() == Some(std::ffi::OsStr::new("1"))
        {
            promote_stable_to_nightly(&string)
        } else {
            string
        };

        break match rustc::parse(&string) {
            rustc::ParseResult::Success(version) => version,
            rustc::ParseResult::OopsClippy if !is_clippy_driver => {
                is_clippy_driver = true;
                continue;
            }
            rustc::ParseResult::OopsMirai if !is_mirai && rustc_wrapper.is_some() => {
                is_mirai = true;
                continue;
            }
            rustc::ParseResult::Unrecognized
            | rustc::ParseResult::OopsClippy
            | rustc::ParseResult::OopsMirai => {
                eprintln!(
                    "Error: unexpected output from `rustc --version`: {:?}\n\n\
                    Please file an issue in https://github.com/dtolnay/rustversion",
                    string
                );
                process::exit(1);
            }
        };
    };

    if version.minor < 38 {
        // Prior to 1.38, a #[proc_macro] is not allowed to be named `cfg`.
        println!("cargo:rustc-cfg=cfg_macro_not_allowed");
    }

    if version.minor >= 80 {
        println!("cargo:rustc-check-cfg=cfg(cfg_macro_not_allowed)");
        println!("cargo:rustc-check-cfg=cfg(host_os, values(\"windows\"))");
    }

    let version = format!("{:#}\n", Render(&version));
    let out_dir = env::var_os("OUT_DIR").expect("OUT_DIR not set");
    let out_file = Path::new(&out_dir).join("version.expr");
    fs::write(out_file, version).expect("failed to write version.expr");

    let host = env::var_os("HOST").expect("HOST not set");
    if let Some("windows") = host.to_str().unwrap().split('-').nth(2) {
        println!("cargo:rustc-cfg=host_os=\"windows\"");
    }
}

// Rewrites `rustc 1.X.Y (HASH DATE)` to `rustc 1.X.Y-nightly (HASH DATE)` so
// the existing parser's nightly branch picks up the date. No-op if a channel
// suffix is already present (`-nightly`, `-beta`, `-dev`).
fn promote_stable_to_nightly(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + "-nightly".len());
    let mut promoted = false;
    for line in input.split_inclusive('\n') {
        if !promoted {
            if let Some(rewritten) = try_promote_line(line) {
                out.push_str(&rewritten);
                promoted = true;
                continue;
            }
        }
        out.push_str(line);
    }
    out
}

fn try_promote_line(line: &str) -> Option<String> {
    let trimmed = line.trim_end_matches(|c| c == '\r' || c == '\n');
    let trailing = &line[trimmed.len()..];
    let mut words = trimmed.split(' ');
    if words.next()? != "rustc" {
        return None;
    }
    let version = words.next()?;
    if version.contains('-') {
        // Already has a channel suffix.
        return None;
    }
    let rest = words.collect::<Vec<_>>().join(" ");
    let mut rewritten = String::with_capacity(line.len() + "-nightly".len());
    rewritten.push_str("rustc ");
    rewritten.push_str(version);
    rewritten.push_str("-nightly");
    if !rest.is_empty() {
        rewritten.push(' ');
        rewritten.push_str(&rest);
    }
    rewritten.push_str(trailing);
    Some(rewritten)
}

// Shim Version's {:?} format into a {} format, because {:?} is unusable in
// format strings when building with `-Zfmt-debug=none`.
struct Render<'a>(&'a rustc::Version);

impl<'a> Display for Render<'a> {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        Debug::fmt(self.0, formatter)
    }
}
