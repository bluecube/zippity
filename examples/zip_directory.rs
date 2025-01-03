use std::path::{Path, PathBuf};

use clap::Parser;
use tokio::{fs::File, io};
use zippity::{Builder, TokioFileEntry};

#[derive(Parser)]
#[command(about, long_about = None)]
struct Args {
    // Source directory
    source_dir: PathBuf,
    // Destination zipfile
    output: PathBuf,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let builder = gather_metadata(&args.source_dir)?;
    let mut zippity = builder.build();

    println!("Zip file will be {} B large", zippity.size());

    let mut output = File::create(args.output).await?;
    io::copy(&mut zippity, &mut output).await?;
    drop(output);

    Ok(())
}

/// Gathers metadata from files in a directory into a new Builder.
fn gather_metadata(directory: &Path) -> std::io::Result<Builder<TokioFileEntry>> {
    let mut builder: Builder<TokioFileEntry> = Builder::new();

    for entry in walkdir::WalkDir::new(directory) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.into_path();
        let entry_name = path
            .strip_prefix(directory)
            .unwrap()
            .to_string_lossy()
            .into_owned();

        builder.add_entry(entry_name, path)?;
    }

    Ok(builder)
}
