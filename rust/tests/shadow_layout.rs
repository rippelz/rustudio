// Rust / shadow_layout.rs / rustc --test / shadow atlas geometry
#[allow(dead_code)]
#[path = "../crates/rustudio-core/src/mech.rs"]
mod mech;

#[test]
fn cascades_fit_without_overlap() {
    for (i, (&[x, y], &size)) in mech::SUN_CASCADE_ORIGINS.iter().zip(&mech::SUN_CASCADE_TILES).enumerate() {
        assert!(size > 0 && size <= mech::SHADOW_MAP_TEXELS);
        assert!(x + size <= mech::SUN_SHADOW_ATLAS[0]);
        assert!(y + size <= mech::SUN_SHADOW_ATLAS[1]);
        for j in 0..i {
            let [other_x, other_y] = mech::SUN_CASCADE_ORIGINS[j];
            let other_size = mech::SUN_CASCADE_TILES[j];
            assert!(x + size <= other_x || other_x + other_size <= x ||
                y + size <= other_y || other_y + other_size <= y);
        }
    }
}

#[test]
fn local_light_atlas_has_room_for_all_six_faces_per_light() {
    let cols = mech::LSHADOW_ATLAS / mech::LSHADOW_FACE;
    assert!(cols * cols >= mech::LSHADOW_MAX_LIGHTS as u32 * 6);
    assert_eq!(cols, 10);
}

#[test]
fn mobile_budget_is_a_quarter_of_desktop_shadow_textures() {
    let depth = u64::from(mech::SHADOW_MAP_TEXELS).pow(2) * 4;
    let local = u64::from(mech::LSHADOW_ATLAS).pow(2) * 12;
    let sun = u64::from(mech::SUN_SHADOW_ATLAS[0]) * u64::from(mech::SUN_SHADOW_ATLAS[1]) * 8;
    let blur = u64::from(*mech::SUN_CASCADE_TILES.iter().max().unwrap()).pow(2) * 8;
    assert_eq!((depth + local + sun + blur) * u64::from(mech::MOBILE_SHADOW_DIVISOR).pow(2), 153_092_096);
}

