# EMS (External Media Server)

EMS is a service, that enriches your calls with:
* [x] Speech recognition
* [x] Speech synthesis
* [x] Audio playback using WebSocket API
* [x] Call termination using WebSocket API
* [x] Dynamic speech recognition service configuration

You can use it only in pair with some AudioSocket client, as EMS itself only receives extracted call audio and sends audio back for playback.

Currently, only one client has a support for AudioSocket protocol - [Asterisk](https://wiki.asterisk.org/wiki/display/AST/Home).

Relevant Asterisk documentation on configuration of external media and AudioSocket protocol can be found here:
* [AudioSocket protocol](https://wiki.asterisk.org/wiki/display/AST/AudioSocket)
* [External Media](https://wiki.asterisk.org/wiki/display/AST/External+Media+and+ARI)

While EMS can work fully standalone relying only on AudioSocket client, features like Text-to-Speech or Speech-to-Text require
additional connection from your application via WebSocket. With configured WebSocket stream EMS can send you speech transcriptions
and play text that you send via connection.

Currently, only Google Cloud services are supported - [Cloud Speech-to-Text](https://cloud.google.com/speech-to-text) and [Cloud Text-to-Speech](https://cloud.google.com/text-to-speech). If you have an implementation for similar services of other cloud providers - PRs are always appreciated.

## Usage
To start using EMS you have to configure a server via `./ems.toml` file (you can change path via `--config` flag):
```toml
audiosocket_addr = "0.0.0.0:12345"
websocket_addr = "0.0.0.0:12346"

recognition_driver = "google"
synthesis_driver = "google"

[gcs]
service_account_path = "./my_gcs_key.json"

[gctts]
service_account_path = "./my_gctts_key.json"
```

Then, launch EMS with `./ems`.

## Configuration
Config provided above uses recommended parameters for optional keys. All available keys and their default values are provided below:
| Name                       | Value           | Description                                                                                                                                                                 |
|----------------------------|-----------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| threads                    | All CPU threads | A max number of threads EMS can use in runtime                                                                                                                              |
| audiosocket_addr           |                 | Required. Address on which EMS should listen for incoming AudioSocket messages                                                                                              |
| websocket_addr             |                 | Required. Address on which EMS should listen for incoming WebSocket streams                                                                                                 |
| message_timeout            | 3               | Max amount of seconds that EMS can wait between new AudioSocket messages. If elapsed time is greater than provided value, then EMS will close AudioSocket connection        |
| recognition_config_timeout | 3               | Max amount of seconds that EMS can wait for speech recognition config. If elapsed time is greater than provided value, then EMS will use the config provided in `ems.toml`. |
| recognition_driver         |                 | Speech recognition driver that EMS can use for call transcription generation. Supported values: `google`                                                                    |
| synthesis_driver           |                 | Speech synthesis driver that EMS can use for call voice synthesis. Supported values: `google`                                                                               |
| gcs                        |                 | Google Cloud Text-to-Speech configuration                                                                                                                                   |
| gctts                      |                 | Google Cloud Speech-to-Text configuration                                                                                                                                   |
| recognition_fallback       |                 | Speech recognition fallback config. Used in case if WebSocket client missed opportunity to send recognition config. If empty, `en-US` config is used as a fallback.         |
| loopback_audio             | false           | Send all received audio back                                                                                                                                                |

## WebSocket API

Every WebSocket message requires you to provide UUID of call. Usually, you specify this UUID when registering external media server.

### Requests

Terminate call:
```json
{
    "id": "00000000-0000-0000-0000-000000000000",
    "data": "Hangup"
}
```

Synthesize speech:

`gender`, `speaking_rate` and `pitch` are optional.
```json
{
    "id": "00000000-0000-0000-0000-000000000000",
    "data": {
        "Synthesize": {
            "ssml": "Hello, world",
            "language_code": "en-US",
            "gender": "neutral",
            "speaking_rate": 2,
            "pitch": 1
        }
    }
}
```

Speech recognition config:

`profanity_filter` and `punctuation` are optional.
```json
{
    "id": "00000000-0000-0000-0000-000000000000",
    "data": {
        "RecognitionConfig": {
            "language": "en-US",
            "profanity_filter": false,
            "punctuation": false
        }
    }
}
```

### Responses

Call transcription:
```json
{
    "id": "00000000-0000-0000-0000-000000000000",
    "data": {
        "Transcription": "Hello, world"
    }
}
```

Recognition config request:
```json
{
    "id": "00000000-0000-0000-0000-000000000000",
    "data": "RecognitionConfigRequest"
}
```