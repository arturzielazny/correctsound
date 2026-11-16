//! Audio I/O: a playback source played on one channel while the mic is recorded.
//!
//! [`AudioBackend`] is implemented per platform (see [`crate::platform`]) and by the
//! simulator in [`sim`].

pub mod scene;
pub mod sim;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// A stereo output channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Channel {
    Left,
    Right,
}

impl Channel {
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];

    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
        }
    }

    #[must_use]
    pub fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// Where playback is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Straight to the output, bypassing the EQ host.
    Hardware,
    /// Through the EQ host, so the loaded EQ applies (verification only).
    ThroughEq,
}

/// What to play.
#[derive(Debug, Clone)]
pub enum Program {
    /// Nothing: record only.
    Silence,
    /// Loop the signal forever, fading in over `fade_in` samples.
    Loop { signal: Arc<[f32]>, fade_in: usize },
    /// `pre` samples of silence, the signal once, then `post` samples of silence.
    Once {
        signal: Arc<[f32]>,
        pre: usize,
        post: usize,
    },
}

/// A program bound to a channel, rendered block by block into interleaved stereo.
#[derive(Debug, Clone)]
pub struct Source {
    program: Program,
    channel: Channel,
    pos: usize,
}

impl Source {
    #[must_use]
    pub fn new(program: Program, channel: Channel) -> Self {
        Self {
            program,
            channel,
            pos: 0,
        }
    }

    /// Total frames for a `Once` program; `None` for endless programs.
    #[must_use]
    pub fn total_frames(&self) -> Option<usize> {
        match &self.program {
            Program::Once { signal, pre, post } => Some(pre + signal.len() + post),
            _ => None,
        }
    }

    /// True when there is nothing to play (record only).
    #[must_use]
    pub fn is_silent(&self) -> bool {
        matches!(self.program, Program::Silence)
    }

    /// True once a `Once` program has played everything including the trailing silence.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.total_frames().is_some_and(|t| self.pos >= t)
    }

    /// Frames rendered so far.
    #[must_use]
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Fills interleaved stereo `out`; frames past the end of a `Once` program are silent.
    pub fn fill(&mut self, out: &mut [f32]) {
        let ch = self.channel.index();
        let (frames, _) = out.as_chunks_mut::<2>();
        for frame in frames {
            frame[0] = 0.0;
            frame[1] = 0.0;
            frame[ch] = self.next_sample();
        }
    }

    /// Fills a mono buffer with the samples for the active channel.
    pub fn fill_mono(&mut self, out: &mut [f32]) {
        for v in out {
            *v = self.next_sample();
        }
    }

    fn next_sample(&mut self) -> f32 {
        let i = self.pos;
        self.pos += 1;
        match &self.program {
            Program::Silence => 0.0,
            Program::Loop { signal, fade_in } => {
                let v = signal[i % signal.len()];
                if i < *fade_in {
                    v * (i as f32 / *fade_in as f32).powi(2)
                } else {
                    v
                }
            }
            Program::Once { signal, pre, .. } => {
                if i >= *pre && i - pre < signal.len() {
                    signal[i - pre]
                } else {
                    0.0
                }
            }
        }
    }
}

/// A running stream: captured mic samples arrive on `capture`; dropping it stops audio.
pub struct Stream {
    pub capture: Receiver<Vec<f32>>,
    finished: Arc<AtomicBool>,
    stop: Option<Box<dyn FnOnce() + Send>>,
}

impl Stream {
    /// Creates a stream; `stop` is called once on drop.
    pub fn new(
        capture: Receiver<Vec<f32>>,
        finished: Arc<AtomicBool>,
        stop: Box<dyn FnOnce() + Send>,
    ) -> Self {
        Self {
            capture,
            finished,
            stop: Some(stop),
        }
    }

    /// True when a `Once` program has finished playing.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Drains all captured samples received so far.
    #[must_use]
    pub fn drain(&self) -> Vec<f32> {
        let mut out = Vec::new();
        while let Ok(block) = self.capture.try_recv() {
            out.extend(block);
        }
        out
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop();
        }
    }
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("finished", &self.is_finished())
            .finish()
    }
}

/// Plays a source and records the mic at 48 kHz.
pub trait AudioBackend: Send {
    /// Starts playing `source` via `route` while capturing from the mic.
    ///
    /// # Errors
    ///
    /// Fails if the streams cannot be created or the mic or sink is missing.
    fn start(&mut self, source: Source, route: Route) -> Result<Stream>;
}
