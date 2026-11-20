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

use crate::audio::{AudioBackend, Route, Source, Stream};
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
type ErrorSlot = Arc<Mutex<Option<String>>>;

const MAX_STDERR_BYTES: u64 = 4096;

fn record_error(slot: &ErrorSlot, msg: String) {
    let mut guard = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(msg);
    }
}

fn spawn_stderr_reader(stderr: Option<std::process::ChildStderr>) -> thread::JoinHandle<String> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = stderr {
            let _ = s.take(MAX_STDERR_BYTES).read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).trim().to_owned()
    })
}

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

fn spawn_capture(
    mic: &str,
    children: &Shared,
    stop: &Arc<AtomicBool>,
    error: &ErrorSlot,
    tx: mpsc::Sender<Vec<f32>>,
) -> Result<()> {
    let mut child = pw_cat("--record", mic, 1, CAPTURE_NODE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting pw-cat --record (is pipewire installed?)")?;
    let mut stdout = child.stdout.take().context("pw-cat stdout")?;
    let stderr = spawn_stderr_reader(child.stderr.take());
    children
        .lock()
        .expect("child list lock poisoned")
        .push(child);
    let (children2, stop2, error2) = (Arc::clone(children), Arc::clone(stop), Arc::clone(error));
    thread::spawn(move || {
        let mut buf = vec![0u8; 4096 * 4];
        let mut carry: Vec<u8> = Vec::new();
        let mut receiver_gone = false;
        loop {
            let n = match stdout.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            carry.extend_from_slice(&buf[..n]);
            let whole = carry.len() / 4 * 4;
            let samples: Vec<f32> = carry[..whole]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            carry.drain(..whole);
            if tx.send(samples).is_err() {
                receiver_gone = true;
                break;
            }
        }
        if receiver_gone || stop2.load(Ordering::Acquire) {
            return;
        }
        // stdout closed unexpectedly: check whether pw-cat exited with an error.
        let mut status = None;
        for _ in 0..10 {
            if stop2.load(Ordering::Acquire) {
                return;
            }
            {
                let mut list = children2
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(c) = list.first_mut()
                    && let Ok(Some(s)) = c.try_wait()
                {
                    status = Some(s);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(20));
        }
        if let Some(s) = status
            && !s.success()
        {
            let msg = stderr.join().unwrap_or_default();
            let text = if msg.is_empty() {
                format!("recording stopped unexpectedly ({s})")
            } else {
                format!("recording stopped: {msg}")
            };
            record_error(&error2, text);
        }
    });
    Ok(())
}

/// Writes audio frames to `stdin`; returns `true` if `stdin` broke before `source` finished
/// and before `stop` was requested.
fn write_loop(mut stdin: ChildStdin, mut source: Source, stop: &AtomicBool) -> bool {
    let mut frames = vec![0f32; 1024 * 2];
    let mut bytes = Vec::with_capacity(frames.len() * 4);
    while !stop.load(Ordering::Acquire) && !source.is_done() {
        source.fill(&mut frames);
        bytes.clear();
        for v in &frames {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        if stdin.write_all(&bytes).is_err() {
            return !stop.load(Ordering::Acquire);
        }
    }
    false
}

impl AudioBackend for PwCat {
    fn start(&mut self, source: Source, route: Route) -> Result<Stream> {
        let children: Shared = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let finished = Arc::new(AtomicBool::new(false));
        let error: ErrorSlot = Arc::new(Mutex::new(None));
        let (tx, rx) = mpsc::channel();
        spawn_capture(&self.mic, &children, &stop, &error, tx)?;

        if !source.is_silent() {
            // Let the capture stream start first so the pre-roll is recorded.
            thread::sleep(Duration::from_millis(200));
            let target = match route {
                Route::Hardware => self.sink.as_str(),
                Route::ThroughEq => super::EASYEFFECTS_SINK,
            };
            let mut child = match pw_cat("--playback", target, 2, PLAYBACK_NODE)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    kill_all(&children);
                    return Err(e).context("starting pw-cat --playback");
                }
            };
            let stdin = child.stdin.take().context("pw-cat stdin")?;
            let stderr = spawn_stderr_reader(child.stderr.take());
            children
                .lock()
                .expect("child list lock poisoned")
                .push(child);
            let (stop2, finished2, children2, error2) = (
                Arc::clone(&stop),
                Arc::clone(&finished),
                Arc::clone(&children),
                Arc::clone(&error),
            );
            thread::spawn(move || {
                let write_failed = write_loop(stdin, source, &stop2);
                // stdin is closed here; pw-cat drains its buffer and exits.
                let mut exit_status = None;
                loop {
                    let done = {
                        let mut list = children2
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        match list.get_mut(1) {
                            None => true,
                            Some(c) => match c.try_wait() {
                                Ok(Some(s)) => {
                                    exit_status = Some(s);
                                    true
                                }
                                Ok(None) => false,
                                Err(_) => true,
                            },
                        }
                    };
                    if done || stop2.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                if !stop2.load(Ordering::Acquire) {
                    if let Some(s) = exit_status
                        && !s.success()
                    {
                        let msg = stderr.join().unwrap_or_default();
                        let text = if msg.is_empty() {
                            format!("playback stopped unexpectedly ({s})")
                        } else {
                            format!("playback stopped: {msg}")
                        };
                        record_error(&error2, text);
                    } else if write_failed {
                        record_error(
                            &error2,
                            "playback stream closed before the audio finished".into(),
                        );
                    }
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
        )
        .with_error(error))
    }
}
