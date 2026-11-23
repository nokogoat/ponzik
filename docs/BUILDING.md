# Building from source

You need a Rust toolchain (1.85 or newer, edition 2024). On Linux you also need the
ALSA library (`alsa-lib`, or `libasound2-dev` on Debian and Ubuntu). There is nothing
else to install.

```sh
git clone https://github.com/nokogoat/ponzik
cd ponzik
cargo run --release              # opens Ponzik on the house demo
```

Other commands:

```sh
cargo run --release -- breakcore                     # the "Amen rave" demo (180 BPM)
cargo run --release -- zalgo                         # the "Zalgo Rave" demo (160 BPM)
cargo run --release -- render out.wav [morceau|pattern] [breakcore|zalgo]
cargo run --release -- library [dir]                 # generate the sound library
cargo run --release -- screenshot ui.png mixer       # off-screen capture of a view
cargo test                                           # the test suite
```

| Environment variable | Effect |
|---|---|
| `PONZIK_LATENCY_MS` | Audio buffer in milliseconds (2–100, default 10; also in Settings) |
| `PONZIK_LANG` | Interface language (`en`, `fr`), otherwise the one chosen in Settings |
| `CLAP_PATH`, `VST3_PATH` | Extra folders to scan for plugins |
