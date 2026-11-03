use std::path::PathBuf;

use clap::Subcommand;
use starbreaker_datacore::database::Database;

use crate::common::{load_dcb_bytes, ExportOpts};
use crate::error::{CliError, Result};

#[derive(Subcommand)]
pub enum SocpakCommand {
    /// Export socpak interior containers to GLB
    Export {
        /// P4k path substring for socpak files (case-insensitive)
        pattern: String,
        /// Output .glb path
        output: Option<PathBuf>,
        /// Path to Data.p4k
        #[arg(long, env = "SC_DATA_P4K")]
        p4k: Option<PathBuf>,
        #[command(flatten)]
        opts: ExportOpts,
    },
    /// Export many socpaks, one GLB each, in a single run: the P4k and DataCore are opened once and the meshes the
    /// socpaks share are loaded once (one line per socpak in the list: its file name without ".socpak", or a P4k
    /// path, optionally followed by a tab and the output name; the output is <output dir>/<name>.glb). A bare name
    /// takes the first socpak of that name in the P4k: several share a name (two util_cmpd_wrhse_lge_001_int, 3 m
    /// apart in height), so give the path for those. Socpaks whose GLB already exists are skipped unless --overwrite.
    /// One line per export on stdout: "ok\t<name>\t<bytes>" or "FAIL\t<name>\t<error>"
    ExportList {
        /// Text file listing the socpaks
        list: PathBuf,
        /// Output directory
        output_dir: PathBuf,
        /// Path to Data.p4k
        #[arg(long, env = "SC_DATA_P4K")]
        p4k: Option<PathBuf>,
        /// Export again socpaks whose GLB already exists
        #[arg(long)]
        overwrite: bool,
        /// Socpaks per batch: each batch loads its meshes, writes its GLBs and frees them before the next, so memory
        /// stays bounded on long lists (the 128 QV Logistics Station modules in one batch peaked at 3.3 GB)
        #[arg(long, default_value_t = 50)]
        batch: usize,
        #[command(flatten)]
        opts: ExportOpts,
    },
}

impl SocpakCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Export {
                pattern,
                output,
                p4k,
                opts,
            } => export(pattern, output, p4k, opts),
            Self::ExportList {
                list,
                output_dir,
                p4k,
                overwrite,
                batch,
                opts,
            } => export_list(list, output_dir, p4k, overwrite, batch, opts),
        }
    }
}

fn export(
    pattern: String,
    output: Option<PathBuf>,
    p4k_path: Option<PathBuf>,
    opts: ExportOpts,
) -> Result<()> {
    let (p4k, dcb_bytes) = load_dcb_bytes(p4k_path.as_deref(), None)?;
    let p4k = p4k.ok_or_else(|| CliError::MissingRequirement("P4k required for socpak export".into()))?;
    let db = Database::from_bytes(&dcb_bytes)?;

    let search_lower = pattern.to_lowercase();
    let socpak_paths: Vec<String> = p4k
        .entries()
        .iter()
        .filter(|e| {
            let name = e.name.to_lowercase();
            name.contains(&search_lower) && name.ends_with(".socpak")
        })
        .map(|e| e.name.clone())
        .collect();

    if socpak_paths.is_empty() {
        return Err(CliError::NotFound(format!("no .socpak files matching '{pattern}'")));
    }

    eprintln!("Found {} socpak files", socpak_paths.len());
    let export_opts = starbreaker_3d::ExportOptions::from(&opts);
    let glb = starbreaker_3d::socpaks_to_glb(&db, &p4k, &socpak_paths, &export_opts)?;

    let output = output.unwrap_or_else(|| PathBuf::from(format!("{pattern}.glb")));
    std::fs::write(&output, &glb)
        .map_err(|e| CliError::IoPath { source: e, path: output.display().to_string() })?;
    eprintln!("Written {} bytes to {}", glb.len(), output.display());
    Ok(())
}

fn export_list(
    list: PathBuf,
    output_dir: PathBuf,
    p4k_path: Option<PathBuf>,
    overwrite: bool,
    batch: usize,
    opts: ExportOpts,
) -> Result<()> {
    let text = std::fs::read_to_string(&list)
        .map_err(|e| CliError::IoPath { source: e, path: list.display().to_string() })?;
    std::fs::create_dir_all(&output_dir)
        .map_err(|e| CliError::IoPath { source: e, path: output_dir.display().to_string() })?;

    let (p4k, dcb_bytes) = load_dcb_bytes(p4k_path.as_deref(), None)?;
    let p4k = p4k.ok_or_else(|| CliError::MissingRequirement("P4k required for socpak export".into()))?;
    let db = Database::from_bytes(&dcb_bytes)?;

    // socpak file name (lower case) -> its P4k path, for list lines given as a bare name
    let mut by_name: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for e in p4k.entries() {
        let lower = e.name.to_lowercase();
        if let Some(stem) = lower.strip_suffix(".socpak") {
            let name = stem.rsplit(['\\', '/']).next().unwrap_or(stem).to_string();
            by_name.entry(name).or_insert_with(|| e.name.clone());
        }
    }

    let mut names = Vec::new();
    let mut groups = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (socpak, out_name) = match line.split_once('\t') {
            Some((s, n)) => (s.trim(), Some(n.trim())),
            None => (line, None),
        };
        let lower = socpak.to_lowercase().replace('/', "\\");
        let stem = lower.strip_suffix(".socpak").unwrap_or(&lower);
        let name = out_name
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| stem.rsplit('\\').next().unwrap_or(stem).to_string());
        if !overwrite && output_dir.join(format!("{name}.glb")).exists() {
            continue;
        }
        let path = if stem.contains('\\') {
            p4k.entries().iter().find(|e| e.name.to_lowercase() == format!("{stem}.socpak")).map(|e| e.name.clone())
        } else {
            by_name.get(&name).cloned()
        };
        match path {
            Some(path) => {
                names.push(name);
                groups.push(vec![path]);
            }
            None => println!("FAIL\t{name}\tnot found in the P4k"),
        }
    }
    eprintln!("{} socpaks to export", groups.len());

    let export_opts = starbreaker_3d::ExportOptions::from(&opts);
    let mut failures = 0;
    let batch = batch.max(1);
    for (b, chunk) in groups.chunks(batch).enumerate() {
        let base = b * batch;
        starbreaker_3d::socpaks_to_glbs(&db, &p4k, chunk, &export_opts, |i, glb| {
            let name = &names[base + i];
            let result = glb.map_err(|e| e.to_string()).and_then(|bytes| {
                std::fs::write(output_dir.join(format!("{name}.glb")), &bytes)
                    .map(|_| bytes.len())
                    .map_err(|e| e.to_string())
            });
            match result {
                Ok(n) => println!("ok\t{name}\t{n}"),
                Err(e) => {
                    failures += 1;
                    println!("FAIL\t{name}\t{e}");
                }
            }
        })?;
    }
    eprintln!("{} written, {} failed", groups.len() - failures, failures);
    Ok(())
}
