use serde::{Deserialize, Serialize};

/// Sample rate in Hertz of the audio data sent in all [`RecognitionAudio`] messages.
///
/// Valid values are: 8000-48000. 16000 is optimal.
/// For best results, set the sampling rate of the audio source to 16000 Hz.
/// If that's not possible, use the native sample rate of the audio source (instead of re-sampling).
/// This field is optional for FLAC and WAV audio files, but is required for all other audio formats.
/// For details, see [`AudioEncoding`].
#[derive(Serialize)]
pub struct SampleRateHertz(u16);

impl SampleRateHertz {
    pub fn new(value: u16) -> Option<Self> {
        if 8000 <= value && value <= 48000 {
            Some(SampleRateHertz(value))
        } else {
            None
        }
    }
}

/// The number of channels in the input audio data.
///
/// ONLY set this for MULTI-CHANNEL recognition.
/// Valid values for LINEAR16 and FLAC are 1-8.
/// Valid values for OGG_OPUS are '1'-'254'.
/// Valid value for MULAW, AMR, AMR_WB and SPEEX_WITH_HEADER_BYTE is only 1.
/// If 0 or omitted, defaults to one channel (mono).
#[derive(Serialize)]
pub struct AudioChannelCount(u8);

impl AudioChannelCount {
    pub fn new(value: u8, encoding: AudioEncoding) -> Option<Self> {
        match encoding {
            AudioEncoding::Linear16 | AudioEncoding::Flac if 1 <= value && value <= 8 => {
                Some(AudioChannelCount(value))
            }
            AudioEncoding::OggOpus if 1 <= value && value <= 254 => Some(AudioChannelCount(value)),
            AudioEncoding::Mulaw
            | AudioEncoding::Amr
            | AudioEncoding::AmrWb
            | AudioEncoding::SpeexWithHeaderByte
                if value == 1 =>
            {
                Some(AudioChannelCount(value))
            }
            _ => None,
        }
    }
}

/// Maximum number of recognition hypotheses to be returned.
///
/// Specifically, the maximum number of [`SpeechRecognitionAlternative`] messages within each [`SpeechRecognitionResult`].
/// The server may return fewer than max_alternatives.
/// Valid values are 0-30. A value of 0 or 1 will return a maximum of one. If omitted, will return a maximum of one.
#[derive(Serialize)]
pub struct MaxAlternatives(u8);

impl MaxAlternatives {
    pub fn new(value: u8) -> Option<Self> {
        if value <= 30 {
            Some(MaxAlternatives(value))
        } else {
            None
        }
    }
}

/// Provides "hints" to the speech recognizer to favor specific words and phrases in the results.
#[derive(Serialize)]
pub struct SpeechContext {
    pub phrases: Vec<String>,
}

/// Encoding of audio data sent in all [`RecognitionAudio`] messages.
#[derive(Copy, Clone, Serialize)]
pub enum AudioEncoding {
    /// Not specified.
    Unspecified,

    /// Uncompressed 16-bit signed little-endian samples (Linear PCM).
    Linear16,

    /// FLAC (Free Lossless Audio Codec) is the recommended encoding because it is lossless--therefore recognition
    /// is not compromised--and requires only about half the bandwidth of LINEAR16.
    /// FLAC stream encoding supports 16-bit and 24-bit samples, however, not all fields in STREAMINFO are supported.
    Flac,

    /// 8-bit samples that compand 14-bit audio samples using G.711 PCMU/mu-law.
    Mulaw,

    /// Adaptive Multi-Rate Narrowband codec. sampleRateHertz must be 8000.
    Amr,

    /// Adaptive Multi-Rate Wideband codec. sampleRateHertz must be 16000.
    AmrWb,

    /// Opus encoded audio frames in Ogg container (OggOpus).
    /// sampleRateHertz must be one of 8000, 12000, 16000, 24000, or 48000.
    OggOpus,

    /// Although the use of lossy encodings is not recommended, if a very low bitrate encoding is required, OGG_OPUS is highly preferred over Speex encoding.
    /// The Speex encoding supported by Cloud Speech API has a header byte in each block, as in MIME type audio/x-speex-with-header-byte.
    /// It is a variant of the RTP Speex encoding defined in RFC 5574.
    /// The stream is a sequence of blocks, one block per RTP packet.
    /// Each block starts with a byte containing the length of the block, in bytes, followed by one or more frames of Speex data,
    /// padded to an integral number of bytes (octets) as specified in RFC 5574.
    /// In other words, each RTP header is replaced with a single byte containing the block length.
    /// Only Speex wideband is supported. sampleRateHertz must be 16000.
    SpeexWithHeaderByte,
}

/// Provides information to the recognizer that specifies how to process the request.
#[derive(Serialize)]
pub struct RecognitionConfig {
    /// See [`AudioEncoding`].
    pub encoding: AudioEncoding,

    /// See [`SampleRateHertz`].
    pub sample_rate_hertz: SampleRateHertz,

    /// See [`AudioChannelCount`].
    pub audio_channel_count: AudioChannelCount,

    /// This needs to be set to true explicitly and `audio_channel_count > 1` to get each channel recognized separately.
    /// The recognition result will contain a `channel_tag` field to state which channel that result belongs to.
    /// If this is not true, we will only recognize the first channel.
    /// The request is billed cumulatively for all channels recognized: `audio_channel_count` multiplied by the length of the audio.
    pub enable_separate_recognition_per_channel: bool,

    /// The language of the supplied audio as a BCP-47 language tag.
    ///
    /// Example: "en-US".
    pub language_code: String,

    /// See [`MaxAlternatives`].
    pub max_alternatives: MaxAlternatives,

    /// If set to true, the server will attempt to filter out profanities.
    pub profanity_filter: bool,

    /// See [`SpeechContext`].
    pub speech_contexts: Vec<SpeechContext>,
}

/// Contains audio data in the encoding specified in the [`RecognitionConfig`].
///
/// Note that only byte representation is supported.
#[derive(Serialize)]
pub struct RecognitionAudio<'b> {
    content: &'b [u8],
}

/// The top-level message sent by the client for the Recognize method.
#[derive(Serialize)]
pub struct RecognitionRequest<'b> {
    pub config: RecognitionConfig,
    pub audio: RecognitionAudio<'b>,
}

/// Provides information to the recognizer that specifies how to process the request.
#[derive(Serialize)]
pub struct StreamingRecognitionConfig {
    /// Provides information to the recognizer that specifies how to process the request.
    config: RecognitionConfig,

    /// If false or omitted, the recognizer will perform continuous recognition (continuing to wait for and process audio even if the user pauses speaking)
    /// until the client closes the input stream (gRPC API) or until the maximum time limit has been reached.
    /// May return multiple [`StreamingRecognitionResult`] with the `is_final` flag set to true.
    /// If true, the recognizer will detect a single spoken utterance.
    /// When it detects that the user has paused or stopped speaking, it will return an `END_OF_SINGLE_UTTERANCE` event and cease recognition.
    /// It will return no more than one [`StreamingRecognitionResult`] with the `is_final` flag set to true.
    single_utterance: bool,

    /// If true, interim results (tentative hypotheses) may be returned as they become available (these interim results are indicated with the `is_final=false` flag).
    /// If false or omitted, only `is_final=true` result(s) are returned.
    interim_results: bool
}

/// The top-level message sent by the client for the `StreamingRecognize` method.
///
/// Multiple [`StreamingRecognizeRequest`] messages are sent.
/// The first message must contain a `streaming_config` message and must not contain `audio_content`.
/// All subsequent messages must contain `audio_content` and must not contain a `streaming_config` message.
#[derive(Serialize)]
pub struct StreamingRecognizeRequest<'a> {
    /// Provides information to the recognizer that specifies how to process the request.
    ///
    /// The first [`StreamingRecognizeRequest`] message must contain a streaming_config message.
    streaming_config: StreamingRecognitionConfig,

    audio_content: &'a [u8]
}

/// The only message returned to the client by the Recognize method.
///
/// It contains the result as zero or more sequential [`SpeechRecognitionResult`] messages.
#[derive(Deserialize)]
pub struct RecognitionResponse {
    results: Vec<SpeechRecognitionResult>,
}

/// A speech recognition result corresponding to a portion of the audio.
#[derive(Deserialize)]
pub struct SpeechRecognitionResult {
    /// May contain one or more recognition hypotheses (up to the maximum specified in `max_alternatives`).
    ///
    /// These alternatives are ordered in terms of accuracy, with the top (first) alternative being the most probable, as ranked by the recognizer.
    alternatives: Vec<SpeechRecognitionAlternative>,

    /// For multi-channel audio, this is the channel number corresponding to the recognized result for the audio from that channel.
    ///
    /// For `audio_channel_count = N`, its output values can range from '1' to 'N'.
    channel_tag: u32,
}

/// Alternative hypotheses (a.k.a. n-best list).
#[derive(Deserialize)]
pub struct SpeechRecognitionAlternative {
    /// Transcript text representing the words that the user spoke.
    transcript: String,

    /// The confidence estimate between 0.0 and 1.0.
    ///
    /// A higher number indicates an estimated greater likelihood that the recognized words are correct.
    confidence: f32,

    /// A list of word-specific information for each recognized word.
    words: Vec<WordInfo>,
}

/// Word-specific information for recognized words.
#[derive(Deserialize)]
pub struct WordInfo {
    /// Time offset relative to the beginning of the audio, and corresponding to the start of the spoken word.
    ///
    /// This field is only set if `enable_word_time_offsets=true` and only in the top hypothesis.
    /// This is an experimental feature and the accuracy of the time offset can vary.
    start_time: Duration,

    /// Time offset relative to the beginning of the audio, and corresponding to the end of the spoken word.
    ///
    /// This field is only set if `enable_word_time_offsets=true` and only in the top hypothesis.
    /// This is an experimental feature and the accuracy of the time offset can vary.
    end_time: Duration,

    /// The word corresponding to this set of information.
    word: String,

    /// Output only. A distinct integer value is assigned for every speaker within the audio.
    ///
    /// This field specifies which one of those speakers was detected to have spoken this word.
    /// Value ranges from '1' to `diarization_speaker_count`.
    /// `speaker_tag` is set if `enable_speaker_diarization = 'true'` and only in the top alternative.
    speaker_tag: i32,
}

#[derive(Deserialize)]
pub struct Duration {
    /// Signed seconds of the span of time.
    ///
    /// Must be from -315,576,000,000 to +315,576,000,000 inclusive.
    seconds: i64,

    /// Signed fractions of a second at nanosecond resolution of the span of time.
    ///
    /// Durations less than one second are represented with a 0 seconds field and a positive or negative nanos field.
    /// For durations of one second or more, a non-zero value for the nanos field must be of the same sign as the seconds field.
    /// Must be from -999,999,999 to +999,999,999 inclusive.
    nanos: i32,
}

/// The Status type defines a logical error model that is suitable for different programming environments, including REST APIs and RPC APIs.
///
/// It is used by gRPC. Each Status message contains three pieces of data: error code, error message, and error details.
pub struct Status {
    /// The status code.
    code: i32,

    /// A developer-facing error message, which should be in English.
    message: String,

    /// A list of messages that carry the error details.
    details: Vec<String>
}