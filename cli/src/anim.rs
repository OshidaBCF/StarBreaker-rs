//! Animation inspection CLI.
//!
//! `dump` prints a `.caf` or `.aim` (one clip; an aim pose has a frame per aim direction) or `.dba` (a
//! database of clips) as JSON: per clip its name, fps and
//! frame count, and per bone channel its keyframes (first / last, or all with `--all-keyframes`). With
//! `--skeleton` (a `.chr`, or a `.cga` / `.cgam` scene graph) the bones' CRC32 hashes are resolved to names.
//! The same output as the MCP server's `dba_dump` tool.

use std::path::PathBuf;

use clap::Subcommand;

use crate::common::load_p4k;
use crate::error::{CliError, Result};

#[derive(Subcommand)]
pub enum AnimCommand {
    /// Dump a `.caf` / `.aim` / `.dba` animation as JSON.
    Dump {
        /// Path to a `.caf` / `.aim` / `.dba` file, on disk or inside the P4k (`Data/Animations/...`).
        path: String,
        /// Path to Data.p4k (only required when reading P4k-internal paths).
        #[arg(long, env = "SC_DATA_P4K")]
        p4k: Option<PathBuf>,
        /// Skeleton (`.chr`, `.cga`, `.cgam`) whose bone names resolve the channels' hashes.
        #[arg(long)]
        skeleton: Option<String>,
        /// Only clips whose name contains this (case-insensitive).
        #[arg(long)]
        filter: Option<String>,
        /// Only channels whose bone name contains this (case-insensitive; needs --skeleton).
        #[arg(long)]
        bones: Option<String>,
        /// Every keyframe, not only the first and last.
        #[arg(long)]
        all_keyframes: bool,
    },
}

impl AnimCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Dump { path, p4k, skeleton, filter, bones, all_keyframes } => {
                dump(&path, p4k.as_ref(), skeleton.as_deref(), filter.as_deref(), bones.as_deref(), all_keyframes)
            }
        }
    }
}

fn read_input(path: &str, p4k_path: Option<&PathBuf>) -> Result<Vec<u8>> {
    let direct = std::path::Path::new(path);
    if direct.is_file() {
        return std::fs::read(direct).map_err(|e| CliError::IoPath { source: e, path: direct.display().to_string() });
    }
    let p4k = load_p4k(p4k_path.map(|p| p.as_path()))
        .map_err(|e| CliError::InvalidInput(format!("p4k open failed: {e}")))?;
    let entry = p4k
        .entry_case_insensitive(&path.replace('/', "\\"))
        .or_else(|| p4k.entry_case_insensitive(path))
        .ok_or_else(|| CliError::NotFound(format!("'{path}' not found in P4k")))?;
    p4k.read(entry).map_err(|e| CliError::InvalidInput(format!("p4k read failed: {e}")))
}

fn dump(
    path: &str,
    p4k: Option<&PathBuf>,
    skeleton: Option<&str>,
    filter: Option<&str>,
    bones: Option<&str>,
    all_keyframes: bool,
) -> Result<()> {
    let bytes = read_input(path, p4k)?;
    let lower = path.to_ascii_lowercase();
    let db = if lower.ends_with(".caf") || lower.ends_with(".aim") {
        starbreaker_3d::animation::parse_caf(&bytes)
    } else {
        starbreaker_3d::animation::parse_dba(&bytes)
    }
    .map_err(|e| CliError::InvalidInput(format!("parse failed for {path}: {e}")))?;
    let mut hash_to_name = std::collections::HashMap::new();
    let mut skeleton_bones = Vec::new();
    if let Some(skeleton) = skeleton {
        let data = read_input(skeleton, p4k)?;
        let names = starbreaker_3d::skeleton::parse_rig_node_names(&data).ok_or_else(|| {
            CliError::InvalidInput(format!("'{skeleton}' has no CompiledBones (CHR) or NMC (CGA/CGAM) chunk"))
        })?;
        for name in names {
            hash_to_name.insert(starbreaker_3d::animation::bone_name_hash(&name), name);
        }
        skeleton_bones = starbreaker_3d::skeleton::parse_skeleton(&data).unwrap_or_default();
    }
    let mut value = starbreaker_3d::animation::dump_database_to_json(&db, &hash_to_name, filter, bones, all_keyframes);
    // A CHR's whole skeleton, for a reader to play the clip on: the keys are each bone's rotation and position
    // relative to its parent here, and a bone without keys keeps this rest pose. Rotations x, y, z, w, like
    // the keys; world_*: in the model, the bind pose of the skins made for it
    if !skeleton_bones.is_empty() {
        let xyzw = |[w, x, y, z]: [f32; 4]| [x, y, z, w];
        let list: Vec<serde_json::Value> = skeleton_bones
            .iter()
            .map(|b| {
                serde_json::json!({
                    "name": b.name,
                    "parent": b.parent_index.and_then(|i| skeleton_bones.get(i as usize)).map(|p| p.name.clone()),
                    "position": b.local_position,
                    "rotation": xyzw(b.local_rotation),
                    "world_position": b.world_position,
                    "world_rotation": xyzw(b.world_rotation),
                })
            })
            .collect();
        if let Some(obj) = value.as_object_mut() {
            obj.insert("skeleton".to_string(), serde_json::Value::Array(list));
        }
    }
    println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
    Ok(())
}
