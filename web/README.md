# CorrectSound in a web page

The page at <https://arturzielazny.github.io/correctsound/try/> runs the app in the browser
against the simulated room: the same wizard, screens and keys as `correctsound --simulate`
in a terminal. Nothing is played or recorded. This is stage 1 of the web version; IDEAS.md
has the next ones (a real mic and speakers through the browser, checking the EQ in the page).

## How it works

- The library (`correctsound`) is compiled to WebAssembly unchanged. What a browser lacks is
  behind small modules that pick their web version when building for it:
  - threads: `src/background.rs` (work waits in a queue and runs between frames);
  - files: `src/files.rs` (kept in memory while the page is open);
  - the clock: the `web-time` crate;
  - the terminal, signals and downloads: only built outside the browser (Cargo.toml).
- ratatui lays the screen out in cells as always. `src/ui/cells.rs` turns the cells into
  rectangles and runs of text, the same ones the window draws. The charts are drawn by the
  app into pixels (`Graphics::pixels`), each again only when it changes.
- This crate draws all of it on a canvas (`src/screen.rs`), turns key presses into the app's
  keys (`src/keys.rs`), and runs the app's loop on the browser's frames (`src/lib.rs`).
- The text is JetBrains Mono (`docs/try/JetBrainsMono-Regular.ttf`, SIL Open Font License,
  `docs/try/JetBrainsMono-OFL.txt`), sized so a character is exactly a cell wide.

## Building

The page loads the built program from `docs/try/pkg/`, which is committed so GitHub Pages
can serve it. Rebuild it after changing the app, so the page shows the current version:

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked   # the version web/Cargo.toml names
./web/build.sh
```

Try it with any static web server, for example:

```sh
python3 -m http.server --directory docs   # then open http://localhost:8000/try/
```
