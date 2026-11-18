<p align="center">
  <img src="icon.png" alt="Universal Viewer" width="256">
</p>

<h1 align="center">Universal Viewer</h1>

<p align="center">
  Lihat isi map, shulker box, dan bundle tanpa membukanya, untuk Horizon (Minecraft Java 26.3 versi Rust).<br>
  Sepenuhnya di sisi client, jadi jalan di server mana pun, termasuk server vanilla.
</p>

## Fitur

- **Gambar map di slot**: map yang sudah termuat menampilkan gambarnya sendiri sebagai ikon, penuh tanpa bingkai,
  di inventory, chest, hotbar, dan tab creative.
- **Tooltip map**: hover map untuk melihatnya lebih besar di tooltip vanilla, di dalam bingkai map asli game, lengkap
  dengan **ID**, **skala** (1:1 sampai 1:16), dan status **Locked/Unlocked**.
- **Isi shulker box dan bundle**: tahan **Shift** sambil hover untuk melihat seluruh isinya dalam grid bergaya
  inventory vanilla, berwarna sesuai warna shulker atau bundle-nya (16 warna dye plus versi polos).
- **Shulker box yang ditaruh**: saat dibuka, layarnya ikut berwarna sesuai warna shulker-nya.

## Persyaratan

- Horizon launcher **0.2.73** atau lebih baru
- Instance di Nightly **0b14f8a** (18 November 2026). Mod ini memakai mixin untuk membaca data map, dan mixin terikat
  ke build game yang dituju, jadi Nightly lain butuh mod yang sudah dipindah ke build itu.
- Mod native dikompilasi di komputer kamu dan berjalan dengan hak akses yang sama seperti game, tanpa sandbox.
  Baca source-nya dulu kalau perlu.

## Pasang

1. Di launcher, buka **Mods**, lalu bagian *Add a native mod from GitHub*.
2. Tempel `https://github.com/Aiken1609/uni-viewer` dan klik **Add from GitHub**.
3. Pilih instance di Nightly yang sesuai, klik **Build**, dan setujui peringatan trust. Build pertama bisa lama
   karena launcher mengunduh toolchain Rust, SDK, dan source engine untuk mixin.
4. Tunggu sampai **Installing the build** selesai, lalu mainkan instance itu dengan versi `native-...` hasil build.

Alternatif lewat terminal:

```
horizon-launcher native-build . --instance "NamaInstance" --trust-native-code
```

## Cara pakai

| Aksi | Cara |
|---|---|
| Lihat gambar map | Langsung tampil di ikon slot |
| Lihat map lebih besar beserta ID, skala, dan status lock | Hover map |
| Lihat isi shulker box atau bundle | Tahan `Shift` sambil hover |
| Shulker box berwarna saat dibuka | Klik kanan shulker box yang ditaruh |

Tanpa `Shift`, shulker box dan bundle menampilkan petunjuk merah "Hold Shift to view contents". Tidak ada keybind
atau pengaturan tambahan.

## Batasan

- Gambar map berasal dari data yang dikirim server ke client, yaitu untuk map yang pernah ada di inventory, di tangan,
  atau di item frame dekat kamu. Map yang belum pernah dikirim tetap memakai ikon dan tooltip vanilla sampai datanya
  diterima. Di singleplayer, dan di server yang juga memasang mod ini, map di chest langsung tampil.
- Maksimal 64 gambar map disimpan sekaligus. Map yang paling lama tidak terlihat diganti saat ada map baru.
- Minecraft tidak mencatat siapa pembuat map, jadi tooltip tidak bisa menampilkannya.
- Warna layar shulker yang ditaruh mengikuti tata letak layar vanilla (176 x 166). Resource pack yang memindahkan
  slotnya akan membuat warnanya tidak pas. Warna ditentukan dari blok yang kamu klik kanan, jadi shulker yang dibuka
  lewat command atau plugin tetap tampil vanilla.

## Untuk pengembang

```
horizon-mod.toml           manifest, pin build Horizon, dan baris mixin
Cargo.toml                 depends on horizon-mod-api
src/lib.rs                 hook ke game, cache map, dan semua tampilan
src/mixins/vanilla_map.rs  mixin yang menyalin paket map vanilla di client
icon.png                   ikon mod (PNG persegi, maks 512 x 512)
```

Data map dibaca lewat mixin di `remote_map_item_data`, karena API mod belum bisa membaca pixel map di client.
Sebagai cadangan, mod juga punya payload opsional untuk meminta gambar map ke server yang memasang mod ini.
Untuk pindah build, ganti `horizon = "..."` di `horizon-mod.toml` dengan commit Nightly tujuan (atau
`cargo horizon-mod update`), lalu pastikan target mixin masih ada di build itu.

## Lisensi

MIT. Bingkai map memakai texture `map_background` milik game itu sendiri saat runtime; repo ini tidak menyertakan
aset Minecraft.
