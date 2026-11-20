//! The UMIK-1's ALSA capture control, via `amixer`.

use anyhow::Result;

use super::run;

/// A simple mixer control with capture volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureControl {
    pub value: i64,
    pub max: i64,
    pub switch_on: bool,
}

/// Name of the mixer control; the UMIK-1 exposes exactly one, `Mic`.
const CONTROL: &str = "Mic";

/// Reads the capture control on `card`, or `None` if it has none.
///
/// # Errors
///
/// Fails if amixer cannot be run.
pub fn capture_control(card: &str) -> Result<Option<CaptureControl>> {
    let Ok(out) = run("amixer", &["-c", card, "sget", CONTROL]) else {
        return Ok(None);
    };
    Ok(parse(&out))
}

fn parse(out: &str) -> Option<CaptureControl> {
    let mut max = None;
    let mut value = None;
    let mut switch_on = true;
    for line in out.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Limits:") {
            // "Limits: Capture 0 - 127"
            max = rest.split_whitespace().last().and_then(|v| v.parse().ok());
        } else if line.contains("Capture")
            && line.contains('[')
            && !line.starts_with("Capabilities")
        {
            // "Mono: Capture 127 [100%] [0.00dB] [on]"
            let after = line.split("Capture").nth(1)?;
            value = after.split_whitespace().next().and_then(|v| v.parse().ok());
            switch_on = !line.contains("[off]");
        }
    }
    Some(CaptureControl {
        value: value?,
        max: max?,
        switch_on,
    })
}

/// Sets the control to maximum (0 dB) and switched on, registering the undo step first.
///
/// # Errors
///
/// Fails if amixer fails.
pub fn set_unity(card: &str, current: &CaptureControl) -> Result<()> {
    let value = current.value.to_string();
    let switch = if current.switch_on { "cap" } else { "nocap" };
    let off = if current.switch_on {
        ""
    } else {
        ", switched off"
    };
    crate::restore::register_command(
        "mic-alsa",
        &format!(
            "The mic's ALSA capture level is back to {value}/{}{off}.",
            current.max
        ),
        &["amixer", "-q", "-c", card, "sset", CONTROL, &value, switch],
    );
    run(
        "amixer",
        &[
            "-q",
            "-c",
            card,
            "sset",
            CONTROL,
            &current.max.to_string(),
            "cap",
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_umik_output() {
        let out = "Simple mixer control 'Mic',0\n  Capabilities: cvolume cvolume-joined cswitch cswitch-joined\n  Capture channels: Mono\n  Limits: Capture 0 - 127\n  Mono: Capture 127 [100%] [0.00dB] [on]\n";
        assert_eq!(
            parse(out),
            Some(CaptureControl {
                value: 127,
                max: 127,
                switch_on: true
            })
        );
        let off = out.replace("127 [100%] [0.00dB] [on]", "80 [63%] [-23.50dB] [off]");
        assert_eq!(
            parse(&off),
            Some(CaptureControl {
                value: 80,
                max: 127,
                switch_on: false
            })
        );
    }
}
