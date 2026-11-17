# CorrectSound

**Room correction for Linux, in a terminal.** Plug in a miniDSP UMIK-1, answer a few prompts,
and a measured, verified equalizer for your speakers and room is loaded into EasyEffects. Carry
a bass trap around the room and watch the bass change as you walk.

**Everything else is on the website: how it works, a tour of every screen, the science, and
what's new. <https://arturzielazny.github.io/correctsound/>**

![The result screen: measured, target and predicted response above the equalizer's curve](docs/img/6-result.png)

> Written end to end by an AI coding agent (Claude), with one person testing it on real
> speakers. It is meant to be taken apart: give it to your own AI agent and make it fit your
> setup (see [Make it yours](#make-it-yours)).

## Install and run

```sh
curl -fsSL https://arturzielazny.github.io/correctsound/install.sh | sh
correctsound --simulate    # the whole app against a simulated room, no hardware, any OS
correctsound               # measure your speakers
correctsound --window      # in a window of its own, drawn on the GPU (experimental)
```

The script installs the ready-made program for Linux or macOS from the
[latest release](https://github.com/arturzielazny/correctsound/releases/latest) into
`~/.local/bin`; Windows has a zip there. From source, with Rust:
`cargo install --git https://github.com/arturzielazny/correctsound --features window`.

On Linux it needs PipeWire (`pw-cat`, `pw-dump`), WirePlumber (`wpctl`) and `alsa-utils`
(`amixer`). EasyEffects is optional: without it you still get the EQ files. On macOS and Windows
only the simulator and saved sessions work so far.

Seven steps, one key each: devices, calibration, mic gain, level, measure, result, save. On the
way, `p` opens live placement for room treatment, `r` your room seen from above, `w` the bass
decay, `c` curiosities about your room, `i` how it works. `Esc` goes back, `q` quits, and
everything the app changes is put back when it exits.

Other commands: `correctsound sessions` lists saved measurements, `correctsound review [N]`
opens one again, `correctsound fit SESSION_DIR` redoes its EQ, `correctsound devices` shows what
step 1 sees. `correctsound --help` has the rest.

## Questions and feedback

Ask, share your results or suggest ideas in
[Discussions](https://github.com/arturzielazny/correctsound/discussions); report bugs in
[Issues](https://github.com/arturzielazny/correctsound/issues).

## Make it yours

CorrectSound does one setup well: Linux, PipeWire, EasyEffects, a UMIK-1, two speakers. Yours
may differ: macOS, Windows, another mic, a subwoofer, CamillaDSP. **Clone the repository and give
it to your AI coding agent.** [CONTRIBUTING.md](CONTRIBUTING.md) has prompts to start from,
[AGENTS.md](AGENTS.md) the rules and the code's layout, [IDEAS.md](IDEAS.md) what is planned.
When it works for you, open a pull request.

```sh
cargo test                  # DSP, fitting, parsers, and the whole wizard on the simulator
cargo run -- --simulate     # the app, no hardware
```

## License

[MIT](LICENSE). Take it, change it, ship it.
