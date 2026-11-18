//! Universal Viewer: look inside things without opening them.
//!
//! * **Maps** show their picture right on their slot icon, and a bigger one in the tooltip when
//!   hovered (inventory, chests, creative tabs...). No key needed.
//! * **Shulker boxes** (and any other stack that carries `minecraft:container`) and **bundles**
//!   (`minecraft:bundle_contents`) show their whole contents as a slot grid while Shift is held.
//!
//! Containers and bundles are read straight from the stack's own components, so the client
//! needs nothing from the server. A map's pixels live only on the server (the client mod API has
//! no read of them), so the mod asks the server for them over a small optional payload and caches
//! the answer. On a server without this mod the map panel says so instead of drawing.

use mod_api::{
    ClientHost, ClientTickPreEvent, DrawContext, HostSide, LevelDimension, RightClickBlockEvent,
    ScreenRenderBackgroundEvent, ScreenRenderForegroundEvent, Flow, Hooks, IPayloadContext, ItemDecorationsEvent,
    Identifier, ItemKey, ItemPart, ItemStack,
    PayloadCodec, PayloadId, Registrar, RegistrationContext, RenderTooltipGatherComponentsEvent,
    RenderTooltipPreEvent, Tag, Text, ToClient, ToServer, TooltipElement, VarInt,
};
use mod_api::text::TextContent;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

// ---------------------------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------------------------

/// The shared mod-image slot of the HUD atlas (see `ClientHost::mod_image`).
const MOD_IMAGE_SPRITE: &str = "horizon/modimage";
/// A map is always 128 x 128 map pixels.
const MAP_SIZE: usize = 128;
/// GUI pixels one map pixel is drawn as.
const MAP_SCALE: f32 = 1.0;
/// One inventory slot, in GUI pixels.
const SLOT: f32 = 18.0;
/// Slots per row of a container grid.
const COLUMNS: usize = 9;
/// Ticks before a map that is still hovered is asked for again, so a map being explored updates.
const REFRESH_TICKS: u64 = 100;
/// Ticks before a map shown only on its slot icon is asked for again.
const ICON_REFRESH_TICKS: u64 = 400;
/// At most this many map requests go out per tick (a chest full of maps opens gradually).
const ASKS_PER_TICK: usize = 4;
/// Ticks between two uploads of the map atlas.
const UPLOAD_EVERY: u64 = 4;
/// The mod-image slot is 1024 x 1024 pixels: an 8 x 8 grid of 128 px maps.
const ATLAS: usize = 1024;
const CELLS_PER_ROW: usize = ATLAS / MAP_SIZE;
const CELLS: usize = CELLS_PER_ROW * CELLS_PER_ROW;

const TEXT_WHITE: u32 = 0xFFFF_FFFF;
const TEXT_GRAY: u32 = 0xFFAA_AAAA;

// ---------------------------------------------------------------------------------------------
// Payloads: the client asks for a map, the server answers with its colour ids
// ---------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, PayloadCodec)]
struct MapRequest {
    id: VarInt,
}

#[derive(Debug, PartialEq, PayloadCodec)]
struct MapData {
    id: VarInt,
    /// Which slice of the picture this is, `0..PARTS`.
    part: VarInt,
    /// `PART_LEN` map colour ids (`base * 4 + shade`), row by row.
    colors: Vec<u8>,
}

/// A payload list holds at most 4096 entries, so a 128x128 map goes in four slices.
const PART_LEN: usize = 4096;
const PARTS: usize = MAP_SIZE * MAP_SIZE / PART_LEN;

static REQUEST: Mutex<Option<ToServer<MapRequest>>> = Mutex::new(None);
static REPLY: Mutex<Option<ToClient<MapData>>> = Mutex::new(None);

// ---------------------------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------------------------

/// A map whose picture is in the atlas.
struct CachedMap {
    /// Its cell of the atlas grid.
    cell: usize,
    /// The atlas generation that wrote it; it shows once an upload at least that new happened.
    written: u64,
    /// When it was last drawn (slot or tooltip), to pick a cell to reuse.
    seen: u64,
    /// Whether its cell has been uploaded at least once: a refresh keeps showing the old
    /// picture until the new one is up, instead of falling back to the vanilla icon.
    shown: bool,
    /// A hash of its colour ids, so an unchanged refresh writes and uploads nothing.
    hash: u64,
}

#[derive(Default)]
struct Cache {
    /// Client ticks since the game started, a clock for refreshes and time-outs.
    tick: u64,
    maps: HashMap<i32, CachedMap>,
    /// When each map was last asked for.
    requested: HashMap<i32, u64>,
    /// The map whose tooltip was drawn since the last tick.
    hovered: Option<i32>,
    /// Maps drawn on slot icons since the last tick.
    on_slots: HashSet<i32>,
    /// Maps still arriving: their colour ids so far and which slices came.
    partial: HashMap<i32, (Vec<u8>, u8)>,
    /// The `VANILLA` version of each map last copied into the atlas.
    vanilla: HashMap<i32, u64>,
    /// All map pictures, `ATLAS * ATLAS` RGBA pixels; empty until the first map arrives.
    atlas: Vec<u8>,
    /// Bumped on every write into `atlas`.
    generation: u64,
    /// The generation last uploaded into the mod-image slot.
    uploaded: u64,
    last_upload: u64,
}

impl Cache {
    /// Whether map `id`'s picture is in the uploaded atlas, and its cell.
    fn ready(&self, id: i32) -> Option<usize> {
        let map = self.maps.get(&id)?;
        (map.shown || map.written <= self.uploaded).then_some(map.cell)
    }

    /// Writes map `id`'s pixels into its cell, taking the least recently seen cell if it is new.
    fn store(&mut self, id: i32, colors: &[u8]) {
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            colors.hash(&mut h);
            h.finish()
        };
        let mut shown = false;
        if let Some(map) = self.maps.get_mut(&id) {
            if map.hash == hash {
                return;
            }
            shown = map.shown || map.written <= self.uploaded;
        }
        let rgba = to_rgba(colors);
        if self.atlas.is_empty() {
            self.atlas = vec![0; ATLAS * ATLAS * 4];
        }
        let cell = match self.maps.get(&id) {
            Some(map) => map.cell,
            None => {
                let used: HashSet<usize> = self.maps.values().map(|m| m.cell).collect();
                match (0..CELLS).find(|cell| !used.contains(cell)) {
                    Some(free) => free,
                    None => {
                        let (&old, map) = self
                            .maps
                            .iter()
                            .min_by_key(|(_, m)| m.seen)
                            .expect("all cells are in use");
                        let cell = map.cell;
                        self.maps.remove(&old);
                        cell
                    }
                }
            }
        };
        let (cx, cy) = (cell % CELLS_PER_ROW, cell / CELLS_PER_ROW);
        let row = MAP_SIZE * 4;
        for y in 0..MAP_SIZE {
            let at = ((cy * MAP_SIZE + y) * ATLAS + cx * MAP_SIZE) * 4;
            self.atlas[at..at + row].copy_from_slice(&rgba[y * row..(y + 1) * row]);
        }
        self.generation += 1;
        let seen = self.tick;
        self.maps.insert(
            id,
            CachedMap {
                cell,
                written: self.generation,
                seen,
                shown,
                hash,
            },
        );
    }
}

/// The source rectangle of atlas cell `cell`.
fn cell_rect(cell: usize) -> [f32; 4] {
    let size = MAP_SIZE as f32;
    let (cx, cy) = (cell % CELLS_PER_ROW, cell / CELLS_PER_ROW);
    [cx as f32 * size, cy as f32 * size, size, size]
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Map pictures the server sent the game itself (vanilla's map packet, read by the
/// `vanilla_map` mixin): id to colour ids, with a counter bumped on each change. Works on any
/// server, the mod or not on it, for every map the client has been sent: one carried, held or
/// in a nearby item frame.
static VANILLA_MAPS: Mutex<Option<(u64, HashMap<i32, (u64, Vec<u8>)>)>> = Mutex::new(None);

/// Called by the `vanilla_map` mixin with a map's whole picture after a vanilla map packet.
pub fn vanilla_map_colors(id: i32, colors: Vec<u8>) {
    if colors.len() != MAP_SIZE * MAP_SIZE {
        return;
    }
    let mut guard = VANILLA_MAPS.lock().unwrap_or_else(|p| p.into_inner());
    let (counter, maps) = guard.get_or_insert_with(|| (0, HashMap::new()));
    *counter += 1;
    maps.insert(id, (*counter, colors));
}

/// Map `id`'s picture from the game's own copy, when newer than `after`.
fn vanilla_colors(id: i32, after: u64) -> Option<(u64, Vec<u8>)> {
    let guard = VANILLA_MAPS.lock().unwrap_or_else(|p| p.into_inner());
    let (_, maps) = guard.as_ref()?;
    let (version, colors) = maps.get(&id)?;
    (*version > after).then(|| (*version, colors.clone()))
}

fn with_cache<R>(f: impl FnOnce(&mut Cache) -> R) -> R {
    let mut guard = CACHE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    f(guard.get_or_insert_with(Cache::default))
}

/// The vanilla map base colours (`MapColor`), `0xRRGGBB`, indexed by `colour id >> 2`.
const BASE_COLORS: [u32; 62] = [
    0x000000, 0x7FB238, 0xF7E9A3, 0xC7C7C7, 0xFF0000, 0xA0A0FF, 0xA7A7A7, 0x007C00, 0xFFFFFF,
    0xA4A8B8, 0x976D4D, 0x707070, 0x4040FF, 0x8F7748, 0xFFFCF5, 0xD87F33, 0xB24CD8, 0x6699D8,
    0xE5E533, 0x7FCC19, 0xF27FA5, 0x4C4C4C, 0x999999, 0x4C7F99, 0x7F3FB2, 0x334CB2, 0x664C33,
    0x667F33, 0x993333, 0x191919, 0xFAEE4D, 0x5CDBD5, 0x4A80FF, 0x00D93A, 0x815631, 0x700200,
    0xD1B1A1, 0x9F5224, 0x95576C, 0x706C8A, 0xBA8524, 0x677535, 0xA04D4E, 0x392923, 0x876B62,
    0x575C5C, 0x7A4958, 0x4C3E5C, 0x4C3223, 0x4C522A, 0x8E3C2E, 0x251610, 0xBD3031, 0x943F61,
    0x5C191D, 0x167E86, 0x3A8E8C, 0x562C3E, 0x14B485, 0x646464, 0xD8AF93, 0x7FA796,
];

/// Map colour ids to RGBA bytes, as `MapColor.calculateARGBColor` shades them.
fn to_rgba(colors: &[u8]) -> Vec<u8> {
    const SHADE: [u32; 4] = [180, 220, 255, 135];
    let mut out = Vec::with_capacity(colors.len() * 4);
    for &id in colors {
        let base = usize::from(id >> 2);
        if base == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let rgb = BASE_COLORS.get(base).copied().unwrap_or(0x99_9999);
        let mul = SHADE[usize::from(id & 3)];
        let channel = |shift: u32| (((rgb >> shift) & 0xFF) * mul / 255) as u8;
        out.extend_from_slice(&[channel(16), channel(8), channel(0), 255]);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Reading a stack
// ---------------------------------------------------------------------------------------------

/// What a stack has to show.
enum View {
    Map(i32),
    Contents(Contents),
}

struct Contents {
    columns: usize,
    rows: usize,
    /// Slot index and stack, in grid order.
    slots: Vec<(usize, ItemStack)>,
    /// Total items, for the footer.
    items: u64,
    /// The colour of the shulker box or bundle, `0xRRGGBB`, to tint the panel with.
    color: u32,
}

/// Dye colours as vanilla tints shulker boxes and bundles (`DyeColor.getTextureDiffuseColor`).
const DYES: [(&str, u32); 16] = [
    ("white", 0xF9FFFE),
    ("orange", 0xF9801D),
    ("magenta", 0xC74EBD),
    ("light_blue", 0x3AB3DA),
    ("yellow", 0xFED83D),
    ("lime", 0x80C71F),
    ("pink", 0xF38BAA),
    ("gray", 0x474F52),
    ("light_gray", 0x9D9D97),
    ("cyan", 0x169C9C),
    ("purple", 0x8932B8),
    ("blue", 0x3C44AA),
    ("brown", 0x835432),
    ("green", 0x5E7C16),
    ("red", 0xB02E26),
    ("black", 0x1D1D21),
];
/// An undyed shulker box's purple, and an undyed bundle's leather.
const PLAIN_SHULKER: u32 = 0x97_6997;
const PLAIN_BUNDLE: u32 = 0xA8_6B3C;
/// Anything else that holds items (a chest item with contents, a modded box).
const PLAIN_OTHER: u32 = 0x6E_4C9E;

/// The colour of `item` if it is `minecraft:<dye>_<kind>`, else `plain` when it is
/// `minecraft:<kind>` itself, else `other`.
fn color_of(host: &dyn ClientHost, item: ItemKey, kind: &str, plain: u32, other: u32) -> u32 {
    let is = |id: &str| {
        Identifier::parse(id)
            .ok()
            .and_then(|id| host.item_key(&id))
            .is_some_and(|key| key == item)
    };
    for (dye, rgb) in DYES {
        if is(&format!("minecraft:{dye}_{kind}")) {
            return rgb;
        }
    }
    if is(&format!("minecraft:{kind}")) { plain } else { other }
}

/// The colours one panel is drawn with, ARGB.
#[derive(Clone, Copy)]
struct Style {
    background: u32,
    top: u32,
    bottom: u32,
}

/// The vanilla tooltip's colours, for maps.
const TOOLTIP_STYLE: Style = Style {
    background: 0xF010_0010,
    top: 0xF050_00FF,
    bottom: 0xF028_007F,
};



/// The stack's component patch as one NBT compound to look entries up in.
fn components(stack: &ItemStack) -> Option<Tag> {
    stack
        .components()
        .map(|patch| Tag::Compound(patch.as_nbt().clone()))
}

fn int(tag: &Tag) -> Option<i32> {
    match tag {
        Tag::Int(v) => Some(*v),
        Tag::Short(v) => Some(i32::from(*v)),
        Tag::Byte(v) => Some(i32::from(*v)),
        _ => None,
    }
}

fn stack_from(host: &dyn ClientHost, tag: &Tag) -> Option<ItemStack> {
    match tag {
        Tag::Compound(compound) => ItemStack::from_nbt(host, compound),
        _ => None,
    }
}

/// A map, or the contents of a container or bundle; `None` when the stack is neither.
fn view_of(host: &dyn ClientHost, stack: &ItemStack, with_contents: bool) -> Option<View> {
    let root = components(stack)?;
    if let Some(id) = root.get("minecraft:map_id").and_then(int) {
        return Some(View::Map(id));
    }
    if !with_contents {
        return None;
    }
    if let Some(Tag::List(entries)) = root.get("minecraft:container") {
        let mut slots = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let slot = entry
                .get("slot")
                .and_then(int)
                .and_then(|slot| usize::try_from(slot).ok())
                .unwrap_or(index);
            if let Some(stack) = entry.get("item").and_then(|item| stack_from(host, item)) {
                slots.push((slot, stack));
            }
        }
        if slots.is_empty() {
            return None;
        }
        let highest = slots.iter().map(|(slot, _)| *slot).max().unwrap_or(0);
        let rows = (highest / COLUMNS + 1).max(3);
        let color = color_of(host, stack.item(), "shulker_box", PLAIN_SHULKER, PLAIN_OTHER);
        return Some(View::Contents(finish(COLUMNS, rows, slots, color)));
    }
    if let Some(Tag::List(entries)) = root.get("minecraft:bundle_contents") {
        let slots: Vec<(usize, ItemStack)> = entries
            .iter()
            .filter_map(|item| stack_from(host, item))
            .enumerate()
            .collect();
        if slots.is_empty() {
            return None;
        }
        let columns = slots.len().clamp(1, COLUMNS);
        let rows = slots.len().div_ceil(columns);
        let color = color_of(host, stack.item(), "bundle", PLAIN_BUNDLE, PLAIN_BUNDLE);
        return Some(View::Contents(finish(columns, rows, slots, color)));
    }
    None
}

fn finish(columns: usize, rows: usize, slots: Vec<(usize, ItemStack)>, color: u32) -> Contents {
    let items = slots.iter().map(|(_, s)| u64::from(s.count())).sum();
    Contents {
        columns,
        rows,
        slots,
        items,
        color,
    }
}

/// A text component as plain characters, translating keys through the selected language.
fn plain(host: &dyn ClientHost, text: &Text) -> String {
    let mut out = match &text.content {
        TextContent::Literal(literal) => literal.clone(),
        TextContent::Translatable {
            key,
            fallback,
            arguments,
        } => {
            let mut line = host
                .translate(key)
                .or_else(|| fallback.clone())
                .unwrap_or_else(|| key.clone());
            for argument in arguments {
                let value = plain(host, argument);
                if let Some(at) = line.find("%s") {
                    line.replace_range(at..at + 2, &value);
                }
            }
            line
        }
        _ => String::new(),
    };
    for child in &text.children {
        out.push_str(&plain(host, child));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

/// Draws a tooltip box `w` x `h` GUI pixels hung from the cursor as the game hangs one, kept on
/// screen, and returns where its content starts.
fn panel(
    draw: &mut dyn DrawContext,
    cursor: [i32; 2],
    screen: [i32; 2],
    w: f32,
    h: f32,
    style: Style,
) -> [f32; 2] {
    let (cx, cy) = (cursor[0] as f32, cursor[1] as f32);
    let (sw, sh) = (screen[0] as f32, screen[1] as f32);
    let mut x = cx + 12.0;
    if x + w + 4.0 > sw {
        x = cx - 12.0 - w;
    }
    x = x.max(4.0);
    let mut y = cy - 12.0;
    if y + h + 4.0 > sh {
        y = sh - h - 4.0;
    }
    y = y.max(4.0);

    let Style {
        background,
        top,
        bottom,
        ..
    } = style;
    draw.fill([x - 3.0, y - 4.0, w + 6.0, h + 8.0], background);
    draw.fill([x - 3.0, y - 4.0, w + 6.0, 1.0], top);
    draw.fill([x - 3.0, y + h + 3.0, w + 6.0, 1.0], bottom);
    draw.fill_gradient([x - 3.0, y - 3.0, 1.0, h + 6.0], top, bottom);
    draw.fill_gradient([x + w + 2.0, y - 3.0, 1.0, h + 6.0], top, bottom);
    [x, y]
}

/// Gray `gray` (one channel, as vanilla's GUI textures use) multiplied by `tint` (`0xRRGGBB`), ARGB.
fn mul(gray: u32, tint: u32) -> u32 {
    let channel = |shift: u32| (((tint >> shift) & 0xFF) * gray / 255) << shift;
    0xFF00_0000 | channel(16) | channel(8) | channel(0)
}

/// A dye colour softened toward white, so even black and gray boxes keep a readable panel.
fn soften(rgb: u32) -> u32 {
    let channel = |shift: u32| {
        let c = ((rgb >> shift) & 0xFF) as f32;
        ((c * 0.7 + 255.0 * 0.3).round() as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Inner padding of the vanilla-style panel, as a container screen has.
const PAD: f32 = 7.0;

/// A container-screen panel (black outline, light top-left bevel, dark bottom-right bevel,
/// `#C6C6C6` face), all multiplied by `tint`. `w` x `h` is the content; returns its corner.
fn vanilla_panel(
    draw: &mut dyn DrawContext,
    cursor: [i32; 2],
    screen: [i32; 2],
    w: f32,
    h: f32,
    tint: u32,
) -> [f32; 2] {
    let (pw, ph) = (w + PAD * 2.0, h + PAD * 2.0);
    let (cx, cy) = (cursor[0] as f32, cursor[1] as f32);
    let (sw, sh) = (screen[0] as f32, screen[1] as f32);
    let mut x = cx + 12.0;
    if x + pw + 2.0 > sw {
        x = cx - 12.0 - pw;
    }
    x = x.max(2.0);
    let mut y = cy - 12.0;
    if y + ph + 2.0 > sh {
        y = sh - ph - 2.0;
    }
    y = y.max(2.0);

    let black = 0xFF00_0000;
    let (face, light, dark) = (mul(0xC6, tint), mul(0xFF, tint), mul(0x55, tint));
    // Outline with cut corners.
    draw.fill([x + 1.0, y, pw - 2.0, 1.0], black);
    draw.fill([x + 1.0, y + ph - 1.0, pw - 2.0, 1.0], black);
    draw.fill([x, y + 1.0, 1.0, ph - 2.0], black);
    draw.fill([x + pw - 1.0, y + 1.0, 1.0, ph - 2.0], black);
    draw.fill([x + 1.0, y + 1.0, pw - 2.0, ph - 2.0], face);
    // Bevels.
    draw.fill([x + 1.0, y + 1.0, pw - 4.0, 2.0], light);
    draw.fill([x + 1.0, y + 1.0, 2.0, ph - 4.0], light);
    draw.fill([x + 3.0, y + ph - 3.0, pw - 4.0, 2.0], dark);
    draw.fill([x + pw - 3.0, y + 3.0, 2.0, ph - 4.0], dark);
    [x + PAD, y + PAD]
}

/// One sunken inventory slot at `(x, y)`, 18 x 18, tinted.
fn vanilla_slot(draw: &mut dyn DrawContext, x: f32, y: f32, tint: u32) {
    draw.fill([x, y, SLOT, SLOT], mul(0x8B, tint));
    draw.fill([x, y, SLOT - 1.0, 1.0], mul(0x37, tint));
    draw.fill([x, y, 1.0, SLOT - 1.0], mul(0x37, tint));
    draw.fill([x + 1.0, y + SLOT - 1.0, SLOT - 1.0, 1.0], mul(0xFF, tint));
    draw.fill([x + SLOT - 1.0, y + 1.0, 1.0, SLOT - 1.0], mul(0xFF, tint));
}

fn draw_contents(
    draw: &mut dyn DrawContext,
    cursor: [i32; 2],
    screen: [i32; 2],
    title: &str,
    contents: &Contents,
) {
    let footer = format!("{} items", contents.items);
    let grid_w = contents.columns as f32 * SLOT;
    let grid_h = contents.rows as f32 * SLOT;
    let w = grid_w
        .max(draw.text_width(title))
        .max(draw.text_width(&footer));
    let h = 11.0 + grid_h + 11.0;
    let tint = soften(contents.color);
    let [x, y] = vanilla_panel(draw, cursor, screen, w, h, tint);

    // Vanilla's dark label colour on a light face, white with a shadow on a dark one.
    let (label, shadow) = if dark_face(tint) { (TEXT_WHITE, true) } else { (0xFF40_4040, false) };

    draw.text(title, [x, y], label, shadow);

    let gx = x + ((w - grid_w) / 2.0).floor();
    let gy = y + 11.0;
    for row in 0..contents.rows {
        for column in 0..contents.columns {
            vanilla_slot(draw, gx + column as f32 * SLOT, gy + row as f32 * SLOT, tint);
        }
    }
    for (slot, stack) in &contents.slots {
        let sx = gx + (slot % contents.columns) as f32 * SLOT;
        let sy = gy + (slot / contents.columns) as f32 * SLOT;
        let count = (stack.count() > 1).then(|| stack.count().to_string());
        draw.gui_draw_item_with(stack, [sx + 1.0, sy + 1.0], count.as_deref());
    }
    draw.text(&footer, [x, gy + grid_h + 3.0], label, shadow);
}

/// The map panel: its picture (atlas cell `cell`) with the item name and id.
fn draw_map(
    draw: &mut dyn DrawContext,
    cursor: [i32; 2],
    screen: [i32; 2],
    title: &str,
    id: i32,
    cell: usize,
) {
    let image = MAP_SIZE as f32 * MAP_SCALE;
    let footer = format!("Map #{id}");
    let w = (image + 2.0)
        .max(draw.text_width(title))
        .max(draw.text_width(&footer));
    let h = 12.0 + image + 2.0 + 12.0;
    let [x, y] = panel(draw, cursor, screen, w, h, TOOLTIP_STYLE);

    draw.text(title, [x, y], TEXT_WHITE, true);

    let ix = x + ((w - image - 2.0) / 2.0).floor();
    let iy = y + 12.0;
    // Frame, then the paper the map is drawn on.
    draw.fill([ix, iy, image + 2.0, image + 2.0], 0xFF00_0000);
    draw.fill([ix + 1.0, iy + 1.0, image, image], 0xFFD1_B380);
    draw.sprite(
        MOD_IMAGE_SPRITE,
        cell_rect(cell),
        [ix + 1.0, iy + 1.0, image, image],
        None,
    );
    draw.text(&footer, [x, iy + image + 5.0], TEXT_GRAY, true);
}


// ---------------------------------------------------------------------------------------------
// A placed shulker box's own screen, tinted like the box
// ---------------------------------------------------------------------------------------------

/// The colour of the shulker box the player last right-clicked, `0xRRGGBB`; None after a click
/// on anything else.
static OPENED: Mutex<Option<u32>> = Mutex::new(None);

/// The vanilla shulker box screen: 176 x 166, 3 x 9 box slots, then the inventory and hotbar.
const SCREEN_W: f32 = 176.0;
const SCREEN_H: f32 = 166.0;
/// Top-left of each 9-wide slot row block (the slot frame, one pixel outside the item), rows.
const SCREEN_GRIDS: [(f32, f32, usize); 3] = [(7.0, 17.0, 3), (7.0, 83.0, 3), (7.0, 141.0, 1)];

/// The tint of the open screen, when it is a placed shulker box's of 176 x 166.
fn shulker_screen(screen: &mod_api::ScreenView<'_>) -> Option<([f32; 2], u32)> {
    if screen.menu != Some("minecraft:shulker_box") {
        return None;
    }
    let [x, y, w, h] = screen.rect;
    if (w - SCREEN_W).abs() > 0.5 || (h - SCREEN_H).abs() > 0.5 {
        return None;
    }
    let color = (*OPENED.lock().unwrap_or_else(|p| p.into_inner()))?;
    Some(([x, y], soften(color)))
}

/// Whether labels on a face of this tint want white text.
fn dark_face(tint: u32) -> bool {
    let face = mul(0xC6, tint);
    let luma = ((face >> 16) & 0xFF) * 3 + ((face >> 8) & 0xFF) * 6 + (face & 0xFF);
    luma <= 1300
}

// ---------------------------------------------------------------------------------------------
// The mod
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct UniversalViewer;

#[mod_api::horizon_mod]
impl Hooks for UniversalViewer {
    fn register(&self, r: &mut dyn Registrar, _: &RegistrationContext) {
        let mut events = r.payload_handlers();
        // Optional: the mod still loads, and joins, a server that doesn't have it.
        let mut payloads = events.registrar("1").optional();

        let reply = payloads.play_to_client_typed::<MapData>(
            PayloadId("universal_viewer:map_data".into()),
            on_map_data,
        );
        *REPLY.lock().unwrap_or_else(|p| p.into_inner()) = Some(reply);

        let request = payloads.play_to_server_typed::<MapRequest>(
            PayloadId("universal_viewer:map_request".into()),
            on_map_request,
        );
        *REQUEST.lock().unwrap_or_else(|p| p.into_inner()) = Some(request);

        // The picture on the slot icon of every filled map.
        let map = r.item_key("minecraft:filled_map");
        r.item_part(map, MapIcon);
    }

    /// Once a tick: ask the server for the maps on screen, and upload the atlas when it changed.
    fn client_tick_pre(&self, host: &mut dyn ClientHost, _: &mut ClientTickPreEvent) {
        let asks = with_cache(|cache| {
            cache.tick += 1;
            let tick = cache.tick;
            let hovered = cache.hovered.take();
            let on_slots = std::mem::take(&mut cache.on_slots);
            let mut asks = Vec::new();
            // The hovered map first, refreshed more often, then the slot icons.
            let wanted = hovered
                .iter()
                .map(|id| (*id, REFRESH_TICKS))
                .chain(on_slots.iter().map(|id| (*id, ICON_REFRESH_TICKS)));
            for (id, refresh) in wanted {
                if let Some(map) = cache.maps.get_mut(&id) {
                    map.seen = tick;
                }
                // The game's own copy first: no server help needed.
                let after = if cache.maps.contains_key(&id) {
                    cache.vanilla.get(&id).copied().unwrap_or(0)
                } else {
                    0
                };
                if let Some((version, colors)) = vanilla_colors(id, after) {
                    cache.store(id, &colors);
                    cache.vanilla.insert(id, version);
                }
                if cache.vanilla.contains_key(&id) && cache.maps.contains_key(&id) {
                    continue;
                }
                if asks.len() >= ASKS_PER_TICK || asks.contains(&id) {
                    continue;
                }
                let have = cache.maps.contains_key(&id);
                let wait = if have { refresh } else { REFRESH_TICKS };
                let due = cache
                    .requested
                    .get(&id)
                    .is_none_or(|at| tick.saturating_sub(*at) > wait);
                if due {
                    cache.requested.insert(id, tick);
                    asks.push(id);
                }
            }
            if cache.generation > cache.uploaded
                && tick.saturating_sub(cache.last_upload) >= UPLOAD_EVERY
                && host.mod_image(ATLAS as u32, ATLAS as u32, &cache.atlas)
            {
                cache.uploaded = cache.generation;
                cache.last_upload = tick;
                for map in cache.maps.values_mut() {
                    map.shown = true;
                }
            }
            asks
        });
        if asks.is_empty() {
            return;
        }
        let token = REQUEST.lock().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(token) = token {
            for id in asks {
                // An unsupported peer (a server without the mod) just never answers.
                let _ = token.send(host, &MapRequest { id: VarInt(id) });
            }
        }
    }

    /// Remembers the colour of a shulker box the player right-clicks, for its screen.
    fn right_click_block(&self, host: HostSide, event: &mut RightClickBlockEvent<'_>) -> Flow {
        let HostSide::Client(host) = host else {
            return Flow::Continue;
        };
        let dimension = match event.click.dimension {
            LevelDimension::Overworld => "minecraft:overworld",
            LevelDimension::Nether => "minecraft:the_nether",
            LevelDimension::End => "minecraft:the_end",
            _ => {
                *OPENED.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return Flow::Continue;
            }
        };
        let color = host
            .level(dimension)
            .and_then(|level| level.block(&*host as &dyn mod_api::Host, event.click.pos))
            .and_then(|block| host.block_item(block))
            .map(|item| color_of(&*host, item, "shulker_box", PLAIN_SHULKER, 0))
            .filter(|color| *color != 0);
        *OPENED.lock().unwrap_or_else(|p| p.into_inner()) = color;
        Flow::Continue
    }

    /// Repaints a placed shulker box's screen panel and slots in the box's colour, under its
    /// items and labels.
    fn screen_render_background(&self, _: &dyn ClientHost, event: &mut ScreenRenderBackgroundEvent<'_>) {
        let Some(([x, y], tint)) = shulker_screen(event.screen) else {
            return;
        };
        let draw = &mut *event.draw;
        let black = 0xFF00_0000;
        let (face, light, dark) = (mul(0xC6, tint), mul(0xFF, tint), mul(0x55, tint));
        let (w, h) = (SCREEN_W, SCREEN_H);
        draw.fill([x + 1.0, y, w - 2.0, 1.0], black);
        draw.fill([x + 1.0, y + h - 1.0, w - 2.0, 1.0], black);
        draw.fill([x, y + 1.0, 1.0, h - 2.0], black);
        draw.fill([x + w - 1.0, y + 1.0, 1.0, h - 2.0], black);
        draw.fill([x + 1.0, y + 1.0, w - 2.0, h - 2.0], face);
        draw.fill([x + 1.0, y + 1.0, w - 4.0, 2.0], light);
        draw.fill([x + 1.0, y + 1.0, 2.0, h - 4.0], light);
        draw.fill([x + 3.0, y + h - 3.0, w - 4.0, 2.0], dark);
        draw.fill([x + w - 3.0, y + 3.0, 2.0, h - 4.0], dark);
        for (gx, gy, rows) in SCREEN_GRIDS {
            for row in 0..rows {
                for column in 0..COLUMNS {
                    let sx = x + gx + column as f32 * SLOT;
                    let sy = y + gy + row as f32 * SLOT;
                    vanilla_slot(draw, sx, sy, tint);
                }
            }
        }
    }

    /// On a dark box, writes the screen's labels again in white so they stay readable.
    fn screen_render_foreground(&self, host: &dyn ClientHost, event: &mut ScreenRenderForegroundEvent<'_>) {
        let Some(([x, y], tint)) = shulker_screen(event.screen) else {
            return;
        };
        if !dark_face(tint) {
            return;
        }
        let title = event.screen.title.map(|t| plain(host, t)).unwrap_or_default();
        let inventory = host
            .translate("container.inventory")
            .unwrap_or_else(|| "Inventory".to_string());
        event.draw.text(&title, [x + 8.0, y + 6.0], TEXT_WHITE, true);
        event.draw.text(&inventory, [x + 8.0, y + 72.0], TEXT_WHITE, true);
    }

    /// A hint on things that hold items, so the Shift trick is discoverable.
    fn render_tooltip_gather_components(
        &self,
        host: &dyn ClientHost,
        event: &mut RenderTooltipGatherComponentsEvent<'_>,
    ) -> Flow {
        if !host.modifiers().shift()
            && matches!(view_of(host, event.stack, true), Some(View::Contents(_)))
        {
            event.elements.push(TooltipElement::Text(
                Text::literal("Hold Shift to view contents"),
                // Bright red (`rgba`).
                0xFF55_55FF,
            ));
        }
        Flow::Continue
    }

    /// Takes the hover over for a map (always) or a container/bundle (with Shift held).
    fn render_tooltip_pre(&self, host: &dyn ClientHost, event: &mut RenderTooltipPreEvent<'_>) -> Flow {
        let Some(stack) = event.stack else {
            return Flow::Continue;
        };
        let modifiers = host.modifiers();
        let shift = modifiers.shift();
        let Some(view) = view_of(host, stack, shift) else {
            return Flow::Continue;
        };
        let title = event
            .elements
            .iter()
            .find_map(|element| match element {
                TooltipElement::Text(text, _) => Some(plain(host, text)),
                TooltipElement::Image(_) => None,
            })
            .unwrap_or_default();
        let cursor = [event.place.x, event.place.y];
        let screen = [event.screen_width, event.screen_height];
        let draw = &mut *event.draw;

        match view {
            View::Contents(contents) => draw_contents(draw, cursor, screen, &title, &contents),
            View::Map(id) => {
                let cell = with_cache(|cache| {
                    cache.hovered = Some(id);
                    cache.ready(id)
                });
                // Until the picture is here, the vanilla tooltip stays.
                let Some(cell) = cell else {
                    return Flow::Continue;
                };
                draw_map(draw, cursor, screen, &title, id, cell);
            }
        }
        Flow::Cancel
    }
}

/// Draws a filled map's picture over its slot icon, once the picture is here.
#[derive(Default)]
pub struct MapIcon;

#[mod_api::horizon_part]
impl ItemPart for MapIcon {
    fn item_decorations(&self, host: &dyn ClientHost, event: &mut ItemDecorationsEvent<'_>) {
        let Some(View::Map(id)) = view_of(host, event.stack, false) else {
            return;
        };
        let cell = with_cache(|cache| {
            cache.on_slots.insert(id);
            cache.ready(id)
        });
        let Some(cell) = cell else {
            return;
        };
        let (x, y) = (event.x, event.y);
        // Paper edge, then the picture, over the vanilla icon.
        event.draw.fill([x, y, 16.0, 16.0], 0xFFD1_B380);
        event.draw.fill([x, y, 16.0, 1.0], 0xFF8A_6A3A);
        event.draw.fill([x, y + 15.0, 16.0, 1.0], 0xFF8A_6A3A);
        event.draw.fill([x, y, 1.0, 16.0], 0xFF8A_6A3A);
        event.draw.fill([x + 15.0, y, 1.0, 16.0], 0xFF8A_6A3A);
        event.draw.sprite(MOD_IMAGE_SPRITE, cell_rect(cell), [x + 1.0, y + 1.0, 14.0, 14.0], None);
    }
}

// ---------------------------------------------------------------------------------------------
// Payload handlers
// ---------------------------------------------------------------------------------------------

/// Server: a client wants map `id`. Answer with its colours, in slices, when the world has it.
/// Never returns an error: a failed handler disconnects the player.
fn on_map_request(request: MapRequest, cx: &mut dyn IPayloadContext) -> Result<(), String> {
    let id = request.id.0;
    let colors = {
        let Ok(host) = cx.server_host() else {
            return Ok(());
        };
        // Maps belong to the world, so any level reaches them.
        let Some(level) = host.level("minecraft:overworld") else {
            return Ok(());
        };
        level.map_data(host, id).map(|state| state.colors)
    };
    let Some(colors) = colors else {
        return Ok(());
    };
    if colors.len() != MAP_SIZE * MAP_SIZE {
        return Ok(());
    }
    let Some(token) = REPLY.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
        return Ok(());
    };
    for (part, slice) in colors.chunks(PART_LEN).enumerate() {
        let data = MapData {
            id: VarInt(id),
            part: VarInt(part as i32),
            colors: slice.to_vec(),
        };
        if token.reply(cx, &data).is_err() {
            break;
        }
    }
    Ok(())
}

/// Client: one slice of a map's colours. Once all slices are in, keep the picture as RGBA.
fn on_map_data(data: MapData, _: &mut dyn IPayloadContext) -> Result<(), String> {
    let id = data.id.0;
    let part = data.part.0;
    if part < 0 || part as usize >= PARTS || data.colors.len() != PART_LEN {
        return Ok(());
    }
    let part = part as usize;
    with_cache(|cache| {
        let (colors, got) = cache
            .partial
            .entry(id)
            .or_insert_with(|| (vec![0; MAP_SIZE * MAP_SIZE], 0));
        colors[part * PART_LEN..(part + 1) * PART_LEN].copy_from_slice(&data.colors);
        *got |= 1 << part;
        if *got as usize != (1 << PARTS) - 1 {
            return;
        }
        let Some((colors, _)) = cache.partial.remove(&id) else {
            return;
        };
        cache.store(id, &colors);
    });
    Ok(())
}
