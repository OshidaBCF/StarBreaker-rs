//! Animation inspection CLI.
//!
//! `dump` prints a `.caf` or `.aim` (one clip; an aim pose has a frame per aim direction) or `.dba` (a
//! database of clips) as JSON: per clip its name, fps and
//! frame count, and per bone channel its keyframes (first / last, or all with `--all-keyframes`). With
//! `--skeleton` (a `.chr`, or a `.cga` / `.cgam` scene graph) the bones' CRC32 hashes are resolved to names.
//! The same output as the MCP server's `dba_dump` tool.
//!
//! `dump-list` does many dumps in one go, the P4k opened once (opening it costs about 1.5 s, more than most
//! dumps): a JSON list of jobs, each written to its own file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use clap::Subcommand;
use starbreaker_p4k::MappedP4k;

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
    /// Many dumps, the P4k opened once. The list is a JSON array of jobs:
    /// `{"path", "out", "skeleton"?, "clips"?: [names], "all_keyframes"?: bool}`. Each job's dump (as `dump`
    /// prints it, compact) goes to `<out_dir>/<out>`, or `{"error": "..."}` when it fails. `clips`: only the
    /// clips with these names (file name without extension, any case); none given: every clip.
    DumpList {
        /// The JSON list of jobs.
        list: PathBuf,
        /// Where the dumps are written.
        out_dir: PathBuf,
        /// Path to Data.p4k (only required when reading P4k-internal paths).
        #[arg(long, env = "SC_DATA_P4K")]
        p4k: Option<PathBuf>,
    },
}

impl AnimCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Dump { path, p4k, skeleton, filter, bones, all_keyframes } => {
                let mut source = Source::new(p4k.as_ref());
                let rig = skeleton.as_deref().map(|s| Rig::load(&mut source, s)).transpose()?;
                let value = dump(&mut source, &path, rig.as_ref(), filter.as_deref(), None, bones.as_deref(), all_keyframes)?;
                println!("{}", serde_json::to_string_pretty(&value).unwrap_or_default());
                Ok(())
            }
            Self::DumpList { list, out_dir, p4k } => dump_list(&list, &out_dir, p4k.as_ref()),
        }
    }
}

/// Where the files are read: the disk, else the P4k (opened when first needed, then kept)
struct Source<'a> {
    p4k_path: Option<&'a PathBuf>,
    p4k: Option<MappedP4k>,
}

impl<'a> Source<'a> {
    fn new(p4k_path: Option<&'a PathBuf>) -> Self {
        Self { p4k_path, p4k: None }
    }

    fn read(&mut self, path: &str) -> Result<Vec<u8>> {
        let direct = Path::new(path);
        if direct.is_file() {
            return std::fs::read(direct).map_err(|e| CliError::IoPath { source: e, path: direct.display().to_string() });
        }
        if self.p4k.is_none() {
            let p4k = load_p4k(self.p4k_path.map(|p| p.as_path()))
                .map_err(|e| CliError::InvalidInput(format!("p4k open failed: {e}")))?;
            self.p4k = Some(p4k);
        }
        let p4k = self.p4k.as_ref().expect("opened above");
        let entry = p4k
            .entry_case_insensitive(&path.replace('/', "\\"))
            .or_else(|| p4k.entry_case_insensitive(path))
            .ok_or_else(|| CliError::NotFound(format!("'{path}' not found in P4k")))?;
        p4k.read(entry).map_err(|e| CliError::InvalidInput(format!("p4k read failed: {e}")))
    }
}

/// A skeleton read for dumps: its bones' names by hash, and (a CHR) the whole skeleton as `dump` writes it
struct Rig {
    hash_to_name: HashMap<u32, String>,
    skeleton: Option<serde_json::Value>,
}

impl Rig {
    fn load(source: &mut Source, skeleton: &str) -> Result<Self> {
        let data = source.read(skeleton)?;
        let names = starbreaker_3d::skeleton::parse_rig_node_names(&data).ok_or_else(|| {
            CliError::InvalidInput(format!("'{skeleton}' has no CompiledBones (CHR) or NMC (CGA/CGAM) chunk"))
        })?;
        let hash_to_name = names.into_iter().map(|n| (starbreaker_3d::animation::bone_name_hash(&n), n)).collect();
        // A CHR's whole skeleton, for a reader to play the clip on: the keys are each bone's rotation and position
        // relative to its parent here, and a bone without keys keeps this rest pose. Rotations x, y, z, w, like
        // the keys; world_*: in the model, the bind pose of the skins made for it
        let bones = starbreaker_3d::skeleton::parse_skeleton(&data).unwrap_or_default();
        let xyzw = |[w, x, y, z]: [f32; 4]| [x, y, z, w];
        let skeleton = (!bones.is_empty()).then(|| {
            serde_json::Value::Array(
                bones
                    .iter()
                    .map(|b| {
                        serde_json::json!({
                            "name": b.name,
                            "parent": b.parent_index.and_then(|i| bones.get(i as usize)).map(|p| p.name.clone()),
                            "position": b.local_position,
                            "rotation": xyzw(b.local_rotation),
                            "world_position": b.world_position,
                            "world_rotation": xyzw(b.world_rotation),
                        })
                    })
                    .collect(),
            )
        });
        Ok(Self { hash_to_name, skeleton })
    }
}

/// A clip's short name, as `clips` lists them: its file name without extension, lower case
fn clip_key(name: &str) -> String {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    file.rsplit_once('.').map_or(file, |(stem, _)| stem).to_ascii_lowercase()
}

fn dump(
    source: &mut Source,
    path: &str,
    rig: Option<&Rig>,
    filter: Option<&str>,
    clips: Option<&HashSet<String>>,
    bones: Option<&str>,
    all_keyframes: bool,
) -> Result<serde_json::Value> {
    let bytes = source.read(path)?;
    let lower = path.to_ascii_lowercase();
    let db = if lower.ends_with(".caf") || lower.ends_with(".aim") {
        starbreaker_3d::animation::parse_caf(&bytes)
    } else {
        starbreaker_3d::animation::parse_dba(&bytes)
    }
    .map_err(|e| CliError::InvalidInput(format!("parse failed for {path}: {e}")))?;
    let no_names = HashMap::new();
    let hash_to_name = rig.map_or(&no_names, |r| &r.hash_to_name);
    let mut value = match clips {
        Some(clips) => {
            let keep = |c: &starbreaker_3d::animation::AnimationClip| clips.contains(&clip_key(&c.name));
            starbreaker_3d::animation::dump_database_clips_to_json(&db, hash_to_name, keep, bones, all_keyframes)
        }
        None => starbreaker_3d::animation::dump_database_to_json(&db, hash_to_name, filter, bones, all_keyframes),
    };
    if let (Some(skeleton), Some(obj)) = (rig.and_then(|r| r.skeleton.clone()), value.as_object_mut()) {
        obj.insert("skeleton".to_string(), skeleton);
    }
    Ok(value)
}

fn dump_list(list: &Path, out_dir: &Path, p4k: Option<&PathBuf>) -> Result<()> {
    let text = std::fs::read_to_string(list).map_err(|e| CliError::IoPath { source: e, path: list.display().to_string() })?;
    let jobs: Vec<serde_json::Value> = serde_json::from_str(&text)
        .map_err(|e| CliError::InvalidInput(format!("{}: not a JSON list of jobs: {e}", list.display())))?;
    std::fs::create_dir_all(out_dir).map_err(|e| CliError::IoPath { source: e, path: out_dir.display().to_string() })?;
    let mut source = Source::new(p4k);
    let mut rigs: HashMap<String, std::result::Result<Rig, String>> = HashMap::new();
    let mut failed = 0;
    for (i, job) in jobs.iter().enumerate() {
        let field = |k: &str| job.get(k).and_then(|v| v.as_str());
        let (Some(path), Some(out)) = (field("path"), field("out")) else {
            return Err(CliError::InvalidInput(format!("job {i}: needs \"path\" and \"out\"")));
        };
        let clips: Option<HashSet<String>> = job
            .get("clips")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|c| c.as_str()).map(clip_key).collect());
        let all_keyframes = job.get("all_keyframes").and_then(|v| v.as_bool()).unwrap_or(false);
        let mut result = Ok(None);
        if let Some(skeleton) = field("skeleton") {
            if !rigs.contains_key(skeleton) {
                let rig = Rig::load(&mut source, skeleton).map_err(|e| e.to_string());
                rigs.insert(skeleton.to_string(), rig);
            }
            result = rigs[skeleton].as_ref().map(Some).map_err(|e| e.clone());
        }
        let value = result.and_then(|rig| {
            dump(&mut source, path, rig, None, clips.as_ref(), None, all_keyframes).map_err(|e| e.to_string())
        });
        let value = value.unwrap_or_else(|e| {
            failed += 1;
            serde_json::json!({ "error": e })
        });
        let target = out_dir.join(out);
        let file = std::fs::File::create(&target).map_err(|e| CliError::IoPath { source: e, path: target.display().to_string() })?;
        serde_json::to_writer(std::io::BufWriter::new(file), &value)
            .map_err(|e| CliError::InvalidInput(format!("{}: {e}", target.display())))?;
    }
    println!("{} dumps, {failed} failed", jobs.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::clip_key;

    #[test]
    fn clip_key_is_the_file_name_without_extension_in_lower_case() {
        assert_eq!(clip_key("Animations/Characters/Human/male_v7/Weapons/Stocked_Alerted_Stand_Idle.caf"), "stocked_alerted_stand_idle");
        assert_eq!(clip_key("a\\b\\Pistol_Draw.CAF"), "pistol_draw");
        assert_eq!(clip_key("clip_0"), "clip_0");
    }
}
