//! Utility functions for the WebAssembly module
use anyhow::{bail, Result};
use std::env;
use std::path::PathBuf;

/// Whether or not Wasmer should print with color
pub fn wasmer_should_print_color() -> bool {
    env::var("WASMER_COLOR")
        .ok()
        .and_then(|inner| inner.parse::<bool>().ok())
        .unwrap_or_else(|| atty::is(atty::Stream::Stdout))
}

/// Parses a mapdir from a string
pub fn parse_mapdir(entry: &str) -> Result<(String, PathBuf)> {
    if let [alias, real_dir] = entry.split(':').collect::<Vec<&str>>()[..] {
        let pb = PathBuf::from(&real_dir);
        if let Ok(pb_metadata) = pb.metadata() {
            if !pb_metadata.is_dir() {
                bail!("\"{}\" exists, but it is not a directory", &real_dir);
            }
        } else {
            bail!("Directory \"{}\" does not exist", &real_dir);
        }
        return Ok((alias.to_string(), pb));
    }
    bail!(
        "Directory mappings must consist of two paths separate by a colon. Found {}",
        &entry
    )
}

/// Parses a mapdir from an env var
pub fn parse_envvar(entry: &str) -> Result<(String, String)> {
    if let [env_var, value] = entry.split('=').collect::<Vec<&str>>()[..] {
        return Ok((env_var.to_string(), value.to_string()));
    } else {
        bail!(
            "Env vars must be of the form <var_name>=<value>. Found {}",
            &entry
        );
    }
}
