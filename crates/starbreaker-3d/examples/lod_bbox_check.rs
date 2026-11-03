//! Compare how a mesh's LOD files decode: prints each file's two bounding boxes (model, scaling), the extent of the
//! decoded positions with each box, and the submeshes' node indices. Used to track down LOD meshes coming out
//! squashed in socpak exports (2026-10-30).
//! Usage: lod_bbox_check <p4k path of a .cgfm> [more .cgfm paths...]   (Data.p4k from SC_DATA_P4K)

use starbreaker_3d::ivo::skin::{PositionData, SkinMesh};
use starbreaker_chunks::ChunkFile;

fn extent(points: &[[f32; 3]]) -> [f32; 3] {
    let mut mn = [f32::MAX; 3];
    let mut mx = [f32::MIN; 3];
    for p in points {
        for i in 0..3 {
            mn[i] = mn[i].min(p[i]);
            mx[i] = mx[i].max(p[i]);
        }
    }
    [mx[0] - mn[0], mx[1] - mn[1], mx[2] - mn[2]]
}

fn main() {
    let p4k = starbreaker_p4k::open_p4k().expect("failed to open Data.p4k (set SC_DATA_P4K)");
    for path in std::env::args().skip(1) {
        let Some(entry) = p4k.entry_case_insensitive(&path) else {
            println!("{path}: not in the P4k");
            continue;
        };
        let bytes = p4k.read(entry).expect("read");
        let cf = ChunkFile::from_bytes(&bytes).expect("chunk file");
        let ChunkFile::Ivo(ivo) = &cf else {
            println!("{path}: not IVO");
            continue;
        };
        let Some(skin_entry) = ivo
            .chunks()
            .iter()
            .find(|c| c.chunk_type == starbreaker_chunks::known_types::ivo::IVO_SKIN2)
        else {
            println!("{path}: no IvoSkin2 chunk");
            continue;
        };
        let skin = SkinMesh::read(ivo.chunk_data(skin_entry)).expect("skin");
        let i = &skin.info;
        println!("{path}");
        println!("  model bbox   {:?} .. {:?}", i.model_min, i.model_max);
        println!("  scaling bbox {:?} .. {:?}", i.min_bound, i.max_bound);
        if let PositionData::Quantized(raw) = &skin.streams.positions {
            let with = |mn: &[f32; 3], mx: &[f32; 3]| -> Vec<[f32; 3]> {
                raw.iter().map(|p| starbreaker_3d::dequant::dequantize_position(*p, mn, mx)).collect()
            };
            println!("  decoded with model bbox:   extent {:?}", extent(&with(&i.model_min, &i.model_max)));
            println!("  decoded with scaling bbox: extent {:?}", extent(&with(&i.min_bound, &i.max_bound)));
        } else {
            println!("  positions are floats");
        }
        let nodes: Vec<u16> = skin.submeshes.iter().map(|s| s.node_parent_index).collect();
        println!("  submesh node indices {nodes:?}");
    }
}
