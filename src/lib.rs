//! Universal Viewer: look inside things without opening them.
//!
//! * **Maps** show their picture in the tooltip as soon as you hover them (inventory, chests,
//!   creative tabs...). No key needed.
//! * **Shulker boxes** (and any other stack that carries `minecraft:container`) and **bundles**
//!   (`minecraft:bundle_contents`) show their whole contents as a slot grid while Shift is held.
//!
//! Containers and bundles are read straight from the stack's own components, so the client
//! needs nothing from the server. A map's pixels live only on the server (the client mod API has
//! no read of them), so the mod asks the server for them over a small optional payload and caches
//! the answer. On a server without this mod the map panel says so instead of drawing.

use mod_api::{
    ClientHost, ClientTickPreEvent, DrawContext, Flow, Hooks, IPayloadContext, ItemStack,
    PayloadCodec, PayloadId, Registrar, RegistrationContext, RenderTooltipGatherComponentsEvent,
    RenderTooltipPreEvent, Tag, Text, ToClient, ToServer, TooltipElement, VarInt,
};
use mod_api::text::TextContent;
use std::collections::HashMap;
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
/// Ticks after a request with no answer before the panel says the server can't provide it.
const GIVE_UP_TICKS: u64 = 60;

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
    /// `128 * 128` map colour ids (`base * 4 + shade`), row by row.
    colors: Vec<u8>,
}

static REQUEST: Mutex<Option<ToServer<MapRequest>>> = Mutex::new(None);
static REPLY: Mutex<Option<ToClient<MapData>>> = Mutex::new(None);

// ---------------------------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------------------------

struct CachedMap {
    rgba: Vec<u8>,
    fetched: u64,
}

#[derive(Default)]
struct Cache {
    /// Client ticks since the game started, a clock for refreshes and time-outs.
    tick: u64,
    maps: HashMap<i32, CachedMap>,
    /// When each map was last asked for.
    requested: HashMap<i32, u64>,
    /// The map whose tooltip was drawn since the last tick.
    wanted: Option<i32>,
    /// The map (and the fetch of it) currently in the atlas slot.
    uploaded: Option<(i32, u64)>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

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
}

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
        return Some(View::Contents(finish(COLUMNS, rows, slots)));
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
        return Some(View::Contents(finish(columns, rows, slots)));
    }
    None
}

fn finish(columns: usize, rows: usize, slots: Vec<(usize, ItemStack)>) -> Contents {
    let items = slots.iter().map(|(_, s)| u64::from(s.count())).sum();
    Contents {
        columns,
        rows,
        slots,
        items,
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
fn panel(draw: &mut dyn DrawContext, cursor: [i32; 2], screen: [i32; 2], w: f32, h: f32) -> [f32; 2] {
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

    const BACKGROUND: u32 = 0xF010_0010;
    const TOP: u32 = 0xF050_00FF;
    const BOTTOM: u32 = 0xF028_007F;
    draw.fill([x - 3.0, y - 4.0, w + 6.0, h + 8.0], BACKGROUND);
    draw.fill([x - 3.0, y - 4.0, w + 6.0, 1.0], TOP);
    draw.fill([x - 3.0, y + h + 3.0, w + 6.0, 1.0], BOTTOM);
    draw.fill_gradient([x - 3.0, y - 3.0, 1.0, h + 6.0], TOP, BOTTOM);
    draw.fill_gradient([x + w + 2.0, y - 3.0, 1.0, h + 6.0], TOP, BOTTOM);
    [x, y]
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
    let h = 12.0 + grid_h + 12.0;
    let [x, y] = panel(draw, cursor, screen, w, h);

    draw.text(title, [x, y], TEXT_WHITE, true);

    let gx = x + ((w - grid_w) / 2.0).floor();
    let gy = y + 12.0;
    for row in 0..contents.rows {
        for column in 0..contents.columns {
            let sx = gx + column as f32 * SLOT;
            let sy = gy + row as f32 * SLOT;
            draw.fill([sx, sy, SLOT, SLOT], 0xFF8B_8B8B);
            draw.fill([sx + 1.0, sy + 1.0, SLOT - 2.0, SLOT - 2.0], 0xFF37_3737);
        }
    }
    for (slot, stack) in &contents.slots {
        let sx = gx + (slot % contents.columns) as f32 * SLOT;
        let sy = gy + (slot / contents.columns) as f32 * SLOT;
        let count = (stack.count() > 1).then(|| stack.count().to_string());
        draw.gui_draw_item_with(stack, [sx + 1.0, sy + 1.0], count.as_deref());
    }
    draw.text(&footer, [x, gy + grid_h + 3.0], TEXT_GRAY, true);
}

/// What the map panel can show right now.
enum MapState {
    Ready,
    Loading,
    Unavailable,
}

fn draw_map(
    draw: &mut dyn DrawContext,
    cursor: [i32; 2],
    screen: [i32; 2],
    title: &str,
    id: i32,
    state: MapState,
) {
    let image = MAP_SIZE as f32 * MAP_SCALE;
    let footer = format!("Map #{id}");
    let w = (image + 2.0)
        .max(draw.text_width(title))
        .max(draw.text_width(&footer));
    let h = 12.0 + image + 2.0 + 12.0;
    let [x, y] = panel(draw, cursor, screen, w, h);

    draw.text(title, [x, y], TEXT_WHITE, true);

    let ix = x + ((w - image - 2.0) / 2.0).floor();
    let iy = y + 12.0;
    // Frame, then the paper the map is drawn on.
    draw.fill([ix, iy, image + 2.0, image + 2.0], 0xFF00_0000);
    draw.fill([ix + 1.0, iy + 1.0, image, image], 0xFFD1_B380);
    match state {
        MapState::Ready => {
            let size = MAP_SIZE as f32;
            draw.sprite(
                MOD_IMAGE_SPRITE,
                [0.0, 0.0, size, size],
                [ix + 1.0, iy + 1.0, image, image],
                None,
            );
        }
        MapState::Loading => {
            draw.text("Loading map...", [ix + 8.0, iy + 8.0], 0xFF40_3020, false);
        }
        MapState::Unavailable => {
            draw.text("Map data unavailable", [ix + 8.0, iy + 8.0], 0xFF40_3020, false);
            draw.text("(server needs this mod)", [ix + 8.0, iy + 20.0], 0xFF40_3020, false);
        }
    }
    draw.text(&footer, [x, iy + image + 5.0], TEXT_GRAY, true);
}


/// Lines for the Ctrl+Shift debug panel.
fn debug_lines(host: &dyn ClientHost, stack: &ItemStack) -> Vec<String> {
    let mut lines = vec![
        "Universal Viewer debug (hook is running)".to_string(),
        format!("count: {}", stack.count()),
    ];
    match stack.components().map(|c| c.as_nbt()) {
        None => lines.push("components: none (no patch on this stack)".to_string()),
        Some(patch) => {
            lines.push(format!("components: {}", patch.len()));
            for key in patch.keys().take(10) {
                lines.push(format!("  {}", key.to_string_lossy()));
            }
        }
    }
    let kind = match view_of(host, stack, true) {
        Some(View::Map(id)) => format!("view: map #{id}"),
        Some(View::Contents(c)) => format!("view: contents, {} slots", c.slots.len()),
        None => "view: none".to_string(),
    };
    lines.push(kind);
    lines
}

fn draw_debug(draw: &mut dyn DrawContext, cursor: [i32; 2], screen: [i32; 2], lines: &[String]) {
    let w = lines
        .iter()
        .map(|line| draw.text_width(line))
        .fold(0.0_f32, f32::max);
    let h = lines.len() as f32 * 10.0;
    let [x, y] = panel(draw, cursor, screen, w, h);
    for (index, line) in lines.iter().enumerate() {
        let color = if index == 0 { TEXT_WHITE } else { TEXT_GRAY };
        draw.text(line, [x, y + index as f32 * 10.0], color, true);
    }
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
    }

    /// Once a tick: put the hovered map in the atlas slot, or ask the server for it.
    fn client_tick_pre(&self, host: &mut dyn ClientHost, _: &mut ClientTickPreEvent) {
        enum Step {
            Nothing,
            Upload(i32, u64, Vec<u8>),
            Ask(i32),
        }
        let step = with_cache(|cache| {
            cache.tick += 1;
            let Some(id) = cache.wanted.take() else {
                return Step::Nothing;
            };
            let asked = cache.requested.get(&id).copied();
            let due = asked.is_none_or(|at| cache.tick.saturating_sub(at) > REFRESH_TICKS);
            match cache.maps.get(&id) {
                Some(map) if cache.uploaded != Some((id, map.fetched)) => {
                    Step::Upload(id, map.fetched, map.rgba.clone())
                }
                Some(_) if due => {
                    cache.requested.insert(id, cache.tick);
                    Step::Ask(id)
                }
                Some(_) => Step::Nothing,
                None if due => {
                    cache.requested.insert(id, cache.tick);
                    Step::Ask(id)
                }
                None => Step::Nothing,
            }
        });
        match step {
            Step::Nothing => {}
            Step::Upload(id, fetched, rgba) => {
                if host.mod_image(MAP_SIZE as u32, MAP_SIZE as u32, &rgba) {
                    with_cache(|cache| cache.uploaded = Some((id, fetched)));
                }
            }
            Step::Ask(id) => {
                let token = REQUEST.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(token) = token {
                    // An unsupported peer (a server without the mod) just never answers.
                    let _ = token.send(host, &MapRequest { id: VarInt(id) });
                }
            }
        }
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
                0xAAAA_AAFF,
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
        if shift && modifiers.control() {
            // Debug panel (Ctrl+Shift while hovering any item): proves the hook runs and shows
            // which components the stack carries.
            let cursor = [event.place.x, event.place.y];
            let screen = [event.screen_width, event.screen_height];
            let lines = debug_lines(host, stack);
            draw_debug(&mut *event.draw, cursor, screen, &lines);
            return Flow::Cancel;
        }
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
                let state = with_cache(|cache| {
                    cache.wanted = Some(id);
                    let live = cache.maps.get(&id).map(|map| map.fetched);
                    match live {
                        Some(fetched) if cache.uploaded == Some((id, fetched)) => MapState::Ready,
                        Some(_) => MapState::Loading,
                        None => match cache.requested.get(&id) {
                            Some(at) if cache.tick.saturating_sub(*at) > GIVE_UP_TICKS => {
                                MapState::Unavailable
                            }
                            _ => MapState::Loading,
                        },
                    }
                });
                draw_map(draw, cursor, screen, &title, id, state);
            }
        }
        Flow::Cancel
    }
}

// ---------------------------------------------------------------------------------------------
// Payload handlers
// ---------------------------------------------------------------------------------------------

/// Server: a client wants map `id`. Answer with its colours when the world has it.
fn on_map_request(request: MapRequest, cx: &mut dyn IPayloadContext) -> Result<(), String> {
    let id = request.id.0;
    let colors = {
        let host = cx.server_host().map_err(|e| format!("no server host: {e:?}"))?;
        // Maps belong to the world, so any level reaches them.
        let level = host.level("minecraft:overworld").ok_or("no overworld")?;
        level.map_data(host, id).map(|state| state.colors)
    };
    let Some(colors) = colors else {
        return Ok(());
    };
    if colors.len() != MAP_SIZE * MAP_SIZE {
        return Ok(());
    }
    let token = REPLY
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .ok_or("reply token missing")?;
    token
        .reply(
            cx,
            &MapData {
                id: VarInt(id),
                colors,
            },
        )
        .map(|_| ())
        .map_err(|e| format!("map reply: {e:?}"))
}

/// Client: the server sent a map's colours. Keep them as RGBA until it is hovered.
fn on_map_data(data: MapData, _: &mut dyn IPayloadContext) -> Result<(), String> {
    if data.colors.len() != MAP_SIZE * MAP_SIZE {
        return Err(format!("map {} has {} pixels", data.id.0, data.colors.len()));
    }
    let rgba = to_rgba(&data.colors);
    with_cache(|cache| {
        let fetched = cache.tick;
        cache.maps.insert(data.id.0, CachedMap { rgba, fetched });
        // Make sure a changed picture is uploaded again even in the same tick.
        if cache.uploaded.is_some_and(|(id, _)| id == data.id.0) {
            cache.uploaded = None;
        }
    });
    Ok(())
}
