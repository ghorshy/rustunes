//! The stored form of a [`Source`], which a cache keeps between runs: every
//! field, those the plan's JSON leaves out included, under a version.
//! Reading one back writes it again and requires the very same bytes, so a
//! field this build would lose or read differently refuses the row. A row
//! names the media readers that made it (by their source, `build.rs`); one
//! made by other readers is refused, so a changed reader reads again.
use super::Source;
use crate::media::{fingerprint::EncodedAudio, format::Format};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};

/// Changes whenever a stored field changes meaning.
const VERSION: u32 = 1;
/// This build's media readers.
const READERS: &str = env!("MEDIA_READERS_ID");

#[derive(Serialize)]
struct Writing<'a> {
    version: u32,
    readers: &'a str,
    source: &'a Source,
    /// Each picture's `declared_type`, in order.
    declared_types: Vec<bool>,
    /// The stream's `encoded` of a format other than MPEG-1 Layer III.
    encoded: Option<&'a EncodedAudio>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reading {
    version: u32,
    readers: String,
    source: Source,
    declared_types: Vec<bool>,
    encoded: Option<EncodedAudio>,
}

impl Source {
    /// The bytes a cache keeps for this source ([`Source::from_stored`]).
    pub fn to_stored(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&Writing {
            version: VERSION,
            readers: READERS,
            source: self,
            declared_types: self.artwork.iter().map(|p| p.declared_type).collect(),
            encoded: self.stream.as_ref().map(|s| &s.encoded),
        })?)
    }

    /// The source [`Source::to_stored`] wrote as `bytes`, refused when they
    /// are of another version or would not be written the same again.
    pub fn from_stored(bytes: &[u8]) -> Result<Self> {
        let reading: Reading = serde_json::from_slice(bytes).context("reading a stored source")?;
        ensure!(
            reading.version == VERSION,
            "stored source of another version"
        );
        ensure!(
            reading.readers == READERS,
            "stored source read by other media readers"
        );
        let mut source = reading.source;
        ensure!(
            reading.declared_types.len() == source.artwork.len()
                && reading.encoded.is_some() == source.stream.is_some(),
            "stored source fields do not fit together"
        );
        for (picture, declared) in source.artwork.iter_mut().zip(reading.declared_types) {
            picture.declared_type = declared;
        }
        if let (Some(stream), Some(encoded)) = (source.stream.as_mut(), reading.encoded) {
            stream.encoded = encoded;
        }
        ensure!(
            source.to_stored()? == bytes,
            "stored source does not read back as written"
        );
        Ok(source)
    }
}

/// A placeholder until [`Source::from_stored`] sets the stored one.
pub(crate) fn no_encoded_audio() -> EncodedAudio {
    EncodedAudio {
        format: Format::Flac,
        sample_rate: 0,
        channels: 0,
        bits_per_sample: 0,
        packets: 0,
        coded_samples: 0,
        audio_offset: 0,
        audio_bytes: 0,
        config_sha256: [0; 32],
        audio_sha256: [0; 32],
    }
}

/// `text` as the one of `known` it equals.
fn one_of<'de, D: Deserializer<'de>>(
    deserializer: D,
    known: impl IntoIterator<Item = &'static str>,
) -> Result<&'static str, D::Error> {
    let text = String::deserialize(deserializer)?;
    known
        .into_iter()
        .find(|k| *k == text)
        .ok_or_else(|| serde::de::Error::custom(format!("unknown stored name {text:?}")))
}

pub(super) fn codec<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    one_of(d, crate::media::format::ALL.map(Format::name))
}

pub(super) fn audio_format<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    one_of(d, [super::ENCODED_MPEG1_LAYER3, super::ENCODED_AUDIO])
}

pub(super) fn mime<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    // What `image_facts` names: JPEG, PNG and the formats read to convert.
    let converted = crate::media::bitmap::Format::ALL.map(crate::media::bitmap::Format::mime);
    one_of(d, ["image/jpeg", "image/png"].into_iter().chain(converted))
}

pub(super) fn timing_schema<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    one_of(d, [super::timing::SCHEMA])
}

pub(super) fn encoder_family<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    one_of(d, super::timing::ENCODER_FAMILIES)
}

/// The values of `AppleTiming::source` that `gapless::analyse` sets.
pub(super) fn timing_source<'de, D: Deserializer<'de>>(d: D) -> Result<&'static str, D::Error> {
    one_of(d, ["itunsmpb", "lame_tag", "smpb_frame", "analysis"])
}
