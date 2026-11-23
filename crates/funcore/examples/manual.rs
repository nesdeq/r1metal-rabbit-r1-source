//! Writes a Markdown manual as the device writes its documents: `manual SOURCE.md OUT.pdf`.

use r1_funcore::domain::paper::pages::{Head, to_document};
use r1_funcore::gfx::palette::Roles;
use std::path::Path;
use std::process::ExitCode;
use std::{env, fs};

const USAGE: &str = "usage: manual SOURCE.md OUT.pdf";
const TITLE: &str = "Manual of r1metal";
const NAME: &str = "MANUAL";

fn run() -> Result<(), String> {
    let arguments: Vec<String> = env::args().skip(1).collect();
    let [source, out] = arguments.as_slice() else {
        return Err(USAGE.into());
    };
    let text = fs::read_to_string(source).map_err(|e| format!("{source}: {e}"))?;
    let file = Path::new(source).file_name().map_or_else(|| source.clone(), |name| name.to_string_lossy().into());
    let line = format!("r1metal {}", env!("CARGO_PKG_VERSION"));
    let head = Head { title: TITLE, name: NAME, line: &line, source: &file };
    fs::write(out, to_document(head, &text, None, &Roles::default())).map_err(|e| format!("{out}: {e}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("manual: {e}");
            ExitCode::FAILURE
        }
    }
}
