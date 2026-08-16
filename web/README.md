# Browser frontend

This is the static browser frontend for `snes-rs`. The Rust/WebAssembly
adapter lives in `crates/snes-wasm`; this directory contains the HTML, CSS,
JavaScript, and development server. The generated `web/pkg/` directory is a
build artifact and should not be committed.

From the repository root:

```sh
wasm-pack build crates/snes-wasm --target web --out-dir ../../web/pkg
python3 web/dev_server.py --host 127.0.0.1 --port 8000
```

Open <http://localhost:8000/> and choose an unheadered or copier-headered
SNES `.sfc`/`.smc` ROM. Use `--host 0.0.0.0` when testing from another device
on the local network.

The page provides keyboard and touch controls, optional Web Audio playback,
save/restore state, and hold-to-rewind. Web Audio is resampled to the browser
output rate when needed. Add `?debug=1` to collect browser errors at
`/__debug` while developing.
