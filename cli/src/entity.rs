use std::collections::HashSet;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use starbreaker_datacore::database::Database;
use starbreaker_datacore::loadout::{EntityIndex, LoadoutNode, resolve_loadout_indexed};
use starbreaker_datacore::types::Record;

use crate::common::{ExportOpts, load_dcb_bytes};
use crate::error::{CliError, Result};

fn bundled_extension(format: starbreaker_3d::ExportFormat) -> &'static str {
    match format {
        starbreaker_3d::ExportFormat::Glb => "glb",
        starbreaker_3d::ExportFormat::Stl => "stl",
    }
}

fn export_entity_name(name: &str) -> String {
    let trimmed = name.trim_matches('"');
    trimmed
        .rsplit('.')
        .next()
        .unwrap_or(trimmed)
        .to_string()
}

fn sanitize_export_name(name: &str) -> String {
    let mut cleaned = String::new();
    let mut last_was_space = false;

    for ch in name.chars() {
        if ch.is_alphanumeric() {
            cleaned.push(ch);
            last_was_space = false;
        } else if ch.is_whitespace() || matches!(ch, '_' | '-') {
            if !cleaned.is_empty() && !last_was_space {
                cleaned.push(' ');
                last_was_space = true;
            }
        }
    }

    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "Export".to_string()
    } else {
        cleaned.to_string()
    }
}

fn prepare_decomposed_output_root(output_root: &PathBuf, package_name: &str) -> Result<()> {
    if output_root.exists() {
        if output_root.is_file() {
            return Err(CliError::InvalidInput(format!(
                "decomposed output root '{}' already exists as a file",
                output_root.display(),
            )));
        }
    }

    let packages_root = output_root.join("Packages");
    let package_root = packages_root.join(package_name);
    if package_root.exists() {
        std::fs::remove_dir_all(&package_root)
            .map_err(|e| CliError::IoPath { source: e, path: package_root.display().to_string() })?;
    }

    std::fs::create_dir_all(&package_root)
        .map_err(|e| CliError::IoPath { source: e, path: package_root.display().to_string() })?;
    Ok(())
}

fn should_skip_existing_decomposed_asset(
    file: &starbreaker_3d::ExportedFile,
    skip_existing_assets: bool,
) -> bool {
    skip_existing_assets && file.kind.is_mesh_or_texture_asset()
}

fn write_decomposed_file(
    file: &starbreaker_3d::ExportedFile,
    output_path: &PathBuf,
    skip_existing_assets: bool,
) -> Result<()> {
    if output_path.exists() {
        if !output_path.is_file() {
            return Err(CliError::InvalidInput(format!(
                "decomposed output path '{}' already exists as a directory",
                output_path.display(),
            )));
        }
        if should_skip_existing_decomposed_asset(file, skip_existing_assets) {
            return Ok(());
        }
    }

    std::fs::write(output_path, &file.bytes)
        .map_err(|e| CliError::IoPath { source: e, path: output_path.display().to_string() })?;
    Ok(())
}

/// The mesh (.glb) and texture files already under `output_root`/Data; textures only with `textures_only`
/// (ExportOpts::rewrite_meshes: the meshes are written again).
fn collect_existing_decomposed_assets(output_root: &Path, textures_only: bool) -> Result<HashSet<String>> {
    let data_root = output_root.join("Data");
    let mut existing = HashSet::new();
    if !data_root.exists() {
        return Ok(existing);
    }

    let mut pending = vec![data_root];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| CliError::IoPath { source: e, path: dir.display().to_string() })?
        {
            let entry = entry
                .map_err(|e| CliError::IoPath { source: e, path: dir.display().to_string() })?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|e| CliError::IoPath { source: e, path: path.display().to_string() })?;
            if file_type.is_dir() {
                pending.push(path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let Some(extension) = path.extension().and_then(|ext| ext.to_str()) else {
                continue;
            };
            if !matches!(extension, "glb" | "png" | "zst") || (textures_only && extension == "glb") {
                continue;
            }

            let relative = path
                .strip_prefix(output_root)
                .map_err(|_| {
                    CliError::InvalidInput(format!(
                        "failed to compute relative decomposed asset path for '{}'",
                        path.display(),
                    ))
                })?
                .to_string_lossy()
                .replace('\\', "/")
                .to_ascii_lowercase();
            existing.insert(relative);
        }
    }

    Ok(existing)
}

#[derive(Subcommand)]
pub enum EntityCommand {
    /// Export entity to a bundled file
    Export {
        /// Entity name (substring, case-insensitive)
        name: String,
        /// Output bundled file path
        output: Option<PathBuf>,
        /// Path to Data.p4k
        #[arg(long, env = "SC_DATA_P4K")]
        p4k: Option<PathBuf>,
        /// Write hierarchy JSON instead of GLB
        #[arg(long)]
        dump_hierarchy: bool,
        /// Material to use instead of the entity's own (Objects/.../name.mtl), e.g. from another build's records
        #[arg(long)]
        material: Option<String>,
        /// Tint palette to use instead of the entity's own: a TintPaletteTree record name, or "none"
        #[arg(long)]
        palette: Option<String>,
        /// Mesh to export through the entity instead of its own (Objects/.../name.cgf), for meshes with no record
        /// of their own (trees, grass): the entity is only a carrier, its attachments are dropped
        #[arg(long)]
        geometry: Option<String>,
        #[command(flatten)]
        opts: ExportOpts,
    },
    /// Export many entities into one decomposed folder in a single run: the P4k and DataCore are read once,
    /// several exports run at once, and each mesh / texture is written once (one line per export on stdout:
    /// "ok\t<record>\t<body>\t<seconds>" or "FAIL\t<record>\t<body>\t<error>")
    ExportList {
        /// JSON list: [{"record": "<exact name>", "body": "male|female|prop", "material": "<.mtl path>",
        /// "palette": "<TintPaletteTree name|none>", "group": "<id>", "geometry": "<.cgf/.cga path>",
        /// "name": "<package name>"}]. Only record is required. Consecutive entries with the same group run one
        /// after the other on one worker (variants of one model share textures). geometry: that mesh is exported
        /// through the record (a carrier, its attachments dropped), for meshes with no record of their own
        /// (trees, grass); give it its material, and palette "none"
        list: PathBuf,
        /// Output folder (Data/ and Packages/ go in it)
        output: PathBuf,
        /// Path to Data.p4k
        #[arg(long, env = "SC_DATA_P4K")]
        p4k: Option<PathBuf>,
        /// Exports run at once
        #[arg(long, default_value = "4")]
        jobs: usize,
        #[command(flatten)]
        opts: ExportOpts,
    },
    /// Print entity loadout tree
    Loadout {
        /// Entity name (substring, case-insensitive)
        name: String,
        /// Path to Data.p4k
        #[arg(long, env = "SC_DATA_P4K")]
        p4k: Option<PathBuf>,
    },
}

impl EntityCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Export {
                name,
                output,
                p4k,
                dump_hierarchy,
                material,
                palette,
                geometry,
                opts,
            } => export(name, output, p4k, dump_hierarchy, material, palette, geometry, opts),
            Self::ExportList { list, output, p4k, jobs, opts } => export_list(list, output, p4k, jobs, opts),
            Self::Loadout { name, p4k } => loadout(name, p4k),
        }
    }
}

fn find_candidates<'a>(db: &'a Database, search: &str) -> Result<Vec<&'a Record>> {
    let search = search.to_lowercase();
    let entity_si = db
        .struct_id("EntityClassDefinition")
        .ok_or_else(|| CliError::NotFound("EntityClassDefinition struct not found in DCB".into()))?;
    let mut candidates: Vec<_> = db
        .records_of_type(entity_si)
        .filter(|r| {
            db.resolve_string2(r.name_offset)
                .to_lowercase()
                .contains(&search)
        })
        .collect();
    candidates.sort_by_key(|r| db.resolve_string2(r.name_offset).len());
    Ok(candidates)
}

fn export(
    name: String,
    output: Option<PathBuf>,
    p4k_path: Option<PathBuf>,
    dump_hierarchy: bool,
    material: Option<String>,
    palette: Option<String>,
    geometry: Option<String>,
    opts: ExportOpts,
) -> Result<()> {
    crate::log_mem_stats("start");
    let (p4k, dcb_bytes) = load_dcb_bytes(p4k_path.as_deref(), None)?;
    crate::log_mem_stats("after p4k+dcb load");
    let p4k = p4k.ok_or_else(|| CliError::MissingRequirement("P4k required for entity export".into()))?;
    let db = Database::from_bytes(&dcb_bytes)?;
    crate::log_mem_stats("after db parse");

    let candidates = find_candidates(&db, &name)?;
    if candidates.is_empty() {
        return Err(CliError::NotFound(format!("no EntityClassDefinition records matching '{name}'")));
    }

    let record = candidates[0];
    let rname = db.resolve_string2(record.name_offset);
    let export_name = sanitize_export_name(&export_entity_name(rname));
    if candidates.len() > 1 {
        eprintln!("Found {} candidates, using shortest match: {rname}", candidates.len());
    }

    let idx = EntityIndex::new(&db);
    let mut export_opts = starbreaker_3d::ExportOptions::from(&opts);
    let carried = geometry.is_some();
    if material.is_some() || palette.is_some() || geometry.is_some() {
        export_opts.overrides = Some(starbreaker_3d::EntityOverrides {
            record: export_entity_name(rname),
            material,
            palette,
            geometry,
        });
    }
    let output = output.unwrap_or_else(|| {
        match export_opts.kind {
            starbreaker_3d::ExportKind::Bundled => {
                PathBuf::from(format!("{export_name}.{}", bundled_extension(export_opts.format)))
            }
            starbreaker_3d::ExportKind::Decomposed => PathBuf::from(name.clone()),
        }
    });
    let existing_asset_paths = if export_opts.kind == starbreaker_3d::ExportKind::Decomposed
        && opts.skip_existing_assets
    {
        Some(collect_existing_decomposed_assets(&output, opts.rewrite_meshes)?)
    } else {
        None
    };

    crate::log_mem_stats("before loadout resolve");
    let mut tree = resolve_loadout_indexed(&idx, record);
    if carried {
        tree.root.children.clear();
    }
    crate::log_mem_stats("after loadout resolve");

    eprintln!("\nLoadout tree for {}:", tree.root.entity_name);
    for child in &tree.root.children {
        let g = if child.geometry_path.is_some() { "G" } else { "." };
        eprintln!("  {g} {} -> {}", child.item_port_name, child.entity_name);
    }

    if dump_hierarchy {
        let json = starbreaker_3d::dump_hierarchy(&db, &p4k, record, &tree);
        let json_path = output.with_extension("json");
        std::fs::write(&json_path, &json)
            .map_err(|e| CliError::IoPath { source: e, path: json_path.display().to_string() })?;
        eprintln!("Hierarchy written to {}", json_path.display());
        return Ok(());
    }

    crate::log_mem_stats("before export");
    let result = starbreaker_3d::assemble_glb_with_loadout_with_progress(
        &db,
        &p4k,
        record,
        &tree,
        &export_opts,
        None,
        existing_asset_paths.as_ref(),
    )?;
    crate::log_mem_stats("after export");
    eprintln!("Geometry: {}", result.geometry_path);
    eprintln!("Material: {}", result.material_path);
    match result.kind {
        starbreaker_3d::ExportKind::Bundled => {
            let bundled_bytes = result.bundled_bytes().ok_or_else(|| {
                CliError::InvalidInput(format!(
                    "entity export returned non-bundled output for {:?}",
                    result.kind,
                ))
            })?;
            eprintln!("Bundled export size: {} bytes", bundled_bytes.len());
            std::fs::write(&output, bundled_bytes)
                .map_err(|e| CliError::IoPath { source: e, path: output.display().to_string() })?;
        }
        starbreaker_3d::ExportKind::Decomposed => {
            let decomposed = result.decomposed.as_ref().ok_or_else(|| {
                CliError::InvalidInput("entity export returned no decomposed files".into())
            })?;
            eprintln!("Decomposed export file count: {}", decomposed.files.len());
            // Clean the exporter's own package folder (`<name>[_<body>]_LOD<n>_TEX<n>`, read from its
            // scene.json path) so we don't leave an empty sibling folder.
            let package_name = decomposed
                .files
                .iter()
                .find_map(|f| {
                    f.relative_path.strip_prefix("Packages/")?.strip_suffix("/scene.json").map(str::to_string)
                })
                .unwrap_or_else(|| format!("{export_name}_LOD{}_TEX{}", export_opts.lod_level, export_opts.texture_mip));
            prepare_decomposed_output_root(&output, &package_name)?;
            for file in &decomposed.files {
                let output_path = output.join(&file.relative_path);
                if let Some(parent) = output_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| CliError::IoPath { source: e, path: parent.display().to_string() })?;
                }
                let skip_existing = opts.skip_existing_assets
                    && !(opts.rewrite_meshes && file.kind == starbreaker_3d::ExportedFileKind::MeshAsset);
                write_decomposed_file(file, &output_path, skip_existing)?;
            }
        }
    }
    crate::log_mem_stats("after write");
    eprintln!("Written to {}", output.display());
    Ok(())
}

/// One entry of an `export-list` file.
struct ListEntry {
    record: String,
    body: String,
    material: Option<String>,
    palette: Option<String>,
    group: Option<String>,
    /// Mesh exported through the record instead of its own, and its package name (see ExportList)
    geometry: Option<String>,
    name: Option<String>,
}

fn read_export_list(path: &Path) -> Result<Vec<ListEntry>> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| CliError::IoPath { source: e, path: path.display().to_string() })?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| CliError::InvalidInput(format!("{}: {e}", path.display())))?;
    let items = value
        .as_array()
        .ok_or_else(|| CliError::InvalidInput(format!("{}: expected a JSON list", path.display())))?;
    let text_field = |item: &serde_json::Value, key: &str| item.get(key).and_then(|v| v.as_str()).map(str::to_string);
    items
        .iter()
        .map(|item| {
            Ok(ListEntry {
                record: text_field(item, "record").ok_or_else(|| {
                    CliError::InvalidInput(format!("{}: an entry has no \"record\": {item}", path.display()))
                })?,
                body: text_field(item, "body").unwrap_or_else(|| "male".to_string()),
                material: text_field(item, "material"),
                palette: text_field(item, "palette"),
                group: text_field(item, "group"),
                geometry: text_field(item, "geometry"),
                name: text_field(item, "name"),
            })
        })
        .collect()
}

/// Export one list entry and write its files. Meshes and textures already in `written` (written earlier in
/// this run, or found on disk with --skip-existing-assets) are neither made again nor written again.
fn export_list_entry(
    db: &Database,
    p4k: &starbreaker_p4k::MappedP4k,
    idx: &EntityIndex,
    entry: &ListEntry,
    base_opts: &starbreaker_3d::ExportOptions,
    output: &PathBuf,
    written: &std::sync::Mutex<HashSet<String>>,
) -> Result<()> {
    let record = idx
        .find_record(&entry.record)
        .ok_or_else(|| CliError::NotFound(format!("no EntityClassDefinition record named '{}'", entry.record)))?;
    let rname = db.resolve_string2(record.name_offset);
    let mut export_opts = base_opts.clone();
    export_opts.body = (entry.body != "prop").then(|| entry.body.clone());
    if entry.material.is_some() || entry.palette.is_some() || entry.geometry.is_some() {
        export_opts.overrides = Some(starbreaker_3d::EntityOverrides {
            record: export_entity_name(rname),
            material: entry.material.clone(),
            palette: entry.palette.clone(),
            geometry: entry.geometry.clone(),
        });
    }
    let mut tree = resolve_loadout_indexed(idx, record);
    // A mesh exported through a carrier record: none of the carrier's attachments, and the package is named
    // after the mesh file (or the entry's "name")
    if let Some(geometry) = &entry.geometry {
        tree.root.children.clear();
        tree.root.entity_name = entry.name.clone().unwrap_or_else(|| {
            let file = geometry.rsplit(['/', '\\']).next().unwrap_or(geometry);
            file.rsplit_once('.').map_or(file, |(stem, _)| stem).to_string()
        });
    }
    // A copy: the other workers add to the shared set while this export runs
    let known = written.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let result = starbreaker_3d::assemble_glb_with_loadout_with_progress(
        db,
        p4k,
        record,
        &tree,
        &export_opts,
        None,
        Some(&known),
    )?;
    let decomposed = result
        .decomposed
        .as_ref()
        .ok_or_else(|| CliError::InvalidInput("entity export returned no decomposed files".into()))?;
    let package_name = decomposed
        .files
        .iter()
        .find_map(|f| f.relative_path.strip_prefix("Packages/")?.strip_suffix("/scene.json").map(str::to_string))
        .ok_or_else(|| CliError::InvalidInput("decomposed export has no scene.json".into()))?;
    prepare_decomposed_output_root(output, &package_name)?;
    // scene.json last, so a package that has one is complete (a stopped run redoes the others)
    let is_scene = |f: &&starbreaker_3d::ExportedFile| f.relative_path.ends_with("/scene.json");
    for file in decomposed.files.iter().filter(|f| !is_scene(f)).chain(decomposed.files.iter().filter(is_scene)) {
        let output_path = output.join(&file.relative_path);
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| CliError::IoPath { source: e, path: parent.display().to_string() })?;
        }
        if file.kind.is_mesh_or_texture_asset() {
            // Claim the file: another worker may have written it while this export ran
            let key = file.relative_path.replace('\\', "/").to_ascii_lowercase();
            if !written.lock().unwrap_or_else(|e| e.into_inner()).insert(key) {
                continue;
            }
            // Written under another name, then renamed: a stopped run leaves no half-written mesh or texture
            // that a later --skip-existing-assets run would take as done
            let mut part = output_path.clone().into_os_string();
            part.push(".part");
            std::fs::write(&part, &file.bytes)
                .map_err(|e| CliError::IoPath { source: e, path: output_path.display().to_string() })?;
            std::fs::rename(&part, &output_path)
                .map_err(|e| CliError::IoPath { source: e, path: output_path.display().to_string() })?;
        } else {
            std::fs::write(&output_path, &file.bytes)
                .map_err(|e| CliError::IoPath { source: e, path: output_path.display().to_string() })?;
        }
    }
    Ok(())
}

fn export_list(list: PathBuf, output: PathBuf, p4k_path: Option<PathBuf>, jobs: usize, opts: ExportOpts) -> Result<()> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let entries = read_export_list(&list)?;
    // Units of work: consecutive entries of one group stay together on one worker
    let mut units: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        match units.last_mut() {
            Some(unit) if entry.group.is_some() && entries[unit.start].group == entry.group => unit.end = i + 1,
            _ => units.push(i..i + 1),
        }
    }

    let t0 = std::time::Instant::now();
    let (p4k, dcb_bytes) = load_dcb_bytes(p4k_path.as_deref(), None)?;
    let p4k = p4k.ok_or_else(|| CliError::MissingRequirement("P4k required for entity export".into()))?;
    let db = Database::from_bytes(&dcb_bytes)?;
    let mut opts = opts;
    opts.kind = "decomposed".to_string();
    let base_opts = starbreaker_3d::ExportOptions::from(&opts);
    let written = Mutex::new(if opts.skip_existing_assets {
        collect_existing_decomposed_assets(&output, opts.rewrite_meshes)?
    } else {
        HashSet::new()
    });
    std::fs::create_dir_all(&output).map_err(|e| CliError::IoPath { source: e, path: output.display().to_string() })?;
    eprintln!(
        "{} exports in {} groups, {} at once (game data read in {:.1}s)",
        entries.len(),
        units.len(),
        jobs.max(1),
        t0.elapsed().as_secs_f64()
    );

    let next = AtomicUsize::new(0);
    let failed = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                // The index caches paths in a RefCell, so each worker has its own
                let idx = EntityIndex::new(&db);
                loop {
                    let u = next.fetch_add(1, Ordering::Relaxed);
                    let Some(unit) = units.get(u) else { break };
                    for entry in &entries[unit.clone()] {
                        let t = std::time::Instant::now();
                        // A panic in one export (a malformed file) must not end the whole run
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            export_list_entry(&db, &p4k, &idx, entry, &base_opts, &output, &written)
                        }));
                        let error = match res {
                            Ok(Ok(())) => None,
                            Ok(Err(e)) => Some(e.to_string()),
                            Err(panic) => Some(format!(
                                "panic: {}",
                                panic
                                    .downcast_ref::<String>()
                                    .map(String::as_str)
                                    .or_else(|| panic.downcast_ref::<&str>().copied())
                                    .unwrap_or("?")
                            )),
                        };
                        match error {
                            None => println!("ok\t{}\t{}\t{:.1}", entry.record, entry.body, t.elapsed().as_secs_f64()),
                            Some(e) => {
                                failed.fetch_add(1, Ordering::Relaxed);
                                println!("FAIL\t{}\t{}\t{}", entry.record, entry.body, e.replace(['\n', '\t'], " "));
                            }
                        }
                    }
                }
            });
        }
    });
    eprintln!(
        "Done in {:.0}s: {} exports, {} failed",
        t0.elapsed().as_secs_f64(),
        entries.len(),
        failed.load(Ordering::Relaxed)
    );
    Ok(())
}

fn loadout(name: String, p4k_path: Option<PathBuf>) -> Result<()> {
    let (_, dcb_bytes) = load_dcb_bytes(p4k_path.as_deref(), None)?;
    let db = Database::from_bytes(&dcb_bytes)?;

    let candidates = find_candidates(&db, &name)?;
    if candidates.is_empty() {
        return Err(CliError::NotFound(format!("no EntityClassDefinition records matching '{name}'")));
    }

    let idx = EntityIndex::new(&db);
    for record in &candidates {
        let tree = resolve_loadout_indexed(&idx, record);
        print_loadout_node(&tree.root, 0);
    }
    Ok(())
}

fn print_loadout_node(node: &LoadoutNode, depth: usize) {
    let indent = "  ".repeat(depth);
    let geom = node.geometry_path.as_deref().unwrap_or("-");
    println!(
        "{indent}{} [{}] geom={geom}",
        node.entity_name, node.item_port_name
    );
    for child in &node.children {
        print_loadout_node(child, depth + 1);
    }
}
