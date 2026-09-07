# hyperdu-gui

Desktop front-end for [HyperDU](https://github.com/automationjp/HyperDiskUsage),
built with egui. Scans a tree and shows where the space went.

For scripting and CI, use [`hyperdu-cli`](https://crates.io/crates/hyperdu-cli)
instead.

## Install

```bash
cargo install hyperdu-gui --version 0.5.0-beta.1
```

Needs the usual desktop graphics stack. On Linux that means the X11 or Wayland
development libraries your distribution ships for `winit`.

## License

MIT

Bundled fonts in the egui dependency chain carry their own licenses (OFL-1.1 and
Ubuntu-font-1.0).
