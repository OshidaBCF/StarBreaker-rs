use super::*;

/// A one-bone `.aim` file: an IVO with the AIM_INFO head (fps 15) and the AIM_DATA clip block, one
/// uncompressed rotation key, laid out like the game's aim poses.
fn sample_aim() -> Vec<u8> {
    let quat = [0.0f32, 0.0, 0.6, 0.8];
    let mut file = Vec::new();
    file.extend(b"#ivo");
    for value in [0x900u32, 2, 16] {
        file.extend(value.to_le_bytes());
    }
    for (chunk_type, version, offset) in [(chunk_types::AIM_INFO, 0x901u32, 48u64), (chunk_types::AIM_DATA, 0x900, 64)] {
        file.extend(chunk_type.to_le_bytes());
        file.extend(version.to_le_bytes());
        file.extend(offset.to_le_bytes());
    }
    // AIM_INFO: flags, fps, controllers, reserved, end frame
    file.extend(0u32.to_le_bytes());
    file.extend(15u16.to_le_bytes());
    file.extend(1u16.to_le_bytes());
    file.extend(0u32.to_le_bytes());
    file.extend(0u32.to_le_bytes());
    // AIM_DATA: '#caf', bone count, 0xFFFF, block size, bone hash, controller, its key
    file.extend(b"#caf");
    file.extend(1u16.to_le_bytes());
    file.extend(0xFFFFu16.to_le_bytes());
    file.extend(56u32.to_le_bytes());
    file.extend(bone_name_hash("Spine").to_le_bytes());
    file.extend(1u16.to_le_bytes()); // one rotation key
    file.extend(0x8000u16.to_le_bytes()); // uncompressed quaternion, one key per frame
    file.extend(0u32.to_le_bytes()); // no time table
    file.extend(24u32.to_le_bytes()); // the key right after this controller entry
    file.extend([0u8; 12]); // no position keys
    for value in quat {
        file.extend(value.to_le_bytes());
    }
    file
}

#[test]
fn aim_pose_file_reads_as_a_single_clip() {
    let db = parse_caf(&sample_aim()).unwrap();
    assert_eq!(db.clips.len(), 1);
    let clip = &db.clips[0];
    assert_eq!(clip.fps, 15.0, "fps from the AIM_INFO head");
    assert_eq!(clip.channels.len(), 1);
    assert_eq!(clip.channels[0].bone_hash, bone_name_hash("Spine"));
    assert_eq!(clip.channels[0].rotations[0].value, [0.0, 0.0, 0.6, 0.8]);
}
