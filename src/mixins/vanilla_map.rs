fn vanilla_map(game: &Game<'_>, p: &horizon_protocol::packets::play::MapItemData) {
    // The game's own colour for every packed map colour id, so the mod ships no colour table.
    let palette: ::std::vec::Vec<[u8; 4]> = (0..=255u8).map(packed_rgba).collect();
    mod_universal_viewer::vanilla_map_palette(&palette);
    mod_universal_viewer::vanilla_map_info(p.map_id, p.scale, p.locked);
    // Decoration-only updates change no pixel.
    let Some(patch) = &p.patch else {
        return;
    };
    let mut colors = match game.maps.data.get(&p.map_id) {
        Some(map) if map.colors.len() == 128 * 128 => map.colors.clone(),
        _ => ::std::vec![0u8; 128 * 128],
    };
    for x in 0..patch.width {
        for y in 0..patch.height {
            let Some(&color) = patch.colors.get((x + y * patch.width) as usize) else {
                continue;
            };
            let (px, py) = (patch.start_x + x, patch.start_y + y);
            if (0..128).contains(&px) && (0..128).contains(&py) {
                colors[(px + py * 128) as usize] = color;
            }
        }
    }
    mod_universal_viewer::vanilla_map_colors(p.map_id, colors);
}
