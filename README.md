![Universal Viewer](icon.png)

# Universal Viewer

A [Horizon](https://github.com/horizon-rust/horizon) mod (Minecraft 26.3 in Rust) to look inside things without opening them.

- **Maps**: hover a map item (inventory, chests, creative tabs) and its picture shows in the tooltip.
- **Shulker boxes / bundles**: hold **Shift** while hovering to see the full contents as a slot grid. Any item carrying `minecraft:container` or `minecraft:bundle_contents` works.

## Install

1. Build the package (needs Rust and `cargo-horizon-mod`):
   ```sh
   cargo install --locked --git https://github.com/horizon-rust/horizon cargo-horizon-mod
   cargo horizon-mod build
   ```
   This writes `target/horizon-mod/universal_viewer.hmod`.
2. Copy it into the instance's `horizon-mods/` folder and add `mod.universal_viewer:true` to `horizon-mods.txt`.
3. It is a native mod: the launcher compiles it and asks you to trust native code.

## Notes

- Container and bundle contents are read from the item's own components, client-side only.
- The client API cannot read map pixels, so the mod requests them from the server over an optional payload. Works in singleplayer; on multiplayer the server needs the mod too (otherwise the panel says "Map data unavailable").
- Pinned to Horizon build `0b14f8acfd97fa2c750e42795249dcc981178ca4` (see `horizon-mod.toml`). Move it with `cargo horizon-mod update`.
- Known limits: the server answers map requests for any id without checking the player holds that map; only the overworld level is queried; map colour table is vanilla's.

## License

MIT [LICENSE](https://github.com/Aiken1609/uni-viewer/blob/c630ea4c1a7042f2334837cf777a718cce42c096/LICENSE)

---

## Ringkas (Indonesia)

Mod Horizon untuk melihat isi tanpa membuka: hover map untuk preview gambar, tahan Shift + hover pada shulker/bundle untuk melihat seluruh isinya. Build dengan `cargo horizon-mod build`, taruh `.hmod` di `horizon-mods/`. Di multiplayer, mod harus ada di server agar preview map jalan.
