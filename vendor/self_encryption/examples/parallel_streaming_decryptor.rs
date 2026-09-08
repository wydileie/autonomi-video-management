use bytes::Bytes;
use clap::Parser;
use rayon::prelude::*;
use self_encryption::{deserialize, streaming_decrypt, DataMap, Error, Result};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
use xor_name::XorName;

/// Parallel streaming decryptor for self-encrypted files
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the data map file
    #[arg(short, long, required = true)]
    data_map: String,

    /// Directory containing the encrypted chunks
    #[arg(short, long, required = true)]
    chunks_dir: String,

    /// Path where the decrypted file should be written
    #[arg(short, long, required = true)]
    output: String,
}

fn validate_paths(args: &Args) -> Result<()> {
    if !Path::new(&args.data_map).exists() {
        return Err(Error::Generic(format!(
            "Data map file does not exist: {}",
            args.data_map
        )));
    }

    let chunks_dir = Path::new(&args.chunks_dir);
    if !chunks_dir.exists() {
        return Err(Error::Generic(format!(
            "Chunks directory does not exist: {}",
            args.chunks_dir
        )));
    }
    if !chunks_dir.is_dir() {
        return Err(Error::Generic(format!(
            "Chunks path is not a directory: {}",
            args.chunks_dir
        )));
    }

    let output_path = Path::new(&args.output);
    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            return Err(Error::Generic(format!(
                "Output directory does not exist: {}",
                parent.display()
            )));
        }
        if !parent
            .metadata()
            .map(|m| m.permissions().readonly())
            .unwrap_or(true)
        {
            return Err(Error::Generic(format!(
                "Output directory is not writable: {}",
                parent.display()
            )));
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    validate_paths(&args)?;

    let data_map = load_data_map(&args.data_map)?;

    let get_chunk_parallel = |hashes: &[(usize, XorName)]| -> Result<Vec<(usize, Bytes)>> {
        hashes
            .par_iter()
            .map(|(i, hash)| {
                let chunk_path = Path::new(&args.chunks_dir).join(hex::encode(hash));
                let mut chunk_data = Vec::new();
                File::open(&chunk_path)
                    .and_then(|mut file| file.read_to_end(&mut chunk_data))
                    .map_err(|e| Error::Generic(format!("Failed to read chunk: {e}")))?;
                Ok((*i, Bytes::from(chunk_data)))
            })
            .collect()
    };

    // Use the streaming decryption iterator
    let stream = streaming_decrypt(&data_map, get_chunk_parallel)?;

    let mut output_file = File::create(&args.output)
        .map_err(|e| Error::Generic(format!("Failed to create output file: {e}")))?;

    for chunk_result in stream {
        let chunk = chunk_result?;
        output_file
            .write_all(&chunk)
            .map_err(|e| Error::Generic(format!("Failed to write output: {e}")))?;
    }

    println!("Successfully decrypted file to: {}", args.output);

    Ok(())
}

fn load_data_map(path: &str) -> Result<DataMap> {
    let mut file =
        File::open(path).map_err(|e| Error::Generic(format!("Failed to open data map: {e}")))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)
        .map_err(|e| Error::Generic(format!("Failed to read data map: {e}")))?;
    deserialize(&data)
}
