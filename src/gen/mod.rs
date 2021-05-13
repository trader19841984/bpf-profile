//! bpf-profile generate command implementation.

mod dump;
mod output;
mod profile;
mod trace;

use profile::Profile;
use std::io::BufWriter;
use std::path::PathBuf;
//use tracing::info;

/// Runs the conversion from trace to a profiler output.
pub fn run(
    trace_file: PathBuf,
    dump_file: Option<PathBuf>,
    output_file: Option<PathBuf>,
    _: String, // always 'callgrind' currently
) -> Result<()> {
    if !trace::contains_standard_header(&trace_file)? {
        return Err(Error::TraceFormat(trace_file));
    }

    let dump = dump::read(dump_file)?;
    let profile = Profile::create(trace_file, &dump)?;

    match output_file {
        None => profile.write_callgrind(std::io::stdout()),
        Some(output_file) => {
            let output = output::open_w(output_file)?;
            profile.write_callgrind(BufWriter::new(output))
        }
    }
}

/// Represents errors of the converter.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Unsupported file name '{0}'")]
    Filename(PathBuf),

    #[error("Cannot open file '{1}': {0}")]
    OpenFile(#[source] std::io::Error, PathBuf),
    #[error("Cannot read line '{1}': {0}")]
    ReadLine(#[source] std::io::Error, String),

    #[error("Unsupported format of trace file '{0}'")]
    TraceFormat(PathBuf),
    #[error("Skipped input")]
    Skipped,
    #[error("Cannot parse trace '{0}' at line {1}")]
    Parsing(String, usize),
    #[error("Instruction is not a call: '{0}'")]
    NotCall(String),
    //#[error("Stack is empty on exit")]
    //EmptyStack,
    #[error("Input/output error")]
    Io(#[from] std::io::Error),
}

/// Represents results.
pub type Result<T> = std::result::Result<T, Error>;
