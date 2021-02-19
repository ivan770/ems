use crate::{google::codegen::SsmlVoiceGender, synthesis::SynthesisVoiceGender};

/// GCTTS config
pub mod config;

/// Implementation of speech synthesis driver for Google Cloud Text-to-Speech.
pub mod driver;

impl From<SynthesisVoiceGender> for SsmlVoiceGender {
    fn from(voice: SynthesisVoiceGender) -> Self {
        match voice {
            SynthesisVoiceGender::Any => SsmlVoiceGender::Unspecified,
            SynthesisVoiceGender::Male => SsmlVoiceGender::Male,
            SynthesisVoiceGender::Female => SsmlVoiceGender::Female,
            SynthesisVoiceGender::Neutral => SsmlVoiceGender::Neutral,
        }
    }
}
