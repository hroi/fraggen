use clap::Parser;
use eyre::Result;
use std::{
    io::{stdout, BufWriter},
    path::PathBuf,
};

/// Generate fragments for every type in your GraphQL schema
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Schema filename
    #[arg(long)]
    schema: PathBuf,

    /// Fragment name prefix
    #[arg(long, default_value = "")]
    prefix: String,

    /// Fragment name suffix
    #[arg(long, default_value = "Fields")]
    suffix: String,

    /// Add __typename to object fragments
    #[arg(long)]
    typename: bool,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let args = Cli::parse();

    let schema_content = std::fs::read_to_string(args.schema)?;

    let output = BufWriter::new(stdout().lock());
    fraggen::generate(
        &schema_content,
        output,
        &args.prefix,
        &args.suffix,
        args.typename,
    )?;

    Ok(())
}
