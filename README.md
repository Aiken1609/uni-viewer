<p align="center">
  <img src="icon.png" alt="Universal Viewer" width="256">
</p>

# Universal Viewer

A [Horizon](https://github.com/horizon-rust/horizon) mod (Minecraft 26.3 in Rust) to look inside things without opening them. Client-side: it works on any server, vanilla included.

## Features

- **Maps on their slot**: a filled map shows its own picture as its inventory icon, edge to edge, in the inventory, chests, the hotbar and creative tabs.
- **Map tooltip**: hover a map to see it larger in a vanilla tooltip, inside the game's own held-map frame (`map_background`, so a resource pack's frame shows too), with its **id**, **scale** (1:1 to 1:16) and whether it is **locked**.
- **Shulker boxes and bundles**: hold **Shift** while hovering to see the full contents as a slot grid, drawn like a vanilla inventory and tinted in the box's or bundle's colour (all 16 dyes, plus the plain ones). A red "Hold Shift to view contents" hint shows when Shift is up. Any item carrying `minecraft:container` or `minecraft:bundle_contents` works.
- **Placed shulker boxes**: opening one tints its screen (panel and slots) in the box's colour.

## How maps work

The game receives a map's picture from the server (vanilla's map data packet) whenever the map is in your inventory, in your hand or in a nearby item frame. A small client-side mixin copies that picture, so no server-side mod is needed. A map the client has never been sent keeps the vanilla icon and tooltip until it has been, for example once you have carried it.

In singleplayer, or on a server that also runs the mod, the mod can also ask the server for a map's picture directly, so maps sitting in a chest show at once.

Up to 64 map pictures are kept at a time; the least recently seen one makes room for a new one.

## Install

In the Horizon launcher: **Mods > Add mods**, from GitHub `Aiken1609/uni-viewer`, then **Build** and play the built version. It is a native mod: the launcher compiles it on your machine and asks you to trust native code. The first build also downloads the game's engine source for the map mixin, so it takes longer.

To build the package yourself (needs Rust and `cargo-horizon-mod`):

```sh
cargo install --locked --git https://github.com/horizon-rust/horizon cargo-horizon-mod
cargo horizon-mod build
```

This writes `target/horizon-mod/universal_viewer.hmod`; put it in the instance's `horizon-native-mods/` folder and build from the launcher.

## Notes

- Pinned to Horizon build `0b14f8acfd97fa2c750e42795249dcc981178ca4` (see `horizon-mod.toml`). The map mixin targets that build's code, so a newer Horizon needs the pin moved (`cargo horizon-mod update`) and the mod rebuilt.
- Maps do not record who made them, so the tooltip cannot show a creator.
- The placed shulker box tint follows the vanilla 176 x 166 screen layout; a resource pack that moves its slots will not line up.

## License

[MIT](LICENSE)

---

## Ringkas (Indonesia)

Mod Horizon untuk melihat isi tanpa membuka, sepenuhnya di sisi client sehingga jalan di server mana pun, termasuk server vanilla.

- **Map**: gambar map langsung tampil sebagai ikon slotnya. Saat di-hover, map tampil lebih besar di tooltip vanilla dengan bingkai map asli game, ditambah ID, skala, dan status locked/unlocked.
- **Shulker dan bundle**: tahan **Shift** sambil hover untuk melihat seluruh isinya dalam grid bergaya inventory vanilla, berwarna sesuai warna shulker atau bundle-nya.
- **Shulker yang ditaruh**: saat dibuka, layarnya ikut berwarna sesuai warna shulker-nya.

Map yang belum pernah dikirim server ke client (misalnya belum pernah Anda pegang) tetap memakai ikon vanilla sampai datanya diterima.
