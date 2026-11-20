//! PipeWire state via `pw-dump` and volumes via `wpctl`.

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::run;
use crate::platform::{Device, Devices};

/// USB vendor:product of the UMIK-1 as ALSA reports it.
const UMIK1_COMPONENTS: &str = "USB2752:0007";

/// Prefix of every node this app creates.
pub const OWN_NODE_PREFIX: &str = "correctsound";

/// A parsed `pw-dump`.
#[derive(Debug, Clone)]
pub struct Dump(Vec<Value>);

fn prop<'a>(o: &'a Value, key: &str) -> Option<&'a str> {
    o.pointer("/info/props")?.get(key)?.as_str()
}

/// A property as text, whether pw-dump stored it as a string or a number.
fn prop_text(o: &Value, key: &str) -> Option<String> {
    match o.pointer("/info/props")?.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn is_node(o: &Value) -> bool {
    o.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Node")
}

impl Dump {
    /// Runs `pw-dump`.
    ///
    /// # Errors
    ///
    /// Fails if `pw-dump` is missing or prints invalid JSON.
    pub fn load() -> Result<Self> {
        Self::parse(&run("pw-dump", &[])?)
    }

    /// Parses `pw-dump` output.
    ///
    /// # Errors
    ///
    /// Fails on invalid JSON.
    pub fn parse(text: &str) -> Result<Self> {
        let v: Value = serde_json::from_str(text).context("parsing pw-dump output")?;
        match v {
            Value::Array(objs) => Ok(Self(objs)),
            _ => bail!("pw-dump did not print an array"),
        }
    }

    fn nodes(&self) -> impl Iterator<Item = &Value> {
        self.0.iter().filter(|o| is_node(o))
    }

    fn node(o: &Value) -> Option<Device> {
        let positions: Vec<&str> = prop(o, "audio.position")
            .map(|p| {
                p.trim_matches(|c| c == '[' || c == ']')
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Some(Device {
            id: o.get("id")?.as_u64()?.to_string(),
            name: prop(o, "node.name")?.to_owned(),
            description: prop(o, "node.description")
                .or_else(|| prop(o, "node.nick"))
                .unwrap_or_default()
                .to_owned(),
            stereo: positions.contains(&"FL") && positions.contains(&"FR"),
        })
    }

    /// The ALSA card number behind the node called `name`, for its mixer controls.
    #[must_use]
    pub fn alsa_card(&self, name: &str) -> Option<String> {
        let o = self.nodes().find(|o| prop(o, "node.name") == Some(name))?;
        prop_text(o, "alsa.card").or_else(|| prop_text(o, "api.alsa.pcm.card"))
    }

    fn name_of(&self, id: u64) -> Option<&str> {
        self.nodes()
            .find(|o| o.get("id").and_then(Value::as_u64) == Some(id))
            .and_then(|o| prop(o, "node.name"))
    }

    /// The UMIK-1 source, if plugged in.
    #[must_use]
    pub fn mic(&self) -> Option<Device> {
        let sources = || {
            self.nodes()
                .filter(|o| prop(o, "media.class") == Some("Audio/Source"))
        };
        sources()
            .find(|o| prop(o, "alsa.components").is_some_and(|c| c.contains(UMIK1_COMPONENTS)))
            .or_else(|| {
                sources().find(|o| {
                    prop(o, "alsa.card_name").is_some_and(|c| c.to_uppercase().contains("UMIK"))
                })
            })
            .and_then(Self::node)
    }

    /// Description of the active route (port) of the device behind sink `node_name`, e.g.
    /// "HDMI / DisplayPort 4 Output": what EasyEffects keys its autoload files on.
    #[must_use]
    pub fn route_description(&self, node_name: &str) -> Option<String> {
        let node = self
            .nodes()
            .find(|o| prop(o, "node.name") == Some(node_name))?;
        let device_id: u64 = prop_text(node, "device.id")?.parse().ok()?;
        let profile_device: u64 = prop_text(node, "card.profile.device")?.parse().ok()?;
        let device = self
            .0
            .iter()
            .find(|o| o.get("id").and_then(Value::as_u64) == Some(device_id))?;
        device
            .pointer("/info/params/Route")?
            .as_array()?
            .iter()
            .find(|r| {
                r.get("direction").and_then(Value::as_str) == Some("Output")
                    && r.get("device").and_then(Value::as_u64) == Some(profile_device)
            })?
            .get("description")?
            .as_str()
            .map(str::to_owned)
    }

    /// The id of the node called `name`.
    #[must_use]
    pub fn node_id(&self, name: &str) -> Option<u32> {
        self.nodes()
            .find(|o| prop(o, "node.name") == Some(name))
            .and_then(|o| u32::try_from(o.get("id")?.as_u64()?).ok())
    }

    /// The node called `name`, if it exists in PipeWire.
    #[must_use]
    pub fn node_by_name(&self, name: &str) -> Option<Device> {
        self.nodes()
            .find(|o| prop(o, "node.name") == Some(name))
            .and_then(Self::node)
    }

    /// Hardware sinks (excludes EasyEffects and other virtual sinks).
    #[must_use]
    pub fn hardware_sinks(&self) -> Vec<Device> {
        self.nodes()
            .filter(|o| prop(o, "media.class") == Some("Audio/Sink"))
            .filter(|o| prop(o, "device.api").is_some() || prop(o, "api.alsa.path").is_some())
            .filter_map(Self::node)
            .collect()
    }

    /// Output streams from other applications.
    #[must_use]
    pub fn other_streams(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .nodes()
            .filter(|o| prop(o, "media.class") == Some("Stream/Output/Audio"))
            .filter(|o| {
                !prop(o, "node.name")
                    .unwrap_or_default()
                    .starts_with(OWN_NODE_PREFIX)
            })
            .filter(|o| !prop(o, "node.name").unwrap_or_default().starts_with("ee_"))
            .filter_map(|o| prop(o, "application.name").or_else(|| prop(o, "node.name")))
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// True when EasyEffects' virtual sink exists.
    #[must_use]
    pub fn easyeffects_running(&self) -> bool {
        self.nodes()
            .any(|o| prop(o, "node.name") == Some("easyeffects_sink"))
    }

    /// The hardware sink EasyEffects' output is currently linked to.
    #[must_use]
    pub fn easyeffects_output(&self) -> Option<String> {
        let sinks = self.hardware_sinks();
        self.links().find_map(|(from, to)| {
            let from = self.name_of(from)?;
            let to = self.name_of(to)?;
            (from.starts_with("ee_") && sinks.iter().any(|s| s.name == to)).then(|| to.to_owned())
        })
    }

    /// The default sink from the `default` metadata.
    #[must_use]
    pub fn default_sink(&self) -> Option<String> {
        self.0
            .iter()
            .filter(|o| {
                o.pointer("/props/metadata.name").and_then(Value::as_str) == Some("default")
            })
            .flat_map(|o| {
                o.get("metadata")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .find(|m| m.get("key").and_then(Value::as_str) == Some("default.audio.sink"))
            .and_then(|m| m.pointer("/value/name"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    fn links(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.0
            .iter()
            .filter(|o| o.get("type").and_then(Value::as_str) == Some("PipeWire:Interface:Link"))
            .filter_map(|o| {
                Some((
                    o.pointer("/info/output-node-id")?.as_u64()?,
                    o.pointer("/info/input-node-id")?.as_u64()?,
                ))
            })
    }

    /// Names of nodes that `node_name`'s outputs are linked to.
    #[must_use]
    pub fn links_from(&self, node_name: &str) -> Vec<String> {
        let ids: Vec<u64> = self
            .nodes()
            .filter(|o| prop(o, "node.name") == Some(node_name))
            .filter_map(|o| o.get("id").and_then(Value::as_u64))
            .collect();
        let mut out: Vec<String> = self
            .links()
            .filter(|(from, _)| ids.contains(from))
            .filter_map(|(_, to)| self.name_of(to).map(str::to_owned))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// Everything preflight needs, except the preferred sink and the EQ host.
    #[must_use]
    pub fn devices(&self) -> Devices {
        Devices {
            mic: self.mic(),
            sinks: self.hardware_sinks(),
            preferred_sink: None,
            other_streams: self.other_streams(),
            eq_host: None,
        }
    }
}

/// Reads a node's volume and mute state with `wpctl get-volume`.
///
/// # Errors
///
/// Fails if wpctl fails or prints something unexpected.
pub fn volume(id: &str) -> Result<(f64, bool)> {
    let out = run("wpctl", &["get-volume", id])?;
    parse_volume(&out).with_context(|| format!("unexpected wpctl output: {out:?}"))
}

fn parse_volume(out: &str) -> Option<(f64, bool)> {
    let rest = out.trim().strip_prefix("Volume:")?.trim();
    let num = rest.split_whitespace().next()?.parse().ok()?;
    Some((num, rest.contains("[MUTED]")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DUMP: &str = include_str!("../../../tests/data/pw-dump.json");

    #[test]
    fn finds_devices_in_real_dump() {
        let d = Dump::parse(DUMP).expect("valid dump");
        let mic = d.mic().expect("UMIK-1 present");
        assert_eq!(
            mic.name,
            "alsa_input.usb-miniDSP_Ltd._UMIK-1_1-00.mono-fallback"
        );
        assert_eq!(d.alsa_card(&mic.name).as_deref(), Some("3"));
        let sinks = d.hardware_sinks();
        assert!(
            sinks
                .iter()
                .any(|s| s.name == "alsa_output.pci-0000_c1_00.1.HiFi__HDMI4__sink")
        );
        assert!(sinks.iter().all(|s| s.name != "easyeffects_sink"));
        assert!(sinks.iter().all(|s| s.stereo));
        assert!(!mic.stereo);
        assert!(d.easyeffects_running());
        assert_eq!(
            d.easyeffects_output().as_deref(),
            Some("alsa_output.pci-0000_c1_00.1.HiFi__HDMI4__sink")
        );
        assert_eq!(d.default_sink().as_deref(), Some("easyeffects_sink"));
        assert!(d.other_streams().contains(&"Spotify".to_owned()));
        assert_eq!(d.links_from("spotify"), vec!["easyeffects_sink".to_owned()]);
    }

    #[test]
    fn parses_wpctl_volume() {
        assert_eq!(parse_volume("Volume: 1.00\n"), Some((1.0, false)));
        assert_eq!(parse_volume("Volume: 0.40 [MUTED]\n"), Some((0.4, true)));
        assert_eq!(parse_volume("nonsense"), None);
    }

    #[test]
    fn finds_the_route_easyeffects_autoloads_on() {
        let d = Dump::parse(DUMP).expect("valid dump");
        assert_eq!(
            d.route_description("alsa_output.pci-0000_c1_00.1.HiFi__HDMI4__sink")
                .as_deref(),
            Some("HDMI / DisplayPort 4 Output")
        );
        assert_eq!(d.route_description("easyeffects_sink"), None);
    }

    #[test]
    fn looks_up_a_named_node_and_returns_none_when_missing() {
        let d = Dump::parse(DUMP).expect("valid dump");
        let found = d
            .node_by_name("alsa_input.usb-miniDSP_Ltd._UMIK-1_1-00.mono-fallback")
            .expect("node exists in dump");
        assert_ne!(found.id, "0");
        assert_eq!(
            found.name,
            "alsa_input.usb-miniDSP_Ltd._UMIK-1_1-00.mono-fallback"
        );
        assert!(d.node_by_name("no_such_pipewire_node").is_none());
    }
}
