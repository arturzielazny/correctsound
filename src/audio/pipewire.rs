//! PipeWire backend built on `pw-cat` subprocesses (raw f32 over pipes).
//!
//! Both streams set `node.dont-move`, which stops EasyEffects' "process all
//! outputs" from re-targeting the measurement stream to its own sink.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use super::{AudioBackend, Program, Route, Source, Stream};
use crate::dsp::SAMPLE_RATE;

/// Node name of the playback stream, used to check where it is linked.
pub const PLAYBACK_NODE: &str = "correctsound-playback";
/// Node name of the capture stream.
pub const CAPTURE_NODE: &str = "correctsound-capture";

/// Plays to a sink and records from the mic with `pw-cat`.
#[derive(Debug, Clone)]
pub struct PwCat {
    mic: String,
    sink: String,
}

impl PwCat {
    /// Backend for the mic and sink node names.
    #[must_use]
    pub fn new(mic: &str, sink: &str) -> Self {
        Self {
            mic: mic.to_owned(),
            sink: sink.to_owned(),
        }
    }
}

fn properties(node: &str) -> String {
    format!(
        "{{ node.name = {node} application.name = CorrectSound node.dont-move = true \
         node.dont-reconnect = true state.restore-props = false }}"
    )
}

fn pw_cat(mode: &str, target: &str, channels: u32, node: &str) -> Command {
    let mut cmd = Command::new("pw-cat");
    cmd.args([
        mode,
        "--raw",
        "--target",
        target,
        "--rate",
        &SAMPLE_RATE.to_string(),
        "--channels",
        &channels.to_string(),
        "--format",
        "f32",
        "--latency",
        "1024",
        "--volume",
        "1.0",
        "-P",
        &properties(node),
        "-",
    ])
    .stderr(Stdio::null());
    cmd
}

type Shared = Arc<Mutex<Vec<Child>>>;

fn kill_all(children: &Shared) {
    let mut list = children
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for child in list.iter_mut() {
        let _ = child.kill();
        let _ = child.wait();
    }
    list.clear();
}

fn spawn_capture(mic: &str, children: &Shared, tx: mpsc::Sender<Vec<f32>>) -> Result<()> {
    let mut child = pw_cat("--record", mic, 1, CAPTURE_NODE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .context("starting pw-cat --record (is pipewire installed?)")?;
    let mut stdout = child.stdout.take().context("pw-cat stdout")?;
    children
        .lock()
        .expect("child list lock poisoned")
        .push(child);
    thread::spawn(move || {
        let mut buf = vec![0u8; 4096 * 4];
        let mut carry: Vec<u8> = Vec::new();
        loop {
            let n = match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            carry.extend_from_slice(&buf[..n]);
            let whole = carry.len() / 4 * 4;
            let samples: Vec<f32> = carry[..whole]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
            carry.drain(..whole);
            if tx.send(samples).is_err() {
                break;
            }
        }
    });
    Ok(())
}

fn write_loop(mut stdin: ChildStdin, mut source: Source, stop: &AtomicBool) {
    let mut frames = vec![0f32; 1024 * 2];
    let mut bytes = Vec::with_capacity(frames.len() * 4);
    while !stop.load(Ordering::Acquire) && !source.is_done() {
        source.fill(&mut frames);
        bytes.clear();
        for v in &frames {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        if stdin.write_all(&bytes).is_err() {
            break;
        }
    }
}

impl AudioBackend for PwCat {
    fn start(&mut self, source: Source, route: Route) -> Result<Stream> {
        let children: Shared = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        spawn_capture(&self.mic, &children, tx)?;

        if !matches!(source.program, Program::Silence) {
            // Let the capture stream start first so the pre-roll is recorded.
            thread::sleep(Duration::from_millis(200));
            let target = match route {
                Route::Hardware => self.sink.as_str(),
                Route::EasyEffects => "easyeffects_sink",
            };
            let mut child = match pw_cat("--playback", target, 2, PLAYBACK_NODE)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    kill_all(&children);
                    return Err(e).context("starting pw-cat --playback");
                }
            };
            let stdin = child.stdin.take().context("pw-cat stdin")?;
            children
                .lock()
                .expect("child list lock poisoned")
                .push(child);
            let (stop2, finished2, children2) = (
                Arc::clone(&stop),
                Arc::clone(&finished),
                Arc::clone(&children),
            );
            thread::spawn(move || {
                write_loop(stdin, source, &stop2);
                // stdin is closed here; pw-cat drains its buffer and exits.
                loop {
                    let done = {
                        let mut list = children2
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        list.get_mut(1)
                            .is_none_or(|c| c.try_wait().map_or(true, |s| s.is_some()))
                    };
                    if done || stop2.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                finished2.store(true, Ordering::Release);
            });
        }

        Ok(Stream::new(
            rx,
            finished,
            Box::new(move || {
                stop.store(true, Ordering::Release);
                kill_all(&children);
            }),
        ))
    }
}
