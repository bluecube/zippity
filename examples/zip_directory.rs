use std::path::PathBuf;

use chrono;
use clap::Parser;
use tokio::{fs::File, io};
use zippity::Builder;

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

    let top_level_dir_name = args
        .source_dir
        .file_name()
        .map(|x| x.to_string_lossy().into_owned());

    let mut builder = Builder::new();
    builder.system_time_timezone(chrono::Local);
    builder
        .add_directory_recursive(args.source_dir, top_level_dir_name.as_deref())
        .await?;
    let mut zippity = builder.build();

    println!("Zip file will be {} B large", zippity.size());

    let mut output = File::create(args.output).await?;
    io::copy(&mut zippity, &mut output).await?;

    Ok(())
}
